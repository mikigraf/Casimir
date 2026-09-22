//! Rerun orchestration: replay a recorded session's user turns against a harness/model,
//! capture the new session, diff the workspace, and compare against the original.
//! `rerun_matrix` runs replicates (and several simulator models) and aggregates them.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::adapters::{self, RunOpts};
use crate::compare::{compare_sessions, judge_sessions, render_compare_markdown, Report};
use crate::llm::{effective_model, LlmOpts};
use crate::model::{renumber_turns, stats, user_turns, Event, EventKind, Harness, RerunOf, Session, Simulated, SimulatorInfo};
use crate::render::{format_event, RenderOpts};
use crate::simulate::{simulate_user_turn, SimState};
use crate::util::{casimir_home, colors, first_line, fmt_num, now_iso, now_stamp, pad, slug, write_json};
use crate::workspace::{base_commit, capture_diff, create_worktree, is_git_repo, reconstruct_original_diff, repo_root, Diff};

#[derive(Clone, Debug)]
pub struct RerunOpts {
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
}

impl Default for RerunOpts {
    fn default() -> Self {
        RerunOpts {
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
    let plain = |dir: PathBuf, mode: &str| WorkspacePlan { dir, mode: mode.into(), root: None, commit: None, how: None, repo: None, note: None };
    if !matches!(workspace, "auto" | "worktree" | "same") {
        let dir = fs::canonicalize(workspace).with_context(|| format!("workspace directory does not exist: {workspace}"))?;
        return Ok(plain(dir, "dir"));
    }
    let cwd = original.cwd.as_deref().map(Path::new);
    let Some(cwd) = cwd.filter(|p| p.exists()) else {
        if workspace != "auto" {
            bail!("original cwd is not available here ({}); pass --workspace <dir>", original.cwd.as_deref().unwrap_or("?"));
        }
        let mut p = plain(std::env::current_dir()?, "cwd-fallback");
        p.note = Some(format!("original cwd {} not found; using current directory", original.cwd.as_deref().unwrap_or("?")));
        return Ok(p);
    };
    let git = is_git_repo(cwd);
    if workspace == "same" || (workspace == "auto" && !git) {
        let mut p = plain(cwd.to_path_buf(), "same");
        if !git {
            p.note = Some("original cwd is not a git repo; running in place (no diff capture)".into());
        }
        return Ok(p);
    }
    let repo = repo_root(cwd)?;
    let (commit, how) = base_commit(original, cwd)?;
    let dest = casimir_home().join("worktrees").join(run_id);
    let rel = cwd.strip_prefix(&repo).unwrap_or(Path::new(""));
    let dir = if rel.as_os_str().is_empty() { dest.clone() } else { dest.join(rel) };
    Ok(WorkspacePlan { dir, mode: "worktree".into(), root: Some(dest), commit: Some(commit), how: Some(how), repo: Some(repo), note: None })
}

pub fn make_run_id(original: &Session, harness: Harness, model: Option<&str>) -> String {
    let id: String = original.id.chars().take(8).collect();
    format!("{}-{}-{}-{}", now_stamp(), slug(harness.as_str()), slug(model.unwrap_or("default")), id)
}

/// Merge the harness's own log for the rerun with our live capture: our user events carry
/// simulation metadata, everything else comes from the log, plus live errors the log lacks.
fn merge_with_harness_log(session: &mut Session, full: Session) {
    let ours: Vec<Event> = session.events.iter().filter(|e| e.kind == EventKind::User).cloned().collect();
    let mut merged: Vec<Event> = Vec::new();
    let mut ui = 0;
    for e in &full.events {
        if e.kind == EventKind::User {
            merged.push(ours.get(ui).cloned().unwrap_or_else(|| e.clone()));
            ui += 1;
        } else {
            merged.push(e.clone());
        }
    }
    let known: Vec<String> = full.events.iter().filter(|e| e.kind == EventKind::Error).map(|e| e.text_str().to_string()).collect();
    for e in &session.events {
        if e.kind == EventKind::Error && !known.iter().any(|k| k == e.text_str()) {
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
}

/// Original-session diff: the caller's, else reconstructed from git history when the cwd is a repo.
fn original_diff_for(original: &Session, o: &RerunOpts) -> Option<Diff> {
    o.original_diff.clone().or_else(|| reconstruct_original_diff(original))
}

pub fn rerun(original: &Session, o: &RerunOpts, log: &mut dyn FnMut(&str), out: &mut dyn FnMut(&str)) -> Result<RerunOutcome> {
    let c = colors();
    let harness = o.harness.unwrap_or(original.harness());
    let same_harness = harness == original.harness();
    let model = o.model.clone().or_else(|| if same_harness { original.model.clone() } else { None });
    let run_id = o.run_id.clone().unwrap_or_else(|| make_run_id(original, harness, model.as_deref()));
    let ws = plan_workspace(original, &o.workspace, &run_id)?;
    let turns = user_turns(original);
    if turns.is_empty() {
        bail!("original session has no user turns to replay");
    }
    let max_turns = o.turns.map(|n| n.min(turns.len())).unwrap_or(turns.len());
    let run_dir = o.out_dir.clone().unwrap_or_else(|| casimir_home().join("runs").join(&run_id));
    let simulate = o.user_mode == "simulate" || o.user_mode == "auto";
    let sim_model = effective_model(&o.sim_llm);

    log(&format!("{}rerun{} {}:{} → {}{}", c.bold, c.reset, original.harness(), original.id, harness, model.as_ref().map(|m| format!(" ({m})")).unwrap_or_else(|| " (harness default model)".into())));
    log(&format!("  turns: {max_turns}/{}  user mode: {}{}", turns.len(), o.user_mode, if simulate { format!(" (simulator: {sim_model} via {})", crate::llm::pick_backend(&o.sim_llm.backend)) } else { String::new() }));
    let ws_detail = ws.commit.as_ref().map(|cm| format!(", {} — {}", &cm[..cm.len().min(10)], ws.how.as_deref().unwrap_or(""))).unwrap_or_default();
    log(&format!("  workspace: {} [{}{}]", ws.dir.display(), ws.mode, ws_detail));
    if let Some(n) = &ws.note {
        log(&format!("  {}note: {n}{}", c.yellow, c.reset));
    }
    log(&format!("  output: {}", run_dir.display()));
    if o.dry_run {
        for t in turns.iter().take(max_turns) {
            log(&format!("  turn {}: {}", t.turn, first_line(&t.text).chars().take(100).collect::<String>()));
        }
        return Ok(RerunOutcome { run_dir, session: None, report: None, diff: None, workspace: ws, dry_run: true });
    }

    if ws.mode == "worktree" {
        create_worktree(ws.repo.as_ref().unwrap(), ws.commit.as_ref().unwrap(), ws.root.as_ref().unwrap())?;
        log(&format!("  created worktree {}", ws.root.as_ref().unwrap().display()));
    }
    fs::create_dir_all(&run_dir)?;
    write_json(&run_dir.join("original.json"), original)?;

    let mut session = Session::new(harness);
    session.model = model.clone();
    session.cwd = Some(ws.dir.display().to_string());
    session.title = original.title.clone();
    session.started_at = Some(now_iso());
    session.rerun_of = Some(RerunOf { harness: original.harness(), id: original.id.clone(), path: original.path.clone() });
    session.workspace = Some(serde_json::to_value(&ws)?);
    if simulate {
        session.simulator = Some(SimulatorInfo { model: sim_model.clone(), backend: crate::llm::pick_backend(&o.sim_llm.backend) });
    }
    let mut meta = json!({
        "runId": run_id, "original": session.rerun_of, "harness": harness, "model": model,
        "userMode": o.user_mode, "simulator": session.simulator,
        "judgeModel": if o.judge { Some(effective_model(&o.judge_llm)) } else { None },
        "workspace": ws, "startedAt": session.started_at, "requestedTurns": max_turns,
    });
    write_json(&run_dir.join("meta.json"), &meta)?;
    let mut raw = fs::OpenOptions::new().create(true).append(true).open(run_dir.join("raw.jsonl"))?;
    let start = crate::util::ts_ms(session.started_at.as_deref().unwrap_or(""));
    let isolated = ws.mode == "worktree" || ws.mode == "dir";
    let permission_mode = o.permission_mode.clone().unwrap_or_else(|| if isolated { "auto".into() } else { "acceptEdits".into() });
    let sandbox = o.sandbox.clone().unwrap_or_else(|| if isolated { "auto".into() } else { "workspace-write".into() });
    let render = RenderOpts { thinking: o.thinking, max_lines: 6, ..Default::default() };
    let session_path = run_dir.join("session.json");

    let mut harness_session_id: Option<String> = None;
    let mut cost = 0.0f64;
    let mut saw_cost = false;
    let mut sim_state = SimState::default();
    let mut completed_turns = 0usize;

    for (i, t) in turns.iter().take(max_turns).enumerate() {
        let mut message = t.text.clone();
        let mut simulated: Option<Simulated> = None;
        if i > 0 && simulate {
            log(&format!("{}simulating user for turn {}…{}", c.magenta, t.turn, c.reset));
            let sim = simulate_user_turn(original, &session, t.turn, &o.sim_llm, &mut sim_state)?;
            match sim.message {
                None => {
                    let reason = sim.stop_reason.clone().unwrap_or_else(|| "goals_met".into());
                    log(&format!("{}simulator stopped the session ({reason}): {}{}", c.magenta, sim.reason, c.reset));
                    session.events.push(Event::system(t.turn, now_iso(), &format!("simulator-stop:{reason}"), sim.reason));
                    break;
                }
                Some(m) => {
                    if !sim.verbatim {
                        log(&format!("{}adapted message (grounded in turns {:?}): {}{}", c.magenta, sim.grounded_in, first_line(&m).chars().take(120).collect::<String>(), c.reset));
                    }
                    if sim.retries > 0 {
                        log(&format!("{}simulator needed {} retr{} to ground its message{}", c.yellow, sim.retries, if sim.retries == 1 { "y" } else { "ies" }, c.reset));
                    }
                    simulated = Some(Simulated { verbatim: sim.verbatim, reason: sim.reason, grounded_in: sim.grounded_in });
                    message = m;
                }
            }
        }
        let mut user_ev = Event::text(t.turn, now_iso(), EventKind::User, message.clone());
        user_ev.simulated = simulated;
        if !o.quiet {
            if let Some(s) = format_event(&user_ev, &render, start) {
                out(&s);
            }
        }
        session.events.push(user_ev);

        let opts = RunOpts {
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
        let res = adapters::run_turn(harness, &opts, &mut |ev: &Event| {
            live.push(ev.clone());
            if !o.quiet {
                if let Some(s) = format_event(ev, &render, start) {
                    out(&s);
                }
            }
        })?;
        session.events.extend(live);
        for r in &res.raw {
            let _ = writeln!(raw, "{r}");
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
        if res.usage.is_some() {
            session.usage_total = res.usage.clone();
        }
        session.ended_at = Some(now_iso());
        write_json(&session_path, &session)?;
        if res.is_error && !o.continue_on_error {
            log(&format!("{}harness reported an error in turn {}; stopping (use --continue-on-error to keep going){}", c.red, t.turn, c.reset));
            break;
        }
        completed_turns += 1;
    }

    session.id = harness_session_id.clone().unwrap_or_else(|| run_id.clone());
    session.harness_session_id = harness_session_id.clone();
    if saw_cost {
        session.cost_usd = Some(cost);
    }
    if let Some(hid) = &harness_session_id {
        if let Some(file) = adapters::find_log_by_id(harness, hid) {
            match adapters::parse_file(harness, &file) {
                Ok(full) if full.events.iter().any(|e| e.kind == EventKind::User) => merge_with_harness_log(&mut session, full),
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
    session.ended_at = Some(now_iso());
    write_json(&session_path, &session)?;

    let diff = capture_diff(&ws.dir);
    fs::write(run_dir.join("diff.patch"), &diff.patch)?;
    write_json(&run_dir.join("diff.json"), &json!({ "files": diff.files, "stat": diff.stat, "source": diff.source }))?;
    let diff_a = original_diff_for(original, o);
    if let Some(d) = &diff_a {
        fs::write(run_dir.join("original.patch"), &d.patch)?;
    }

    let mut judge = None;
    if o.judge {
        log(&format!("{}asking judge ({}) in both candidate orders…{}", c.magenta, effective_model(&o.judge_llm), c.reset));
        match judge_sessions(original, &session, diff_a.as_ref(), Some(&diff), &o.judge_llm) {
            Ok(j) => judge = Some(j),
            Err(err) => log(&format!("{}judge failed: {err}{}", c.red, c.reset)),
        }
    }
    let report = compare_sessions(original, &session, diff_a, Some(diff.clone()), judge);
    fs::write(run_dir.join("report.md"), render_compare_markdown(&report, "original", "rerun"))?;
    write_json(&run_dir.join("report.json"), &report)?;
    meta["endedAt"] = json!(session.ended_at);
    meta["harnessSessionId"] = json!(harness_session_id);
    meta["completedTurns"] = json!(completed_turns);
    write_json(&run_dir.join("meta.json"), &meta)?;
    Ok(RerunOutcome { run_dir, session: Some(session), report: Some(report), diff: Some(diff), workspace: ws, dry_run: false })
}

// ---------------------------------------------------------------------------------------------
// Replicates

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReplicateEntry {
    pub label: String,
    pub dir: PathBuf,
    pub simulator_model: Option<String>,
    pub requested_turns: usize,
    pub completed_turns: usize,
    pub errors: usize,
    pub tool_calls: usize,
    pub files_touched: usize,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub judge_score: Option<f64>,
    pub judge_winner: Option<String>,
    pub order_sensitive: bool,
    pub end_state_score: Option<f64>,
    pub pass: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct GroupSummary {
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
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct MatrixSummary {
    pub run_dir: PathBuf,
    pub replicates: usize,
    pub pass_threshold: f64,
    pub judged: bool,
    pub entries: Vec<ReplicateEntry>,
    pub groups: Vec<GroupSummary>,
}

fn entry_from(label: &str, sim_model: Option<String>, requested: usize, outcome: &RerunOutcome, threshold: f64, judged: bool) -> ReplicateEntry {
    let session = outcome.session.as_ref().unwrap();
    let st = stats(session);
    let report = outcome.report.as_ref().unwrap();
    let completed = user_turns(session).iter().filter(|t| t.turn <= requested as u32).count();
    let judge_score = report.judge.as_ref().map(|j| j.score_b);
    let end_state = report.end_state.as_ref().map(|e| e.score);
    let all_turns_done = completed >= requested && !session.events.iter().any(|e| e.kind == EventKind::System && e.subtype.as_deref().is_some_and(|s| s.starts_with("simulator-stop:cannot") || s.starts_with("simulator-stop:out")));
    let pass = st.errors == 0 && all_turns_done && (!judged || judge_score.is_some_and(|s| s >= threshold));
    ReplicateEntry {
        label: label.into(),
        dir: outcome.run_dir.clone(),
        simulator_model: sim_model,
        requested_turns: requested,
        completed_turns: completed,
        errors: st.errors,
        tool_calls: st.tool_calls,
        files_touched: st.files_touched,
        output_tokens: st.usage.output,
        cost_usd: st.cost_usd,
        judge_score,
        judge_winner: report.judge.as_ref().map(|j| j.winner.clone()),
        order_sensitive: report.judge.as_ref().is_some_and(|j| j.order_sensitive),
        end_state_score: end_state,
        pass,
    }
}

fn mean(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<f64>() / v.len() as f64)
    }
}

pub fn summarize(entries: Vec<ReplicateEntry>, run_dir: PathBuf, replicates: usize, threshold: f64, judged: bool) -> MatrixSummary {
    let mut models: Vec<Option<String>> = Vec::new();
    for e in &entries {
        if !models.contains(&e.simulator_model) {
            models.push(e.simulator_model.clone());
        }
    }
    let groups = models
        .into_iter()
        .map(|m| {
            let es: Vec<&ReplicateEntry> = entries.iter().filter(|e| e.simulator_model == m).collect();
            let passes = es.iter().filter(|e| e.pass).count();
            let judges: Vec<f64> = es.iter().filter_map(|e| e.judge_score).collect();
            let ends: Vec<f64> = es.iter().filter_map(|e| e.end_state_score).collect();
            let costs: Vec<f64> = es.iter().filter_map(|e| e.cost_usd).collect();
            GroupSummary {
                simulator_model: m,
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
            }
        })
        .collect();
    MatrixSummary { run_dir, replicates, pass_threshold: threshold, judged, entries, groups }
}

pub fn render_matrix_text(m: &MatrixSummary) -> String {
    let c = colors();
    let f = |v: Option<f64>| v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "-".into());
    let mut out = vec![format!("{}replicates: {} per simulator model; pass = no errors, all turns, judge ≥ {:.1}{}{}", c.bold, m.replicates, m.pass_threshold, if m.judged { "" } else { " (no judge: pass = completed cleanly)" }, c.reset)];
    out.push(format!("{}{}{}{}{}{}{}{}{}", pad("replicate", 26), pad("sim model", 22), pad("turns", 8), pad("errs", 6), pad("tools", 7), pad("out tok", 10), pad("judge", 8), pad("end-state", 11), "pass"));
    for e in &m.entries {
        out.push(format!(
            "{}{}{}{}{}{}{}{}{}",
            pad(&e.label, 26),
            pad(e.simulator_model.as_deref().unwrap_or("-"), 22),
            pad(&format!("{}/{}", e.completed_turns, e.requested_turns), 8),
            pad(&e.errors.to_string(), 6),
            pad(&e.tool_calls.to_string(), 7),
            pad(&fmt_num(e.output_tokens), 10),
            pad(&e.judge_score.map(|s| format!("{s:.1}{}", if e.order_sensitive { "*" } else { "" })).unwrap_or_else(|| "-".into()), 8),
            pad(&f(e.end_state_score), 11),
            if e.pass { "yes" } else { "no" }
        ));
    }
    if m.entries.iter().any(|e| e.order_sensitive) {
        out.push(format!("{}* judge verdict flipped when candidate order was swapped{}", c.dim, c.reset));
    }
    out.push(String::new());
    for g in &m.groups {
        out.push(format!("{}simulator {}{}: n={}  pass@1={:.2}  pass^k={}  judge mean={} [{}..{}]  end-state mean={}  out tokens {}..{}  tool calls {}..{}{}",
            c.bold,
            g.simulator_model.as_deref().unwrap_or("(none)"),
            c.reset,
            g.n,
            g.pass_at_1,
            if g.pass_pow_k { "yes" } else { "no" },
            f(g.mean_judge), f(g.min_judge), f(g.max_judge),
            f(g.mean_end_state),
            fmt_num(g.min_output_tokens), fmt_num(g.max_output_tokens),
            g.min_tool_calls, g.max_tool_calls,
            if g.disagree { format!("  {}⚠ replicates disagree on pass/fail{}", c.yellow, c.reset) } else { String::new() }
        ));
    }
    out.join("\n")
}

pub fn render_matrix_markdown(m: &MatrixSummary) -> String {
    let f = |v: Option<f64>| v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "-".into());
    let mut md = vec!["# Replicated rerun".into(), String::new(), format!("- replicates per simulator model: {}", m.replicates), format!("- pass criterion: no errors, all turns completed{}", if m.judged { format!(", judge score ≥ {:.1}", m.pass_threshold) } else { String::new() }), String::new()];
    md.push("| replicate | simulator | turns | errors | tool calls | output tokens | judge | end-state | pass |".into());
    md.push("|---|---|---|---|---|---|---|---|---|".into());
    for e in &m.entries {
        md.push(format!("| {} | {} | {}/{} | {} | {} | {} | {} | {} | {} |", e.label, e.simulator_model.as_deref().unwrap_or("-"), e.completed_turns, e.requested_turns, e.errors, e.tool_calls, e.output_tokens, e.judge_score.map(|s| format!("{s:.1}{}", if e.order_sensitive { " (order-sensitive)" } else { "" })).unwrap_or_else(|| "-".into()), f(e.end_state_score), if e.pass { "yes" } else { "no" }));
    }
    md.push(String::new());
    md.push("| simulator | n | pass@1 | pass^k | judge mean | judge range | end-state mean | output tokens | tool calls | note |".into());
    md.push("|---|---|---|---|---|---|---|---|---|---|".into());
    for g in &m.groups {
        md.push(format!("| {} | {} | {:.2} | {} | {} | {}..{} | {} | {}..{} | {}..{} | {} |", g.simulator_model.as_deref().unwrap_or("(none)"), g.n, g.pass_at_1, if g.pass_pow_k { "yes" } else { "no" }, f(g.mean_judge), f(g.min_judge), f(g.max_judge), f(g.mean_end_state), g.min_output_tokens, g.max_output_tokens, g.min_tool_calls, g.max_tool_calls, if g.disagree { "replicates disagree" } else { "" }));
    }
    md.join("\n")
}

/// Run `replicates` reruns for each simulator model (or once when neither is set) and aggregate.
pub fn rerun_matrix(original: &Session, o: &RerunOpts, log: &mut dyn FnMut(&str), out: &mut dyn FnMut(&str)) -> Result<(Option<RerunOutcome>, Option<MatrixSummary>)> {
    let simulate = o.user_mode == "simulate" || o.user_mode == "auto";
    let sim_models: Vec<Option<String>> = if simulate && !o.sim_models.is_empty() { o.sim_models.iter().cloned().map(Some).collect() } else { vec![None] };
    let replicates = o.replicates.max(1);
    if sim_models.len() == 1 && replicates == 1 {
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
    fs::create_dir_all(&parent_dir)?;
    let requested = o.turns.map(|n| n.min(user_turns(original).len())).unwrap_or(user_turns(original).len());
    let mut entries = Vec::new();
    let c = colors();
    for sm in &sim_models {
        for r in 1..=replicates {
            let label = match sm {
                Some(m) => format!("{}-r{r}", slug(m)),
                None => format!("r{r}"),
            };
            log(&format!("\n{}=== replicate {label} ==={}", c.bold, c.reset));
            let mut single = o.clone();
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
            entries.push(entry_from(&label, sim_model, requested, &outcome, o.pass_threshold, o.judge));
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
