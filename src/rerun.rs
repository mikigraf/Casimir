//! Rerun orchestration: replay a recorded session's user turns against a harness/model,
//! capture the new session, diff the workspace, and compare against the original.
//! `rerun_matrix` runs replicates (and several simulator models) and aggregates them.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::adapters::{self, RunOpts};
use crate::brief::{draft_brief, intent_coverage, Brief};
use crate::compare::{compare_sessions, judge_sessions_with, render_compare_markdown, sequence_similarity, JudgeOpts, Report};
use crate::llm::{effective_model, LlmOpts};
use crate::model::{action_sequence, renumber_turns, stats, user_turns, Event, EventKind, Execution, Harness, RerunOf, Session, Simulated, SimulatorInfo};
use crate::render::{format_event, RenderOpts};
use crate::simulate::simulate_user_turn;
use crate::util::{casimir_home, colors, first_line, fmt_num, now_iso, now_stamp, pad, slug, write_json};
use crate::workspace::{base_commit, capture_diff, capture_diff_against, create_worktree, is_git_repo, reconstruct_original_diff, repo_root, Diff};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RerunOpts {
    pub checks: Option<PathBuf>,
    pub checkpoint_limit: u64,
    pub turn_timeout_secs: u64,
    pub allow_unrestricted: bool,
    pub harness: Option<Harness>,
    pub model: Option<String>,
    /// "verbatim" or "simulate"
    pub user_mode: String,
    /// "auto" | "worktree" | "same" | a directory
    pub workspace: String,
    pub turns: Option<usize>,
    pub permission_mode: Option<String>,
    pub sandbox: Option<String>,
    pub judge: bool,
    /// LLM that plays the user (independent of the judge and of the agent under test).
    pub sim_llm: LlmOpts,
    pub judge_llm: LlmOpts,
    pub out_dir: Option<PathBuf>,
    pub quiet: bool,
    pub thinking: bool,
    pub dry_run: bool,
    pub continue_on_error: bool,
    pub extra_args: Vec<String>,
    pub run_id: Option<String>,
    /// Number of replicate reruns per simulator model.
    pub replicates: usize,
    /// Simulator models to run separately (bounds simulator-induced variance). Empty = sim_llm.model.
    pub sim_models: Vec<String>,
    /// Judge score (0-10) at or above which a replicate counts as a pass.
    pub pass_threshold: f64,
    /// Workspace diff of the original session, if known (else reconstructed from git when possible).
    pub original_diff: Option<Diff>,
    /// Also rerun with the original harness and model as a control group (noise floor).
    pub control: bool,
    /// Fork the original session at this user turn (1-based, >= 2) instead of replaying from turn 1.
    pub from_turn: Option<u32>,
    /// Replacement for the forked turn's user message (an intervention); None = resample verbatim.
    pub intervention: Option<String>,
    /// Same-order judge repeats per ordering (>= 2 measures judge test-retest separately from agent variance).
    pub judge_repeats: usize,
    /// Per-session brief (rubric, session analysis, intents). None = draft one when needed.
    pub brief: Option<PathBuf>,
    /// Matrix runs freeze their brief and reference diff before executing any candidates.
    pub freeze_inputs: bool,
    /// Repository supplied by --workspace DIR, used to create independent matrix worktrees.
    pub workspace_repo: Option<PathBuf>,
}

impl Default for RerunOpts {
    fn default() -> Self {
        RerunOpts {
            checks: None,
            checkpoint_limit: crate::checkpoint::DEFAULT_LIMIT,
            turn_timeout_secs: 900,
            allow_unrestricted: false,
            harness: None,
            model: None,
            user_mode: "verbatim".into(),
            workspace: "auto".into(),
            turns: None,
            permission_mode: None,
            sandbox: None,
            judge: false,
            sim_llm: LlmOpts::default(),
            judge_llm: LlmOpts::default(),
            out_dir: None,
            quiet: false,
            thinking: false,
            dry_run: false,
            continue_on_error: false,
            extra_args: vec![],
            run_id: None,
            replicates: 1,
            sim_models: vec![],
            pass_threshold: 7.0,
            original_diff: None,
            control: false,
            from_turn: None,
            intervention: None,
            judge_repeats: 1,
            brief: None,
            freeze_inputs: false,
            workspace_repo: None,
        }
    }
}

/// Load the brief from `opts.brief`, or draft one (LLM) and store it in the run directory.
/// Needed by the judge (rubric), the simulator (session analysis) and intent coverage.
fn resolve_brief(original: &Session, o: &RerunOpts, diff_a: Option<&Diff>, run_dir: &Path, log: &mut dyn FnMut(&str)) -> Result<Option<Brief>> {
    let c = colors();
    if let Some(p) = &o.brief {
        let b = Brief::load(p)?;
        b.save(&run_dir.join("brief.json"))?;
        return Ok(Some(b));
    }
    let existing = run_dir.join("brief.json");
    if existing.exists() {
        return Ok(Some(Brief::load(&existing)?));
    }
    let needed = !o.freeze_inputs && (o.judge || o.user_mode == "simulate" || o.user_mode == "auto");
    if !needed {
        return Ok(None);
    }
    log(&format!("{}drafting a per-session brief (rubric, session analysis, intents) with {}…{}", c.magenta, effective_model(&o.judge_llm), c.reset));
    match draft_brief(original, diff_a, &o.judge_llm) {
        Ok(b) => {
            b.save(&existing)?;
            fs::write(run_dir.join("brief.md"), crate::brief::render_brief_markdown(&b))?;
            log(&format!("  brief saved to {} (edit it and pass --brief to reuse a human-reviewed version)", existing.display()));
            Ok(Some(b))
        }
        Err(err) => {
            log(&format!("{}could not draft a brief: {err}; the judge falls back to the generic prompt{}", c.yellow, c.reset));
            Ok(None)
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePlan {
    pub dir: PathBuf,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// For forks: how faithfully the workspace state before the forked turn could be restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored: Option<String>,
}

pub struct RerunOutcome {
    pub run_dir: PathBuf,
    pub session: Option<Session>,
    pub report: Option<Report>,
    pub diff: Option<Diff>,
    pub workspace: WorkspacePlan,
    pub dry_run: bool,
}

/// Decide where the rerun will execute.
pub fn plan_workspace(original: &Session, workspace: &str, run_id: &str) -> Result<WorkspacePlan> {
    plan_workspace_at(original, workspace, run_id, None)
}

/// Like `plan_workspace`, but for a fork at `from_turn`: the worktree is checked out at the last
/// commit before that turn's user message, and the plan notes whether earlier edits are covered.
pub fn plan_workspace_at(original: &Session, workspace: &str, run_id: &str, from_turn: Option<u32>) -> Result<WorkspacePlan> {
    let plain = |dir: PathBuf, mode: &str| WorkspacePlan { dir, mode: mode.into(), root: None, commit: None, how: None, repo: None, note: None, restored: None };
    if !matches!(workspace, "auto" | "worktree" | "same") {
        let dir = fs::canonicalize(workspace).with_context(|| format!("workspace directory does not exist: {workspace}"))?;
        if !is_git_repo(&dir) { bail!("runs require a Git repository for reliable checkpoints"); }
        let repo = repo_root(&dir)?;
        let root = casimir_home().join("worktrees").join(run_id);
        let relative = dir.strip_prefix(&repo).unwrap_or(Path::new(""));
        return Ok(WorkspacePlan { dir: root.join(relative), mode: "worktree".into(), root: Some(root), commit: Some(crate::workspace::head_commit(&repo)?), how: Some("explicit repository HEAD".into()), repo: Some(repo), note: None, restored: None });
    }
    let cwd = original.cwd.as_deref().map(Path::new);
    let Some(cwd) = cwd.filter(|p| p.exists()) else {
        bail!("original cwd is unavailable; pass --workspace <repository>");
    };
    let git = is_git_repo(cwd);
    if !git { bail!("runs require a Git repository for reliable checkpoints"); }
    if workspace == "same" || (workspace == "auto" && !git) {
        let mut p = plain(cwd.to_path_buf(), "same");
        if !git {
            p.note = Some("original cwd is not a git repo; running in place (no diff capture)".into());
        }
        return Ok(p);
    }
    let repo = repo_root(cwd)?;
    let (mut commit, mut how) = base_commit(original, cwd)?;
    let mut restored = None;
    if let Some(n) = from_turn {
        let id = original.checkpoints.get(&n).context("fork requires a compatible checkpoint; imported historical sessions support inspection and full reruns only")?;
        let checkpoint = crate::checkpoint::load(id)?;
        crate::checkpoint::require_compatible(&checkpoint)?;
        commit = checkpoint.base;
        how = "verified checkpoint".into();
        restored = Some("recorded repository workspace and conversation; external state excluded".into());
    }
    let dest = casimir_home().join("worktrees").join(run_id);
    let rel = cwd.strip_prefix(&repo).unwrap_or(Path::new(""));
    let dir = if rel.as_os_str().is_empty() { dest.clone() } else { dest.join(rel) };
    Ok(WorkspacePlan { dir, mode: "worktree".into(), root: Some(dest), commit: Some(commit), how: Some(how), repo: Some(repo), note: None, restored })
}

pub fn make_run_id(original: &Session, harness: Harness, model: Option<&str>) -> String {
    let id: String = original.id.chars().take(8).collect();
    format!("{}-{}-{}-{}-{}", now_stamp(), slug(harness.as_str()), slug(model.unwrap_or("default")), id, &uuid::Uuid::new_v4().to_string()[..8])
}

/// Merge the harness's own log for the rerun with our live capture: our user events carry
/// simulation metadata, everything else comes from the log, plus live errors the log lacks.
fn merge_with_harness_log(session: &mut Session, full: Session) {
    let ours: Vec<Event> = session.events.iter().filter(|e| e.kind == EventKind::User && !e.sidechain).cloned().collect();
    let source_turns: std::collections::HashMap<u32, u32> = full.events.iter()
        .filter(|e| e.kind == EventKind::User && !e.sidechain).zip(&ours)
        .map(|(logged, ours)| (logged.turn, ours.source_turn.unwrap_or(ours.turn))).collect();
    let mut merged: Vec<Event> = Vec::new();
    let mut ui = 0;
    for e in &full.events {
        if e.kind == EventKind::User && !e.sidechain {
            merged.push(ours.get(ui).cloned().unwrap_or_else(|| e.clone()));
            ui += 1;
        } else {
            let mut e = e.clone();
            e.source_turn = source_turns.get(&e.turn).copied();
            merged.push(e);
        }
    }
    let known: Vec<String> = full.events.iter().filter(|e| e.kind == EventKind::Error).map(|e| e.text_str().to_string()).collect();
    for e in &session.events {
        if (e.kind == EventKind::Error && !known.iter().any(|k| k == e.text_str()))
            || (e.kind == EventKind::System && e.subtype.as_deref().is_some_and(|s| s == "fork" || s.starts_with("simulator-"))) {
            merged.push(e.clone());
        }
    }
    renumber_turns(&mut merged);
    session.events = merged;
    if session.model.is_none() {
        session.model = full.model;
    }
    if full.usage_total.is_some() {
        session.usage_total = full.usage_total;
    }
    session.harness_log_path = full.path;
    session.version = full.version.or(session.version.clone());
}

/// Original-session diff: the caller's, else reconstructed from git history when the cwd is a repo.
fn original_diff_for(original: &Session, o: &RerunOpts) -> Option<Diff> {
    o.original_diff.clone().or_else(|| if o.freeze_inputs { None } else { reconstruct_original_diff(original) })
}

fn validate_run(original: &Session, o: &RerunOpts) -> Result<()> {
    let n = user_turns(original).len();
    if n == 0 { bail!("original session has no user turns to replay"); }
    if o.replicates == 0 || o.judge_repeats == 0 { bail!("replicates and judge repeats must be at least 1"); }
    if !o.pass_threshold.is_finite() || !(0.0..=10.0).contains(&o.pass_threshold) { bail!("pass threshold must be a finite score from 0 to 10"); }
    if o.turn_timeout_secs == 0 || o.judge_llm.timeout_secs == 0 || o.sim_llm.timeout_secs == 0 { bail!("timeouts must be positive"); }
    if o.turns == Some(0) { bail!("turns must be at least 1"); }
    if let Some(t) = o.from_turn {
        if t < 2 || t as usize > o.turns.unwrap_or(n).min(n) { bail!("fork turn must be between 2 and the last requested turn"); }
        if o.harness.unwrap_or(original.harness()) != original.harness() { bail!("fork-at-turn requires the same harness as the original"); }
        if matches!(original.harness(), Harness::Copilot | Harness::Gemini) { bail!("fork-at-turn is not supported for {}", original.harness()); }
    }
    Ok(())
}

pub fn rerun(original: &Session, o: &RerunOpts, log: &mut dyn FnMut(&str), out: &mut dyn FnMut(&str)) -> Result<RerunOutcome> {
    rerun_inner(original, o, None, log, out)
}
pub(crate) fn rerun_recovered(original: &Session, o: &RerunOpts, recovery: crate::recovery::Recovery, log: &mut dyn FnMut(&str), out: &mut dyn FnMut(&str)) -> Result<RerunOutcome> {
    rerun_inner(original, o, Some(recovery), log, out)
}
fn rerun_inner(original: &Session, o: &RerunOpts, recovery: Option<crate::recovery::Recovery>, log: &mut dyn FnMut(&str), out: &mut dyn FnMut(&str)) -> Result<RerunOutcome> {
    validate_run(original, o)?;
    adapters::validate_permissions(&RunOpts { permission_mode: o.permission_mode.clone(), sandbox: o.sandbox.clone(), extra_args: o.extra_args.clone(), allow_unrestricted: o.allow_unrestricted, ..Default::default() })?;
    let c = colors();
    let harness = o.harness.unwrap_or(original.harness());
    let same_harness = harness == original.harness();
    let model = o.model.clone().or_else(|| if same_harness { original.model.clone() } else { None });
    let run_id = o.run_id.clone().unwrap_or_else(|| make_run_id(original, harness, model.as_deref()));
    let turns = user_turns(original);
    if turns.is_empty() {
        bail!("original session has no user turns to replay");
    }
    let from_turn = recovery.as_ref().map(|r| r.next_turn).unwrap_or_else(|| o.from_turn.unwrap_or(1).max(1));
    if from_turn > turns.len() as u32 && recovery.is_none() {
        bail!("cannot fork at turn {from_turn}: the session has {} user turns", turns.len());
    }
    if from_turn > 1 && !same_harness && recovery.is_none() {
        bail!("fork-at-turn requires the same harness as the original ({}); the forked transcript is resumed natively", original.harness());
    }
    let mut workspace_original = original.clone();
    if let Some(repo) = &o.workspace_repo { workspace_original.cwd = Some(repo.display().to_string()); }
    let restore_id = recovery.as_ref().and_then(|r| r.checkpoint.clone()).or_else(|| o.from_turn.and_then(|t| original.checkpoints.get(&t).cloned()));
    if o.from_turn.is_some() && restore_id.is_none() { bail!("fork requires a compatible checkpoint; timestamps and inferred commits are insufficient"); }
    let checkpoint = restore_id.as_deref().map(crate::checkpoint::load).transpose()?;
    if let Some(cp) = &checkpoint { if from_turn as usize <= o.turns.unwrap_or(turns.len()).min(turns.len()) { crate::checkpoint::require_compatible(cp)?; } }
    let ws = if let Some(cp) = &checkpoint {
        let root = casimir_home().join("worktrees").join(&run_id);
        WorkspacePlan { dir: root.join(&cp.subdir), mode: "worktree".into(), root: Some(root), commit: Some(cp.base.clone()), how: Some("verified checkpoint".into()), repo: Some(crate::checkpoint::repository_path(cp)), note: Some(cp.coverage.clone()), restored: Some("checkpoint".into()) }
    } else { plan_workspace_at(&workspace_original, &o.workspace, &run_id, None)? };
    let max_turns = o.turns.map(|n| n.min(turns.len())).unwrap_or(turns.len());
    let run_dir = o.out_dir.clone().unwrap_or_else(|| casimir_home().join("runs").join(&run_id));
    let mut effective_options = o.clone();
    effective_options.sim_llm.recording_dir = Some(run_dir.join("llm"));
    effective_options.judge_llm.recording_dir = Some(run_dir.join("llm"));
    let o = &effective_options;
    if run_dir.join("meta.json").exists() { bail!("run directory already contains a run: {}; choose a fresh output directory", run_dir.display()); }
    let simulate = o.user_mode == "simulate" || o.user_mode == "auto";
    let sim_model = effective_model(&o.sim_llm);

    log(&format!("{}rerun{} {}:{} → {}{}", c.bold, c.reset, original.harness(), original.id, harness, model.as_ref().map(|m| format!(" ({m})")).unwrap_or_else(|| " (harness default model)".into())));
    log(&format!("  turns: {max_turns}/{}  user mode: {}{}", turns.len(), o.user_mode, if simulate { format!(" (simulator: {sim_model} via {})", crate::llm::pick_backend(&o.sim_llm.backend)) } else { String::new() }));
    let ws_detail = ws.commit.as_ref().map(|cm| format!(", {} — {}", &cm[..cm.len().min(10)], ws.how.as_deref().unwrap_or(""))).unwrap_or_default();
    log(&format!("  workspace: {} [{}{}]", ws.dir.display(), ws.mode, ws_detail));
    if let Some(n) = &ws.note {
        log(&format!("  {}note: {n}{}", c.yellow, c.reset));
    }
    if let Some(r) = &ws.restored {
        log(&format!("  workspace before turn {from_turn}: {r}"));
    }
    if from_turn > 1 {
        log(&format!("  fork: turns 1..{} preserved from the original, turn {from_turn} {}", from_turn - 1, if o.intervention.is_some() { "replaced by the intervention message" } else { "resampled verbatim" }));
    }
    log(&format!("  preflight: at most {} harness turns, {} ordered judge calls; turn timeout {}s; LLM timeout {}s; costs measured when available, otherwise unknown", max_turns.saturating_sub(from_turn as usize - 1), if o.judge { 2 * o.judge_repeats } else { 0 }, o.turn_timeout_secs, o.judge_llm.timeout_secs));
    log(&format!("  output: {}", run_dir.display()));
    if o.dry_run {
        for t in turns.iter().take(max_turns).filter(|t| t.turn >= from_turn) {
            let text = if t.turn == from_turn && from_turn > 1 { o.intervention.as_deref().unwrap_or(&t.text) } else { &t.text };
            log(&format!("  turn {}: {}{}", t.turn, first_line(text).chars().take(100).collect::<String>(), if t.turn == from_turn && o.intervention.is_some() { "  (intervention)" } else { "" }));
        }
        return Ok(RerunOutcome { run_dir, session: None, report: None, diff: None, workspace: ws, dry_run: true });
    }

    let _lock = crate::util::RunLock::acquire(&run_dir)?;
    if run_dir.join("meta.json").exists() { bail!("run directory already contains a run"); }
    for entry in fs::read_dir(&run_dir)? {
        let name = entry?.file_name();
        if name != ".lock" && !(recovery.is_some() && name == "recovery.json") { bail!("output directory must be empty to establish artifact ownership"); }
    }
    if let Some(id) = &restore_id {
        crate::checkpoint::restore_for_run(id, ws.root.as_ref().unwrap(), &run_dir)?;
    } else if ws.mode == "worktree" {
        create_worktree(ws.repo.as_ref().unwrap(), ws.commit.as_ref().unwrap(), ws.root.as_ref().unwrap())?;
        log(&format!("  created worktree {}", ws.root.as_ref().unwrap().display()));
    }
    crate::artifacts::register(&run_dir, &ws)?;
    // Freeze the base before the agent can commit. For forks include the preserved prefix's edits.
    let diff_base = if is_git_repo(&ws.dir) { Some(base_commit(&workspace_original, &ws.dir)?.0) } else { None };
    fs::create_dir_all(&run_dir)?;
    write_json(&run_dir.join("original.json"), original)?;
    let frozen_checks = o.checks.as_ref().map(|p| crate::checks::freeze(p, &run_dir, &ws.dir)).transpose()?;

    let mut session = recovery.as_ref().map(|r| r.session.clone()).unwrap_or_else(|| Session::new(harness));
    session.model = model.clone();
    session.version = crate::doctor::harness_version(harness);
    session.cwd = Some(ws.dir.display().to_string());
    session.title = original.title.clone();
    session.started_at = Some(now_iso());
    session.git_commit = diff_base.clone();
    if recovery.is_none() { session.execution = Some(Execution { requested_turns: max_turns, preserved_turns: (from_turn - 1) as usize, ..Default::default() }); }
    session.rerun_of = Some(RerunOf { harness: original.harness(), id: original.id.clone(), path: original.path.clone() });
    session.workspace = Some(serde_json::to_value(&ws)?);
    if simulate {
        session.simulator = Some(SimulatorInfo { model: sim_model.clone(), backend: crate::llm::pick_backend(&o.sim_llm.backend) });
    }
    let mut meta = json!({
        "schemaVersion": 1, "state": "running", "runId": run_id, "original": session.rerun_of, "harness": harness, "model": model,
        "userMode": o.user_mode, "simulator": session.simulator,
        "judgeModel": if o.judge { Some(effective_model(&o.judge_llm)) } else { None },
        "workspace": ws, "startedAt": session.started_at, "requestedTurns": max_turns,
    });
    write_json(&run_dir.join("meta.json"), &meta)?;
    let mut raw = fs::OpenOptions::new().create(true).append(true).open(run_dir.join("raw.jsonl"))?;
    let start = crate::util::ts_ms(session.started_at.as_deref().unwrap_or(""));
    let permission_mode = o.permission_mode.clone().unwrap_or_else(|| "preserve".into());
    let sandbox = o.sandbox.clone().unwrap_or_else(|| "preserve".into());
    let render = RenderOpts { thinking: o.thinking, max_lines: 6, ..Default::default() };
    let session_path = run_dir.join("session.json");

    let mut harness_session_id: Option<String> = None;
    let mut cost = session.cost_usd.unwrap_or(0.0);
    let mut saw_cost = session.cost_usd.is_some();
    let mut sim_state: crate::simulate::SimState = recovery.as_ref().map(|r| r.simulator.clone()).unwrap_or_default();
    let configuration_hash = crate::checkpoint::hash(&serde_json::to_vec(&json!({"harness":harness,"permissionMode":permission_mode,"sandbox":sandbox,"extraArgs":o.extra_args,"checksHash":frozen_checks.as_ref().map(|(_, hash)| hash)}))?);
    if recovery.is_some() && checkpoint.as_ref().is_some_and(|cp| cp.configuration_hash != configuration_hash) { bail!("recovery configuration differs from its checkpoint"); }
    let diff_a = original_diff_for(original, o);
    let brief = resolve_brief(original, o, diff_a.as_ref(), &run_dir, log)?;
    if let Some(b) = &brief {
        sim_state.analysis = Some(b.analysis_text());
        meta["brief"] = json!({ "path": run_dir.join("brief.json"), "humanReviewed": b.human_reviewed, "draftedBy": b.drafted_by });
        write_json(&run_dir.join("meta.json"), &meta)?;
    }

    if from_turn > 1 && from_turn as usize <= max_turns {
        // Harness-native fork: the transcript before the forked turn is preserved verbatim and the
        // harness resumes it (in-situ intervention, after arXiv 2512.06749).
        let new_id = uuid::Uuid::new_v4().to_string();
        let cp = checkpoint.as_ref().context("native continuation requires a verified checkpoint")?;
        let transcript = adapters::restore_conversation(cp, &new_id, &ws.dir)?;
        log(&format!("  forked transcript: {}", transcript.display()));
        harness_session_id = Some(new_id);
        if recovery.is_none() {
            for e in original.events.iter().filter(|e| e.turn < from_turn) { session.events.push(e.clone()); }
        }
        session.events.push(Event::system(from_turn, now_iso(), "fork", format!("continued from verified checkpoint before turn {from_turn}")));
        meta["forkedAtTurn"] = json!(from_turn);
        meta["intervention"] = json!(o.intervention);
        meta["forkedTranscript"] = json!(transcript);
        write_json(&run_dir.join("meta.json"), &meta)?;
    }

    session.configuration_hash = Some(configuration_hash.clone());
    let mut saved_options = o.clone();
    if frozen_checks.is_some() { saved_options.checks = Some(run_dir.join("checks.definition.json")); }
    if brief.is_some() { saved_options.brief = Some(run_dir.join("brief.json")); }
    let mut journal = crate::recovery::Recovery { schema_version: 1, state: "running".into(), options: saved_options, original: original.clone(), session: session.clone(), simulator: sim_state.clone(), next_turn: from_turn, checkpoint: restore_id.clone(), active: false, source: run_dir.clone(), resumed_as: None };
    journal.save(&run_dir)?;
    for (i, t) in turns.iter().take(max_turns).enumerate() {
        if t.turn < from_turn {
            continue;
        }
        let mut message = t.text.clone();
        let mut simulated: Option<Simulated> = None;
        if t.turn == from_turn && recovery.as_ref().is_some_and(|r| r.active) {
            message = checkpoint.as_ref().unwrap().pending_prompt.clone();
        } else if t.turn == from_turn && from_turn > 1 && recovery.is_none() {
            if let Some(m) = &o.intervention {
                message = m.clone();
                simulated = Some(Simulated { verbatim: false, reason: "intervention: user message replaced at the fork".into(), grounded_in: vec![from_turn], action: Some("intervention".into()) });
            }
        } else if i > 0 && simulate {
            log(&format!("{}simulating user for turn {}…{}", c.magenta, t.turn, c.reset));
            let sim = match simulate_user_turn(original, &session, t.turn, &o.sim_llm, &mut sim_state) {
                Ok(sim) => sim,
                Err(err) => {
                    let message = format!("user simulator failed at turn {}: {err}", t.turn);
                    log(&message);
                    session.events.push(Event::text(t.turn, now_iso(), EventKind::Error, message));
                    let execution = session.execution.as_mut().unwrap();
                    execution.failed_turns += 1;
                    execution.stop_reason = Some("simulator_error".into());
                    break;
                }
            };
            match sim.message {
                None if sim.no_op => {
                    log(&format!("{}simulator: no-op at turn {} ({}){}", c.magenta, t.turn, sim.reason, c.reset));
                    session.events.push(Event::system(t.turn, now_iso(), "simulator-noop", sim.reason));
                    session.execution.as_mut().unwrap().skipped_turns += 1;
                    journal.session = session.clone(); journal.simulator = sim_state.clone(); journal.next_turn = t.turn + 1; journal.save(&run_dir)?;
                    continue;
                }
                None => {
                    let reason = sim.stop_reason.clone().unwrap_or_else(|| "goals_met".into());
                    log(&format!("{}simulator stopped the session ({reason}): {}{}", c.magenta, sim.reason, c.reset));
                    session.events.push(Event::system(t.turn, now_iso(), &format!("simulator-stop:{reason}"), sim.reason));
                    session.execution.as_mut().unwrap().stop_reason = Some(reason);
                    break;
                }
                Some(m) => {
                    if !sim.verbatim {
                        log(&format!("{}adapted message [{}] (grounded in turns {:?}): {}{}", c.magenta, sim.kind.as_deref().unwrap_or("answer"), sim.grounded_in, first_line(&m).chars().take(120).collect::<String>(), c.reset));
                    }
                    if sim.retries > 0 {
                        log(&format!("{}simulator needed {} retr{} to ground its message{}", c.yellow, sim.retries, if sim.retries == 1 { "y" } else { "ies" }, c.reset));
                    }
                    simulated = Some(Simulated { verbatim: sim.verbatim, reason: sim.reason, grounded_in: sim.grounded_in, action: sim.kind });
                    message = m;
                }
            }
        }
        let native = harness_session_id.as_ref().and_then(|id| adapters::find_log_by_id(harness, id));
        if is_git_repo(&ws.dir) {
            let cp = crate::checkpoint::capture(crate::checkpoint::Capture { cwd: &ws.dir, run_dir: &run_dir, native: native.as_deref(), harness, version: session.version.clone(), turn: t.turn, expected_conversation_turns: user_turns(&session).len() as u32, prompt: &message, configuration_hash: &configuration_hash, limit: o.checkpoint_limit })?;
            session.checkpoints.insert(t.turn, cp.clone());
            journal.checkpoint = Some(cp);
        }
        journal.session = session.clone(); journal.simulator = sim_state.clone(); journal.next_turn = t.turn; journal.active = true;
        journal.save(&run_dir)?;
        meta["activeTurn"] = json!(t.turn); write_json(&run_dir.join("meta.json"), &meta)?;
        let mut user_ev = Event::text(t.turn, now_iso(), EventKind::User, message.clone());
        user_ev.source_turn = Some(t.turn);
        user_ev.simulated = simulated;
        if !o.quiet {
            if let Some(s) = format_event(&user_ev, &render, start) {
                out(&s);
            }
        }
        session.events.push(user_ev);

        let opts = RunOpts {
            timeout_secs: Some(o.turn_timeout_secs),
            allow_unrestricted: o.allow_unrestricted,
            spool: Some(run_dir.join("turns").join(t.turn.to_string())),
            prompt: message,
            cwd: Some(ws.dir.clone()),
            model: model.clone(),
            session_id: if harness_session_id.is_none() { Some(uuid::Uuid::new_v4().to_string()) } else { None },
            resume: harness_session_id.clone(),
            permission_mode: Some(permission_mode.clone()),
            sandbox: Some(sandbox.clone()),
            extra_args: o.extra_args.clone(),
            turn: t.turn,
            bin: None,
        };
        let mut live: Vec<Event> = Vec::new();
        let result = adapters::run_turn(harness, &opts, &mut |ev: &Event| {
            live.push(ev.clone());
            if !o.quiet {
                if let Some(s) = format_event(ev, &render, start) {
                    out(&s);
                }
            }
        });
        let mut res = match result {
            Ok(res) => res,
            Err(err) => {
                live.push(Event::text(t.turn, now_iso(), EventKind::Error, format!("harness execution failed: {err}")));
                adapters::RunResult { is_error: true, ..Default::default() }
            }
        };
        res.is_error |= live.iter().any(|e| e.kind == EventKind::Error);
        if res.is_error {
            session.execution.as_mut().unwrap().failed_turns += 1;
            if !live.iter().any(|e| e.kind == EventKind::Error) {
                live.push(Event::text(t.turn, now_iso(), EventKind::Error, "harness reported an unsuccessful turn"));
            }
        } else {
            session.execution.as_mut().unwrap().completed_turns += 1;
        }
        if serde_json::to_vec(&session.events)?.len().saturating_add(serde_json::to_vec(&live)?.len()) > 256 * 1024 * 1024 { bail!("run exceeds 256 MiB normalization limit; raw streams and recovery checkpoint retained"); }
        session.events.extend(live);
        for r in &res.raw {
            writeln!(raw, "{r}")?;
        }
        if res.session_id.is_some() {
            harness_session_id = res.session_id.clone();
        }
        if res.model.is_some() {
            session.model = res.model.clone(); // what the harness actually reported beats what we asked for
        }
        if let Some(cu) = res.cost_usd {
            cost += cu;
            saw_cost = true;
        }
        if let Some(usage) = &res.usage {
            if matches!(harness, Harness::Codex | Harness::Gemini) {
                session.usage_total.get_or_insert_with(Default::default).add(usage);
            } else {
                session.usage_total = Some(usage.clone());
            }
        }
        session.ended_at = Some(now_iso());
        session.harness_session_id = harness_session_id.clone();
        session.cost_usd = if saw_cost { Some(cost) } else { None };
        if !res.is_error {
            journal.session = session.clone(); journal.simulator = sim_state.clone(); journal.next_turn = t.turn + 1; journal.active = false; journal.checkpoint = None;
            // Commit completion before any subsequent work. A crash here never resends this turn.
            journal.save(&run_dir)?;
            if is_git_repo(&ws.dir) {
                let native = harness_session_id.as_ref().and_then(|id| adapters::find_log_by_id(harness, id));
                journal.checkpoint = Some(crate::checkpoint::capture(crate::checkpoint::Capture { cwd: &ws.dir, run_dir: &run_dir, native: native.as_deref(), harness, version: session.version.clone(), turn: t.turn + 1, expected_conversation_turns: user_turns(&session).len() as u32, prompt: turns.get(i + 1).map(|t| t.text.as_str()).unwrap_or(""), configuration_hash: &configuration_hash, limit: o.checkpoint_limit })?);
                journal.save(&run_dir)?;
            }
        }
        write_json(&session_path, &session)?;
        if res.is_error && !o.continue_on_error {
            log(&format!("{}harness reported an error in turn {}; stopping (use --continue-on-error to keep going){}", c.red, t.turn, c.reset));
            break;
        }
    }

    session.id = harness_session_id.clone().unwrap_or_else(|| run_id.clone());
    session.harness_session_id = harness_session_id.clone();
    if saw_cost {
        session.cost_usd = Some(cost);
    }
    for e in &mut session.events { e.source_turn.get_or_insert(e.turn); }
    if let Some(hid) = &harness_session_id {
        if let Some(file) = adapters::find_log_by_id(harness, hid) {
            match adapters::parse_file(harness, &file) {
                // only trust the log when it covers every turn we ran (a forked transcript the
                // harness never appended to must not erase the live capture)
                Ok(full) if user_turns(&full).len() >= user_turns(&session).len() => merge_with_harness_log(&mut session, full),
                Ok(_) => {}
                Err(err) => {
                    if std::env::var_os("CASIMIR_DEBUG").is_some() {
                        log(&format!("could not load harness log: {err}"));
                    }
                }
            }
        }
    }
    renumber_turns(&mut session.events);
    session.checkpoints = session.events.iter().filter(|e| e.kind == EventKind::User && !e.sidechain)
        .filter_map(|e| session.checkpoints.get(&e.source_turn.unwrap_or(e.turn)).cloned().map(|id| (e.turn, id))).collect();
    session.ended_at = Some(now_iso());
    write_json(&session_path, &session)?;
    write_record(&run_dir.join("record.jsonl"), &session, &permission_mode, &sandbox)?;

    let mut diff = diff_base.as_deref().map(|base| capture_diff_against(&ws.dir, base)).unwrap_or_else(|| capture_diff(&ws.dir));
    diff.source = Some(format!("captured from {} against {}", ws.dir.display(), diff_base.as_deref().unwrap_or("unavailable git base")));
    fs::write(run_dir.join("diff.patch"), &diff.patch)?;
    write_json(&run_dir.join("diff.json"), &json!({ "schemaVersion": 1, "files": diff.files, "stat": diff.stat, "source": diff.source }))?;
    if let Some(d) = &diff_a {
        fs::write(run_dir.join("original.patch"), &d.patch)?;
    }

    let check_results = frozen_checks.as_ref().map(|(definition, hash)| crate::checks::execute(definition, hash, &ws.dir, &run_dir)).transpose()?;
    let mut judge = None;
    if o.judge {
        let jo = JudgeOpts { llm: o.judge_llm.clone(), repeats: o.judge_repeats.max(1), brief: brief.clone(), model_a: original.model.clone(), model_b: session.model.clone() };
        log(&format!("{}asking judge ({}) in both candidate orders{}…{}", c.magenta, effective_model(&o.judge_llm), if jo.repeats > 1 { format!(", {} repeats each", jo.repeats) } else { String::new() }, c.reset));
        match judge_sessions_with(original, &session, diff_a.as_ref(), Some(&diff), &jo) {
            Ok(j) => {
                if let Some(w) = &j.family_warning {
                    log(&format!("{}⚠ {w}{}", c.yellow, c.reset));
                }
                judge = Some(j)
            }
            Err(err) => log(&format!("{}judge failed: {err}{}", c.red, c.reset)),
        }
    }
    let mut report = compare_sessions(original, &session, diff_a, Some(diff.clone()), judge);
    report.checks = check_results;
    report.update_outcome(o.pass_threshold);
    if o.judge {
        if let Some(b) = &brief {
            match intent_coverage(b, &session, &o.judge_llm) {
                Ok(ic) => report.intent_coverage = ic,
                Err(err) => log(&format!("{}intent coverage failed: {err}{}", c.yellow, c.reset)),
            }
        }
    }
    fs::write(run_dir.join("report.md"), render_compare_markdown(&report, "original", "rerun"))?;
    write_json(&run_dir.join("report.json"), &report)?;
    session.evaluation = Some(json!({"outcome":report.overall_outcome,"execution":report.execution_status,"checks":report.checks,"judgeAssessment":report.judge_assessment,"judgeModel":if o.judge {Some(effective_model(&o.judge_llm))} else {None},"passThreshold":o.pass_threshold,"rubricHash":brief.as_ref().map(|b| crate::checkpoint::hash(b.rubric_text().as_bytes()))}));
    write_json(&session_path, &session)?;
    if session.execution.as_ref().is_some_and(|e| e.failed_turns == 0) { journal.session = session.clone(); }
    journal.state = if session.execution.as_ref().is_some_and(|e| e.failed_turns > 0) { "failed".into() } else { "completed".into() };
    journal.save(&run_dir)?;
    meta["state"] = json!(if session.execution.as_ref().is_some_and(|e| e.failed_turns > 0) { "failed" } else { "completed" });
    meta["endedAt"] = json!(session.ended_at);
    meta["harnessSessionId"] = json!(harness_session_id);
    meta["completedTurns"] = json!(session.execution.as_ref().unwrap().completed_turns);
    meta["execution"] = json!(session.execution);
    write_json(&run_dir.join("meta.json"), &meta)?;
    Ok(RerunOutcome { run_dir, session: Some(session), report: Some(report), diff: Some(diff), workspace: ws, dry_run: false })
}

// ---------------------------------------------------------------------------------------------
// Replicates

/// Every non-deterministic boundary crossing of a rerun as an addressable envelope
/// (after Chronicle, arXiv 2609.20625): `boundary[occurrence]` with input, output and drift metadata.
/// This is bookkeeping for auditing and diffing runs, not a replay source.
pub fn write_record(path: &Path, session: &Session, permission_mode: &str, sandbox: &str) -> Result<()> {
    let mut envelopes: Vec<Value> = Vec::new();
    let mut model_n = 0usize;
    let mut tool_n: std::collections::HashMap<String, usize> = Default::default();
    let mut pending: std::collections::HashMap<String, usize> = Default::default();
    let drift = json!({ "harness": session.harness(), "model": session.model, "harnessVersion": session.version, "permissionMode": permission_mode, "sandbox": sandbox, "simulator": session.simulator });
    for (index, e) in session.events.iter().enumerate() {
        if e.sidechain { continue; }
        let inherited = session.execution.as_ref().is_some_and(|x| e.source_turn.unwrap_or(e.turn) as usize <= x.preserved_turns);
        match e.kind {
            EventKind::Assistant => {
                model_n += 1;
                // The transcript does not expose the complete model request (system prompt, tool
                // schemas, compaction). Mark it unavailable instead of inventing a replay input.
                envelopes.push(json!({ "boundary": "model", "occurrence": model_n, "address": format!("model[{model_n}]"), "turn": e.turn, "sourceTurn": e.source_turn, "ts": e.ts, "eventIndex": index, "inherited": inherited, "input": Value::Null, "inputAvailable": false, "output": e.text, "usage": e.usage, "model": e.model, "drift": drift }));
            }
            EventKind::ToolCall => {
                if let Some(t) = &e.tool {
                    let n = tool_n.entry(t.name.clone()).or_insert(0);
                    *n += 1;
                    pending.insert(t.id.clone(), envelopes.len());
                    envelopes.push(json!({ "boundary": format!("tool:{}", t.name), "occurrence": n, "address": format!("tool:{}[{n}]", t.name), "turn": e.turn, "sourceTurn": e.source_turn, "ts": e.ts, "eventIndex": index, "inherited": inherited, "id": t.id, "input": t.input, "inputAvailable": true, "output": Value::Null, "unanswered": true, "drift": drift }));
                }
            }
            EventKind::ToolResult => {
                if let Some(r) = &e.result {
                    if let Some(i) = pending.remove(&r.id) {
                        let env = &mut envelopes[i];
                        env["output"] = json!(r.output);
                        env["isError"] = json!(r.is_error);
                        env["resultTs"] = json!(e.ts);
                        env["unanswered"] = json!(false);
                    } else {
                        let name = r.name.as_deref().unwrap_or("tool");
                        let n = tool_n.entry(name.to_string()).or_insert(0);
                        *n += 1;
                        envelopes.push(json!({ "boundary": format!("tool:{name}"), "occurrence": n, "address": format!("tool:{name}[{n}]"), "turn": e.turn, "sourceTurn": e.source_turn, "ts": e.ts, "eventIndex": index, "inherited": inherited, "id": r.id, "input": Value::Null, "inputAvailable": false, "output": r.output, "isError": r.is_error, "orphaned": true, "drift": drift }));
                    }
                }
            }
            _ => {}
        }
    }
    let mut file = fs::File::create(path)?;
    for mut envelope in envelopes { envelope["schemaVersion"] = json!(1); writeln!(file, "{envelope}")?; }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReplicateEntry {
    #[serde(default)] pub outcome: String,
    pub label: String,
    pub dir: PathBuf,
    pub harness: Option<Harness>,
    pub model: Option<String>,
    /// Same harness and model as the original: measures the noise floor.
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub control_model_verified: bool,
    pub simulator_model: Option<String>,
    pub requested_turns: usize,
    pub completed_turns: usize,
    pub errors: usize,
    pub tool_calls: usize,
    pub files_touched: usize,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub judge_score: Option<f64>,
    #[serde(default)]
    pub original_judge_score: Option<f64>,
    pub judge_winner: Option<String>,
    pub order_sensitive: bool,
    pub judge_position_bias: Option<f64>,
    pub judge_test_retest: Option<f64>,
    pub judge_family_warning: Option<String>,
    pub intent_coverage: Option<f64>,
    pub end_state_score: Option<f64>,
    pub end_state_recall: Option<f64>,
    /// 1 − LCS similarity of canonical action sequences against the original.
    pub tool_sequence_distance: f64,
    pub first_divergent_turn: Option<u32>,
    pub pass: bool,
    /// Passed, but the trajectory shows an anti-pattern (search loop, re-read churn, verification skip).
    #[serde(default)]
    pub lucky: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct GroupSummary {
    pub harness: Option<Harness>,
    pub model: Option<String>,
    #[serde(default)]
    pub control: bool,
    pub simulator_model: Option<String>,
    pub n: usize,
    /// Fraction of replicates that passed (marginal per-run success).
    pub pass_at_1: f64,
    /// All replicates passed (strictest consistency measure).
    pub pass_pow_k: bool,
    /// Replicates disagreed on pass/fail.
    pub disagree: bool,
    pub mean_judge: Option<f64>,
    pub min_judge: Option<f64>,
    pub max_judge: Option<f64>,
    pub mean_end_state: Option<f64>,
    pub min_output_tokens: u64,
    pub max_output_tokens: u64,
    pub min_tool_calls: usize,
    pub max_tool_calls: usize,
    pub min_cost: Option<f64>,
    pub max_cost: Option<f64>,
    /// pass@1 excluding lucky passes (process quality, after arXiv 2605.12925).
    pub principled_pass_at_1: f64,
    pub lucky_passes: usize,
    pub mean_distance: f64,
    pub min_distance: f64,
    pub max_distance: f64,
    pub earliest_divergent_turn: Option<u32>,
    /// Mean divergence exceeds the control group's largest divergence (only when a control exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exceeds_control: Option<bool>,
    /// validated | partial | refuted | inconclusive (majority thresholds, after arXiv 2512.06749).
    pub verdict: String,
    /// Wilson 95% interval for pass@1.
    pub pass_at_1_ci: (f64, f64),
    /// Share of replicates whose judge verdict flipped under order swap.
    pub order_sensitive_rate: Option<f64>,
    pub mean_judge_position_bias: Option<f64>,
    pub mean_judge_test_retest: Option<f64>,
    pub mean_intent_coverage: Option<f64>,
    /// Judge-quality warning (high tie rate, position bias above the gate, family asymmetry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_warning: Option<String>,
}

/// Spread of a metric across simulator models for the same target (simulator effect size).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SimulatorSpread {
    pub harness: Option<Harness>,
    pub model: Option<String>,
    pub simulators: usize,
    pub pass_at_1_min: f64,
    pub pass_at_1_max: f64,
    pub judge_min: Option<f64>,
    pub judge_max: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct MatrixSummary {
    #[serde(default)] pub schema_version: u32,
    pub run_dir: PathBuf,
    pub replicates: usize,
    pub pass_threshold: f64,
    pub judged: bool,
    pub entries: Vec<ReplicateEntry>,
    pub groups: Vec<GroupSummary>,
    /// Between-simulator spread per target, when several simulator models were run.
    #[serde(default)]
    pub simulator_spread: Vec<SimulatorSpread>,
    #[serde(default)]
    pub notes: Vec<String>,
}

fn entry_from(label: &str, original: &Session, control: bool, sim_model: Option<String>, requested: usize, outcome: &RerunOutcome, opts: &RerunOpts) -> ReplicateEntry {
    let judged = opts.judge;
    let session = outcome.session.as_ref().unwrap();
    let st = stats(session);
    // Inherited errors belong to the factual prefix, not to this continuation.
    let start = session.execution.as_ref().map(|e| e.preserved_turns as u32 + 1).unwrap_or(1);
    let scoped = |s: &Session, align_source: bool| Session { events: s.events.iter().cloned().map(|mut e| {
        if align_source { e.turn = e.source_turn.unwrap_or(e.turn); }
        e
    }).filter(|e| e.turn >= start && e.turn <= requested as u32).collect(), ..s.clone() };
    let reference = scoped(original, false);
    let candidate = scoped(session, true);
    let continuation_errors = stats(&candidate).errors;
    let report = outcome.report.as_ref().unwrap();
    let completed = session.execution.as_ref().map(|e| e.preserved_turns + e.completed_turns + e.skipped_turns).unwrap_or(0);
    let judge_score = report.judge.as_ref().map(|j| j.score_b);
    let end_state = report.end_state.as_ref().filter(|e| e.informative).map(|e| e.score);
    let stopped = session.execution.as_ref().and_then(|e| e.stop_reason.as_deref());
    let all_turns_done = (completed >= requested && stopped.is_none()) || (stopped == Some("goals_met") && judged);
    let pass = continuation_errors == 0 && session.execution.as_ref().is_some_and(|e| e.failed_turns == 0) && all_turns_done && report.overall_outcome == "passed";
    // Compare only the executed suffix for forks; a long preserved prefix must not dilute distance.
    let distance = 1.0 - sequence_similarity(&action_sequence(&reference), &action_sequence(&candidate));
    ReplicateEntry {
        outcome: report.overall_outcome.clone(),
        label: label.into(),
        dir: outcome.run_dir.clone(),
        harness: session.harness,
        model: session.model.clone(),
        control,
        control_model_verified: !control || original.model.as_deref().zip(session.model.as_deref()).is_some_and(|(a, b)| a == b),
        simulator_model: sim_model,
        requested_turns: requested,
        completed_turns: completed,
        errors: continuation_errors,
        tool_calls: st.tool_calls,
        files_touched: st.files_touched,
        output_tokens: st.usage.output,
        cost_usd: st.cost_usd,
        judge_score,
        original_judge_score: report.judge.as_ref().map(|j| j.score_a),
        judge_winner: report.judge.as_ref().map(|j| j.winner.clone()),
        order_sensitive: report.judge.as_ref().is_some_and(|j| j.order_sensitive),
        judge_position_bias: report.judge.as_ref().map(|j| j.position_bias),
        judge_test_retest: report.judge.as_ref().and_then(|j| j.test_retest),
        judge_family_warning: report.judge.as_ref().and_then(|j| j.family_warning.clone()),
        intent_coverage: report.intent_coverage.as_ref().map(|ic| ic.score),
        end_state_score: end_state,
        end_state_recall: report.end_state.as_ref().filter(|e| e.informative).map(|e| e.recall),
        tool_sequence_distance: distance,
        first_divergent_turn: crate::compare::first_divergent_turn(&reference, &candidate),
        lucky: pass && st.anti_patterns.any(),
        pass,
    }
}

/// Wilson 95% confidence interval for a proportion.
pub fn wilson(successes: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let z = 1.96f64;
    let p = successes as f64 / n as f64;
    let nf = n as f64;
    let denom = 1.0 + z * z / nf;
    let centre = (p + z * z / (2.0 * nf)) / denom;
    let half = z * ((p * (1.0 - p) / nf) + z * z / (4.0 * nf * nf)).sqrt() / denom;
    (if successes == 0 { 0.0 } else { (centre - half).max(0.0) }, if successes == n { 1.0 } else { (centre + half).min(1.0) })
}

fn mean(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<f64>() / v.len() as f64)
    }
}

/// Majority-threshold verdict over replicates (after arXiv 2512.06749): validated when at least
/// two thirds pass; partial when fewer pass but at least two thirds completed cleanly; inconclusive
/// when most replicates did not even complete; refuted otherwise.
pub fn verdict(passes: usize, clean: usize, n: usize) -> &'static str {
    if n == 0 {
        return "inconclusive";
    }
    let need = (2 * n).div_ceil(3).max(1);
    if passes >= need {
        "validated"
    } else if clean < need {
        "inconclusive"
    } else if passes > 0 {
        "partial"
    } else {
        "refuted"
    }
}

pub fn summarize(entries: Vec<ReplicateEntry>, run_dir: PathBuf, replicates: usize, threshold: f64, judged: bool) -> MatrixSummary {
    type Key = (Option<Harness>, Option<String>, bool, Option<String>);
    let mut keys: Vec<Key> = Vec::new();
    for e in &entries {
        let k = (e.harness, e.model.clone(), e.control, e.simulator_model.clone());
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    // control groups first so targets can be compared against them
    keys.sort_by_key(|k| !k.2);
    let mut groups: Vec<GroupSummary> = keys
        .into_iter()
        .map(|(h, m, control, sim)| {
            let es: Vec<&ReplicateEntry> = entries.iter().filter(|e| e.harness == h && e.model == m && e.control == control && e.simulator_model == sim).collect();
            let passes = es.iter().filter(|e| e.pass).count();
            let clean = es.iter().filter(|e| e.errors == 0 && e.completed_turns >= e.requested_turns).count();
            let lucky = es.iter().filter(|e| e.lucky).count();
            let judges: Vec<f64> = es.iter().filter_map(|e| e.judge_score).collect();
            let ends: Vec<f64> = es.iter().filter_map(|e| e.end_state_score).collect();
            let costs: Vec<f64> = es.iter().filter_map(|e| e.cost_usd).collect();
            let dists: Vec<f64> = es.iter().map(|e| e.tool_sequence_distance).collect();
            let judged_entries: Vec<&&ReplicateEntry> = es.iter().filter(|e| e.judge_score.is_some()).collect();
            let order_sensitive_rate = if judged_entries.is_empty() { None } else { Some(judged_entries.iter().filter(|e| e.order_sensitive).count() as f64 / judged_entries.len() as f64) };
            let biases: Vec<f64> = es.iter().filter_map(|e| e.judge_position_bias).collect();
            let retests: Vec<f64> = es.iter().filter_map(|e| e.judge_test_retest).collect();
            let intents: Vec<f64> = es.iter().filter_map(|e| e.intent_coverage).collect();
            let mut warnings: Vec<String> = Vec::new();
            if let Some(r) = order_sensitive_rate {
                if r >= 0.5 && judged_entries.len() >= 2 {
                    warnings.push(format!("{:.0}% of verdicts flipped on order swap: treat this as a judge-quality problem, not as 'no difference'", r * 100.0));
                }
            }
            if let Some(b) = mean(&biases) {
                if b >= 0.10 {
                    warnings.push(format!("mean position bias {b:.2} is above the 0.10 reliability gate"));
                }
            }
            if let Some(w) = es.iter().find_map(|e| e.judge_family_warning.clone()) {
                warnings.push(w);
            }
            GroupSummary {
                harness: h,
                model: m,
                control,
                simulator_model: sim,
                n: es.len(),
                pass_at_1: if es.is_empty() { 0.0 } else { passes as f64 / es.len() as f64 },
                pass_pow_k: !es.is_empty() && passes == es.len(),
                disagree: passes != 0 && passes != es.len(),
                mean_judge: mean(&judges),
                min_judge: judges.iter().cloned().reduce(f64::min),
                max_judge: judges.iter().cloned().reduce(f64::max),
                mean_end_state: mean(&ends),
                min_output_tokens: es.iter().map(|e| e.output_tokens).min().unwrap_or(0),
                max_output_tokens: es.iter().map(|e| e.output_tokens).max().unwrap_or(0),
                min_tool_calls: es.iter().map(|e| e.tool_calls).min().unwrap_or(0),
                max_tool_calls: es.iter().map(|e| e.tool_calls).max().unwrap_or(0),
                min_cost: costs.iter().cloned().reduce(f64::min),
                max_cost: costs.iter().cloned().reduce(f64::max),
                principled_pass_at_1: if es.is_empty() { 0.0 } else { (passes - lucky) as f64 / es.len() as f64 },
                lucky_passes: lucky,
                mean_distance: mean(&dists).unwrap_or(0.0),
                min_distance: dists.iter().cloned().reduce(f64::min).unwrap_or(0.0),
                max_distance: dists.iter().cloned().reduce(f64::max).unwrap_or(0.0),
                earliest_divergent_turn: es.iter().filter_map(|e| e.first_divergent_turn).min(),
                exceeds_control: None,
                verdict: if es.iter().any(|e| e.outcome == "inconclusive") { "inconclusive".into() } else { verdict(passes, clean, es.len()).into() },
                pass_at_1_ci: wilson(passes, es.len()),
                order_sensitive_rate,
                mean_judge_position_bias: mean(&biases),
                mean_judge_test_retest: mean(&retests),
                mean_intent_coverage: mean(&intents),
                judge_warning: if warnings.is_empty() { None } else { Some(warnings.join("; ")) },
            }
        })
        .collect();
    // the simulator model is a blocking factor: compare targets only against the control run with the same simulator
    let clean_group = |g: &GroupSummary| entries.iter().filter(|e| e.control == g.control && e.harness == g.harness && e.model == g.model && e.simulator_model == g.simulator_model).all(|e| e.errors == 0 && e.completed_turns >= e.requested_turns);
    let controls: Vec<(Option<String>, f64)> = groups.iter().filter(|g| g.control && clean_group(g) && entries.iter().filter(|e| e.control && e.model == g.model && e.simulator_model == g.simulator_model).all(|e| e.control_model_verified)).map(|g| (g.simulator_model.clone(), g.max_distance)).collect();
    let mut notes: Vec<String> = Vec::new();
    if entries.iter().any(|e| e.control && !e.control_model_verified) {
        notes.push("control model identity did not match the recorded original model; that group cannot establish a same-model noise floor (use full model identifiers)".into());
    }
    for g in groups.iter_mut().filter(|g| !g.control) {
        if !clean_group(g) {
            notes.push("control-spread comparison withheld for an incomplete or failed target group".into());
            continue;
        }
        if let Some((_, cm)) = controls.iter().find(|(sim, _)| *sim == g.simulator_model) {
            g.exceeds_control = Some(g.mean_distance > *cm);
        } else if entries.iter().any(|e| e.control) {
            notes.push(format!("no complete, error-free control group shares simulator {:?}; the comparison against the noise floor is skipped for that group", g.simulator_model));
        }
    }
    let mut simulator_spread: Vec<SimulatorSpread> = Vec::new();
    let mut targets: Vec<(Option<Harness>, Option<String>, bool)> = Vec::new();
    for g in &groups {
        let k = (g.harness, g.model.clone(), g.control);
        if !targets.contains(&k) {
            targets.push(k);
        }
    }
    for (h, m, control) in targets {
        let gs: Vec<&GroupSummary> = groups.iter().filter(|g| g.harness == h && g.model == m && g.control == control && g.simulator_model.is_some()).collect();
        if gs.len() >= 2 {
            let judges: Vec<f64> = gs.iter().filter_map(|g| g.mean_judge).collect();
            simulator_spread.push(SimulatorSpread {
                harness: h,
                model: m,
                simulators: gs.len(),
                pass_at_1_min: gs.iter().map(|g| g.pass_at_1).fold(1.0, f64::min),
                pass_at_1_max: gs.iter().map(|g| g.pass_at_1).fold(0.0, f64::max),
                judge_min: judges.iter().cloned().reduce(f64::min),
                judge_max: judges.iter().cloned().reduce(f64::max),
            });
        }
    }
    if groups.iter().any(|g| g.simulator_model.is_some()) {
        notes.push("simulated-user pass rates are relative comparisons between groups sharing a simulator, not absolute task success: simulators inflate agent success by roughly 14–20 points against real users (arXiv 2603.11245, 2601.17087)".into());
    }
    if replicates <= 3 {
        notes.push(format!("{replicates} replicate(s) per group give only a coarse estimate; inspect the Wilson intervals before interpreting differences"));
    }
    if !judged { notes.push("pass@1 measures clean execution only; task correctness has not been evaluated".into()); }
    notes.push("lucky and principled are heuristic process flags, not validated AgentLens quality classifications".into());
    MatrixSummary { schema_version: 1, run_dir, replicates, pass_threshold: threshold, judged, entries, groups, simulator_spread, notes }
}

pub fn render_matrix_text(m: &MatrixSummary) -> String {
    let c = colors();
    let f = |v: Option<f64>| v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "-".into());
    let mut out = vec![format!("{}replicates: {} per group; pass = no errors, all turns, judge ≥ {:.1}{}{}", c.bold, m.replicates, m.pass_threshold, if m.judged { "" } else { " (no evaluation evidence: task outcome is inconclusive)" }, c.reset)];
    out.push(format!("{}{}{}{}{}{}{}{}{}{}", pad("replicate", 22), pad("model", 20), pad("turns", 7), pad("errs", 5), pad("tools", 6), pad("out tok", 9), pad("judge", 7), pad("end/recall", 11), pad("dist@turn", 10), "pass"));
    for e in &m.entries {
        out.push(format!(
            "{}{}{}{}{}{}{}{}{}{}",
            pad(&format!("{}{}", e.label, if e.control { " (control)" } else { "" }), 22),
            pad(e.model.as_deref().unwrap_or("-"), 20),
            pad(&format!("{}/{}", e.completed_turns, e.requested_turns), 7),
            pad(&e.errors.to_string(), 5),
            pad(&e.tool_calls.to_string(), 6),
            pad(&fmt_num(e.output_tokens), 9),
            pad(&e.judge_score.map(|s| format!("{s:.1}{}", if e.order_sensitive { "*" } else { "" })).unwrap_or_else(|| "-".into()), 7),
            pad(&format!("{}/{}", f(e.end_state_score), f(e.end_state_recall)), 11),
            pad(&format!("{:.2}@{}", e.tool_sequence_distance, e.first_divergent_turn.map(|t| t.to_string()).unwrap_or_else(|| "-".into())), 10),
            format_args!("{}{}", if e.pass { "yes" } else { "no" }, if e.lucky { " (lucky)" } else { "" })
        ));
    }
    if m.entries.iter().any(|e| e.order_sensitive) {
        out.push(format!("{}* judge verdict flipped when candidate order was swapped{}", c.dim, c.reset));
    }
    if m.entries.iter().any(|e| e.lucky) {
        out.push(format!("{}(lucky) passed, but the trajectory shows a search loop, re-read churn, or skipped verification{}", c.dim, c.reset));
    }
    out.push(String::new());
    for g in &m.groups {
        let name = format!("{}{}{}", g.harness.map(|h| h.to_string()).unwrap_or_default(), g.model.as_ref().map(|m| format!("/{m}")).unwrap_or_default(), g.simulator_model.as_ref().map(|s| format!(" sim={s}")).unwrap_or_default());
        out.push(format!("{}{}{}{}: n={}  verdict={}  pass@1={:.2} (principled {:.2}, lucky {})  pass^k={}  judge mean={} [{}..{}]  end-state mean={}  distance mean={:.2} [{:.2}..{:.2}] earliest divergence turn {}  out tokens {}..{}  tool calls {}..{}{}{}",
            c.bold,
            if g.control { "control " } else { "" },
            name,
            c.reset,
            g.n,
            g.verdict,
            g.pass_at_1, g.principled_pass_at_1, g.lucky_passes,
            if g.pass_pow_k { "yes" } else { "no" },
            f(g.mean_judge), f(g.min_judge), f(g.max_judge),
            f(g.mean_end_state),
            g.mean_distance, g.min_distance, g.max_distance,
            g.earliest_divergent_turn.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
            fmt_num(g.min_output_tokens), fmt_num(g.max_output_tokens),
            g.min_tool_calls, g.max_tool_calls,
            if g.disagree { format!("  {}⚠ replicates disagree on pass/fail{}", c.yellow, c.reset) } else { String::new() },
            match g.exceeds_control {
                Some(true) => format!("  {}→ mean distance exceeds the observed control maximum (descriptive){}", c.green, c.reset),
                Some(false) => format!("  {}→ mean distance does not exceed the observed control maximum (descriptive){}", c.yellow, c.reset),
                None => String::new(),
            }
        ));
        out.push(format!("    pass@1 95% CI [{:.2}, {:.2}]{}{}{}{}",
            g.pass_at_1_ci.0, g.pass_at_1_ci.1,
            g.order_sensitive_rate.map(|r| format!("  judge order-sensitive rate {:.2}", r)).unwrap_or_default(),
            g.mean_judge_position_bias.map(|b| format!("  position bias {b:.2}")).unwrap_or_default(),
            g.mean_judge_test_retest.map(|t| format!("  test-retest {t:.2}")).unwrap_or_default(),
            g.mean_intent_coverage.map(|i| format!("  intent coverage {i:.2}")).unwrap_or_default(),
        ));
        if let Some(w) = &g.judge_warning {
            out.push(format!("    {}⚠ judge: {w}{}", c.yellow, c.reset));
        }
    }
    for sp in &m.simulator_spread {
        out.push(format!("{}between-simulator spread{} for {}{}: pass@1 {:.2}..{:.2}{} across {} simulators — compare this with the between-target spread; when it dominates, the simulator is the finding",
            c.bold, c.reset,
            sp.harness.map(|h| h.to_string()).unwrap_or_default(), sp.model.as_ref().map(|m| format!("/{m}")).unwrap_or_default(),
            sp.pass_at_1_min, sp.pass_at_1_max,
            match (sp.judge_min, sp.judge_max) { (Some(a), Some(b)) => format!(", judge {a:.1}..{b:.1}"), _ => String::new() },
            sp.simulators));
    }
    if !m.groups.iter().any(|g| g.control) && m.groups.len() > 1 {
        out.push(format!("{}no control group: add --control to measure observed same-model variation{}", c.dim, c.reset));
    }
    for n in &m.notes {
        out.push(format!("{}note: {n}{}", c.dim, c.reset));
    }
    out.join("\n")
}

pub fn render_matrix_markdown(m: &MatrixSummary) -> String {
    let f = |v: Option<f64>| v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "-".into());
    let mut md = vec!["# Replicated rerun".into(), String::new(), format!("- replicates per simulator model: {}", m.replicates), format!("- pass criterion: no errors, all turns completed{}", if m.judged { format!(", judge score ≥ {:.1}", m.pass_threshold) } else { String::new() }), String::new()];
    md.push("| replicate | model | simulator | turns | errors | tool calls | output tokens | judge | end-state | recall | distance | first divergent turn | pass |".into());
    md.push("|---|---|---|---|---|---|---|---|---|---|---|---|---|".into());
    for e in &m.entries {
        md.push(format!("| {}{} | {} | {} | {}/{} | {} | {} | {} | {} | {} | {} | {:.2} | {} | {}{} |", e.label, if e.control { " (control)" } else { "" }, e.model.as_deref().unwrap_or("-"), e.simulator_model.as_deref().unwrap_or("-"), e.completed_turns, e.requested_turns, e.errors, e.tool_calls, e.output_tokens, e.judge_score.map(|s| format!("{s:.1}{}", if e.order_sensitive { " (order-sensitive)" } else { "" })).unwrap_or_else(|| "-".into()), f(e.end_state_score), f(e.end_state_recall), e.tool_sequence_distance, e.first_divergent_turn.map(|t| t.to_string()).unwrap_or_else(|| "-".into()), if e.pass { "yes" } else { "no" }, if e.lucky { " (lucky)" } else { "" }));
    }
    md.push(String::new());
    md.push("| group | n | verdict | pass@1 | principled pass@1 | pass^k | judge mean | judge range | end-state mean | distance mean | distance range | vs control | note |".into());
    md.push("|---|---|---|---|---|---|---|---|---|---|---|---|---|".into());
    for g in &m.groups {
        let name = format!("{}{}{}{}", if g.control { "control " } else { "" }, g.harness.map(|h| h.to_string()).unwrap_or_default(), g.model.as_ref().map(|m| format!("/{m}")).unwrap_or_default(), g.simulator_model.as_ref().map(|s| format!(" sim={s}")).unwrap_or_default());
        let mut note = vec![];
        if g.disagree {
            note.push("replicates disagree".to_string());
        }
        if let Some(w) = &g.judge_warning {
            note.push(format!("judge: {w}"));
        }
        md.push(format!("| {} | {} | {} | {:.2} [{:.2}, {:.2}] | {:.2} | {} | {} | {}..{} | {} | {:.2} | {:.2}..{:.2} | {} | {} |", name, g.n, g.verdict, g.pass_at_1, g.pass_at_1_ci.0, g.pass_at_1_ci.1, g.principled_pass_at_1, if g.pass_pow_k { "yes" } else { "no" }, f(g.mean_judge), f(g.min_judge), f(g.max_judge), f(g.mean_end_state), g.mean_distance, g.min_distance, g.max_distance, match g.exceeds_control { Some(true) => "beyond noise floor", Some(false) => "within noise floor", None => "-" }, note.join("; ")));
    }
    if !m.simulator_spread.is_empty() {
        md.extend([String::new(), "| target | simulators | pass@1 spread | judge spread |".into(), "|---|---|---|---|".into()]);
        for sp in &m.simulator_spread {
            md.push(format!("| {}{} | {} | {:.2}..{:.2} | {} |", sp.harness.map(|h| h.to_string()).unwrap_or_default(), sp.model.as_ref().map(|m| format!("/{m}")).unwrap_or_default(), sp.simulators, sp.pass_at_1_min, sp.pass_at_1_max, match (sp.judge_min, sp.judge_max) { (Some(a), Some(b)) => format!("{a:.1}..{b:.1}"), _ => "-".into() }));
        }
    }
    for n in &m.notes {
        md.push(format!("\n> {n}"));
    }
    md.join("\n")
}

/// Run `replicates` reruns for each simulator model (or once when neither is set) and aggregate.
pub fn rerun_matrix(original: &Session, o: &RerunOpts, log: &mut dyn FnMut(&str), out: &mut dyn FnMut(&str)) -> Result<(Option<RerunOutcome>, Option<MatrixSummary>)> {
    validate_run(original, o)?;
    if o.control && original.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
        bail!("a same-model control requires the original model to be recorded in the session");
    }
    let simulate = o.user_mode == "simulate" || o.user_mode == "auto";
    let sim_models: Vec<Option<String>> = if simulate && !o.sim_models.is_empty() { o.sim_models.iter().cloned().map(Some).collect() } else { vec![None] };
    let replicates = o.replicates.max(1);
    if sim_models.len() == 1 && replicates == 1 && !o.control {
        let mut single = o.clone();
        if let Some(Some(m)) = sim_models.first() {
            single.sim_llm.model = Some(m.clone());
        }
        return Ok((Some(rerun(original, &single, log, out)?), None));
    }
    let harness = o.harness.unwrap_or(original.harness());
    let model = o.model.clone().or_else(|| if harness == original.harness() { original.model.clone() } else { None });
    let parent_id = o.run_id.clone().unwrap_or_else(|| make_run_id(original, harness, model.as_deref()));
    let parent_dir = o.out_dir.clone().unwrap_or_else(|| casimir_home().join("runs").join(&parent_id));
    let count = replicates * sim_models.len() * if o.control { 2 } else { 1 };
    log(&format!("Preflight: {count} runs; up to {} harness turns; {} AB/BA judge calls plus brief/coverage and simulator calls. Unavailable costs remain unknown.", count * o.turns.unwrap_or(user_turns(original).len()), if o.judge { count * 2 * o.judge_repeats } else { 0 }));
    let _experiment_lock = if o.dry_run { None } else { Some(crate::util::RunLock::acquire(&parent_dir)?) };
    let frozen = prepare_experiment(original, o, &parent_dir, log)?;
    let requested = o.turns.map(|n| n.min(user_turns(original).len())).unwrap_or(user_turns(original).len());
    let mut entries = Vec::new();
    let c = colors();
    // (control?, harness, model) target groups; the control reruns the original harness and model
    let mut targets: Vec<(bool, Option<Harness>, Option<String>)> = Vec::new();
    if o.control {
        targets.push((true, Some(original.harness()), original.model.clone()));
    }
    targets.push((false, o.harness, o.model.clone()));
    for (control, h, m) in &targets {
        for sm in &sim_models {
            for r in 1..=replicates {
                let label = format!("{}{}r{r}", if *control { "control-" } else { "" }, sm.as_ref().map(|m| format!("{}-", slug(m))).unwrap_or_default());
                log(&format!("\n{}=== replicate {label} ==={}", c.bold, c.reset));
                let mut single = frozen.clone();
                single.control = false;
                single.harness = *h;
                single.model = m.clone();
                single.run_id = Some(format!("{parent_id}-{label}"));
                single.out_dir = Some(parent_dir.join(&label));
                if let Some(m) = sm {
                    single.sim_llm.model = Some(m.clone());
                }
                let outcome = rerun(original, &single, log, out)?;
                if outcome.dry_run {
                    continue;
                }
                let sim_model = if simulate { Some(effective_model(&single.sim_llm)) } else { None };
                entries.push(entry_from(&label, original, *control, sim_model, requested, &outcome, o));
            }
        }
    }
    if o.dry_run {
        return Ok((None, None));
    }
    let summary = summarize(entries, parent_dir.clone(), replicates, o.pass_threshold, o.judge);
    write_json(&parent_dir.join("replicates.json"), &summary)?;
    fs::write(parent_dir.join("report.md"), render_matrix_markdown(&summary))?;
    Ok((None, Some(summary)))
}

/// One reference and rubric for the whole experiment, and a fresh worktree for every replicate.
fn prepare_experiment(original: &Session, o: &RerunOpts, dir: &Path, log: &mut dyn FnMut(&str)) -> Result<RerunOpts> {
    let mut frozen = o.clone();
    frozen.sim_llm.recording_dir = Some(dir.join("llm"));
    frozen.judge_llm.recording_dir = Some(dir.join("llm"));
    if o.workspace == "same" { bail!("replicates require independent worktrees; use --workspace auto or a git repository directory"); }
    if !matches!(o.workspace.as_str(), "auto" | "worktree") {
        let repo = fs::canonicalize(&o.workspace)?;
        if !is_git_repo(&repo) { bail!("replicates require a git repository to create independent worktrees"); }
        frozen.workspace_repo = Some(repo);
    } else if frozen.workspace_repo.is_none() && !original.cwd.as_deref().is_some_and(|p| is_git_repo(Path::new(p))) {
        bail!("replicates require a git repository; pass --workspace <repository>");
    }
    frozen.workspace = "worktree".into();
    if !o.dry_run {
        if dir.join("replicates.json").exists() || dir.join("attribution.json").exists() { bail!("experiment directory already contains results: {}; choose a fresh output directory", dir.display()); }
        fs::create_dir_all(dir)?;
        if let Some(source) = &o.checks {
            let workspace = frozen.workspace_repo.as_deref().or_else(|| original.cwd.as_deref().map(Path::new)).context("experiment repository missing")?;
            crate::checks::freeze(source, dir, workspace)?;
            frozen.checks = Some(dir.join("checks.definition.json"));
        }
        frozen.original_diff = original_diff_for(original, o);
        if let Some(brief) = resolve_brief(original, &frozen, frozen.original_diff.as_ref(), dir, log)? {
            let path = dir.join("brief.json");
            brief.save(&path)?;
            frozen.brief = Some(path);
        }
        frozen.freeze_inputs = true;
    }
    Ok(frozen)
}

// ---------------------------------------------------------------------------------------------
// Failure attribution by resampling at each turn (point-of-commitment rule, arXiv 2606.08275)

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnEffect {
    pub turn: u32,
    pub n: usize,
    pub passes: usize,
    pub pass_rate: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub run_dir: PathBuf,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Attribution {
    #[serde(default)] pub schema_version: u32,
    pub run_dir: PathBuf,
    pub effects: Vec<TurnEffect>,
    /// Latest turn whose resampling still rescues the run (CI excludes zero). None = no turn does.
    pub point_of_commitment: Option<u32>,
    pub replicates: usize,
    #[serde(default)]
    pub dry_run: bool,
    /// Infrastructure failure is distinct from a valid experiment with no rescues.
    #[serde(default)]
    pub execution_failed: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Resample the session at each candidate turn (fork, same message) with replicates, and locate the
/// point of commitment: the latest turn whose pass-rate interval excludes zero. Earlier turns with
/// non-zero effects are confounded by everything re-rolled after them, so the latest one wins.
pub fn attribute(original: &Session, o: &RerunOpts, turns: &[u32], log: &mut dyn FnMut(&str), out: &mut dyn FnMut(&str)) -> Result<Attribution> {
    validate_run(original, o)?;
    if !o.judge { bail!("attribution requires --judge: clean execution alone cannot establish that a failed task was rescued"); }
    if o.harness.unwrap_or(original.harness()) != original.harness() || o.model.as_ref().is_some_and(|m| Some(m) != original.model.as_ref()) {
        bail!("attribution resamples the original harness and model; use fork for model-change interventions");
    }
    if original.model.is_none() { bail!("attribution requires the original model to be recorded"); }
    let evaluation = original.evaluation.as_ref().context("attribution withheld: original has no recorded evaluation; demonstrate failure under frozen criteria first")?;
    if evaluation["outcome"] != "failed" || evaluation["execution"] != "completed" { bail!("attribution withheld: original must demonstrate task failure without infrastructure failure"); }
    if evaluation["judgeModel"].as_str() != Some(effective_model(&o.judge_llm).as_str()) || evaluation["passThreshold"].as_f64() != Some(o.pass_threshold) { bail!("attribution withheld: evaluation configuration differs from the original"); }
    let rubric_hash = o.brief.as_ref().map(|path| Brief::load(path).map(|b| crate::checkpoint::hash(b.rubric_text().as_bytes()))).transpose()?;
    if evaluation.get("rubricHash").is_none() || evaluation["rubricHash"] != json!(rubric_hash) {
        bail!("attribution withheld: supply the original frozen --brief; rubric evidence is missing or differs");
    }
    let checks_hash = o.checks.as_ref().map(fs::read).transpose()?.map(|bytes| crate::checkpoint::hash(&bytes));
    let configuration_hash = crate::checkpoint::hash(&serde_json::to_vec(&json!({"harness":original.harness(),"permissionMode":o.permission_mode.as_deref().unwrap_or("preserve"),"sandbox":o.sandbox.as_deref().unwrap_or("preserve"),"extraArgs":o.extra_args,"checksHash":checks_hash}))?);
    if original.configuration_hash.as_deref() != Some(&configuration_hash) { bail!("attribution withheld: harness permissions, arguments, or executable checks differ from original"); }
    if o.user_mode != "verbatim" || o.sim_models.len() > 1 { bail!("attribution requires verbatim user turns to avoid mixing simulator effects with turn effects"); }
    let n = user_turns(original).len();
    if o.turns.is_some_and(|t| t < n) { bail!("attribution must evaluate the whole task; omit --turns"); }
    if turns.is_empty() { bail!("attribution requires at least one fork turn"); }
    let mut unique = std::collections::BTreeSet::new();
    for &k in turns {
        if !unique.insert(k) { bail!("duplicate attribution turn {k}"); }
        let mut check = o.clone();
        check.from_turn = Some(k);
        validate_run(original, &check)?;
    }
    let harness = o.harness.unwrap_or(original.harness());
    let parent_id = o.run_id.clone().unwrap_or_else(|| format!("{}-attribute", make_run_id(original, harness, o.model.as_deref())));
    let parent_dir = o.out_dir.clone().unwrap_or_else(|| casimir_home().join("runs").join(&parent_id));
    for turn in turns {
        let id = original.checkpoints.get(turn).context("attribution withheld: missing checkpoint")?;
        let checkpoint = crate::checkpoint::load(id)?; crate::checkpoint::require_compatible(&checkpoint)?;
        if checkpoint.configuration_hash != configuration_hash { bail!("attribution withheld: checkpoint configuration differs"); }
    }
    let _experiment_lock = if o.dry_run { None } else { Some(crate::util::RunLock::acquire(&parent_dir)?) };
    // Never draft a replacement rubric for attribution: null means the original used generic criteria.
    let mut attribution_options = o.clone();
    attribution_options.freeze_inputs = true;
    let frozen = prepare_experiment(original, &attribution_options, &parent_dir, log)?;
    let mut effects = Vec::new();
    let mut original_failed = true;
    let mut execution_failed = false;
    for &k in turns {
        let mut opts = frozen.clone();
        opts.from_turn = Some(k);
        opts.intervention = None;
        opts.control = false;
        opts.run_id = Some(format!("{parent_id}-turn{k}"));
        opts.out_dir = Some(parent_dir.join(format!("turn{k}")));
        opts.replicates = o.replicates.max(1);
        let (single, matrix) = rerun_matrix(original, &opts, log, out)?;
        let (n, passes) = match (&single, &matrix) {
            (_, Some(m)) => {
                execution_failed |= m.entries.iter().any(|e| e.errors > 0 || e.judge_score.is_none() || e.outcome == "inconclusive" || e.model != original.model);
                original_failed &= m.entries.iter().all(|e| e.original_judge_score.is_some_and(|s| s < o.pass_threshold));
                (m.entries.len(), m.entries.iter().filter(|e| e.pass).count())
            }
            (Some(s), None) => {
                if s.dry_run { continue; }
                let entry = entry_from("r1", original, false, None, user_turns(original).len(), s, o);
                execution_failed |= entry.errors > 0 || entry.judge_score.is_none() || entry.outcome == "inconclusive" || entry.model != original.model;
                original_failed &= entry.original_judge_score.is_some_and(|s| s < o.pass_threshold);
                (1, usize::from(entry.pass))
            }
            _ => (0, 0),
        };
        let (lo, hi) = wilson(passes, n);
        effects.push(TurnEffect { turn: k, n, passes, pass_rate: if n == 0 { 0.0 } else { passes as f64 / n as f64 }, ci_low: lo, ci_high: hi, run_dir: opts.out_dir.clone().unwrap() });
    }
    let point_of_commitment = if original_failed && !execution_failed && !o.dry_run { effects.iter().filter(|e| e.ci_low > 0.0).map(|e| e.turn).max() } else { None };
    let mut notes = vec!["Turn-level diagnostic conditional on a failed original, judge scores, and reconstructed workspace state; not proof of a causal step. Wilson intervals describe rescue proportions, not paired effect differences.".into()];
    if !original_failed { notes.push("Point of commitment withheld: the judge did not consistently score the original below the pass threshold (or judging failed).".into()); }
    if execution_failed { notes.push("Point of commitment withheld: a harness or judge failed; inspect the saved run diagnostics.".into()); }
    let att = Attribution { schema_version: 1, run_dir: parent_dir.clone(), effects, point_of_commitment, replicates: o.replicates, dry_run: o.dry_run, execution_failed, notes };
    if !o.dry_run {
        write_json(&parent_dir.join("attribution.json"), &att)?;
        fs::write(parent_dir.join("report.md"), render_attribution_markdown(&att))?;
    }
    Ok(att)
}

pub fn render_attribution_text(a: &Attribution) -> String {
    if a.dry_run { return "Dry run: attribution planned; no outcomes evaluated or artifacts written.".into(); }
    let c = colors();
    let mut out = vec![format!("{}resample-at-turn attribution ({} replicates per turn){}", c.bold, a.replicates, c.reset)];
    out.push(format!("{}{}{}{}", pad("turn", 6), pad("pass rate", 12), pad("95% CI", 16), "runs"));
    for e in &a.effects {
        out.push(format!("{}{}{}{}", pad(&e.turn.to_string(), 6), pad(&format!("{}/{} = {:.2}", e.passes, e.n, e.pass_rate), 12), pad(&format!("[{:.2}, {:.2}]", e.ci_low, e.ci_high), 16), e.run_dir.display()));
    }
    out.push(String::new());
    out.push(match a.point_of_commitment {
        Some(t) => format!("{}candidate point of commitment: turn {t}{} — latest tested turn with observed rescues under the configured judge", c.bold, c.reset),
        None => format!("{}no supported point of commitment; see evaluation notes{}", c.yellow, c.reset),
    });
    out.extend(a.notes.iter().cloned());
    out.join("\n")
}

pub fn render_attribution_markdown(a: &Attribution) -> String {
    let mut md = vec!["# Resample-at-turn attribution".into(), String::new(), format!("- replicates per turn: {}", a.replicates), format!("- point of commitment: {}", a.point_of_commitment.map(|t| format!("turn {t}")).unwrap_or_else(|| "none".into())), String::new()];
    md.push("| turn | passes | pass rate | 95% CI | runs |".into());
    md.push("|---|---|---|---|---|".into());
    for e in &a.effects {
        md.push(format!("| {} | {}/{} | {:.2} | [{:.2}, {:.2}] | {} |", e.turn, e.passes, e.n, e.pass_rate, e.ci_low, e.ci_high, e.run_dir.display()));
    }
    md.push(String::new());
    md.extend(a.notes.iter().map(|n| format!("- {n}")));
    md.join("\n")
}
