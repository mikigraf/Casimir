//! Rerun orchestration: replay a recorded session's user turns against a harness/model,
//! capture the new session, diff the workspace, and compare against the original.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::adapters::{self, RunOpts};
use crate::compare::{compare_sessions, judge_sessions, render_compare_markdown, Report};
use crate::llm::LlmOpts;
use crate::model::{renumber_turns, user_turns, Event, EventKind, Harness, RerunOf, Session, Simulated};
use crate::render::{format_event, RenderOpts};
use crate::simulate::simulate_user_turn;
use crate::util::{casimir_home, colors, first_line, now_iso, now_stamp, slug, write_json};
use crate::workspace::{base_commit, capture_diff, create_worktree, is_git_repo, repo_root, Diff};

#[derive(Clone, Debug, Default)]
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
    pub llm: LlmOpts,
    pub out_dir: Option<PathBuf>,
    pub quiet: bool,
    pub thinking: bool,
    pub dry_run: bool,
    pub continue_on_error: bool,
    pub extra_args: Vec<String>,
    pub run_id: Option<String>,
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

    log(&format!("{}rerun{} {}:{} → {}{}", c.bold, c.reset, original.harness(), original.id, harness, model.as_ref().map(|m| format!(" ({m})")).unwrap_or_else(|| " (harness default model)".into())));
    log(&format!("  turns: {max_turns}/{}  user mode: {}", turns.len(), o.user_mode));
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
    let mut meta = json!({
        "runId": run_id, "original": session.rerun_of, "harness": harness, "model": model,
        "userMode": o.user_mode, "workspace": ws, "startedAt": session.started_at, "requestedTurns": max_turns,
    });
    write_json(&run_dir.join("meta.json"), &meta)?;
    let mut raw = fs::OpenOptions::new().create(true).append(true).open(run_dir.join("raw.jsonl"))?;
    let start = crate::util::ts_ms(session.started_at.as_deref().unwrap_or(""));
    let isolated = ws.mode == "worktree" || ws.mode == "dir";
    let permission_mode = o.permission_mode.clone().unwrap_or_else(|| if isolated { "auto".into() } else { "acceptEdits".into() });
    let sandbox = o.sandbox.clone().unwrap_or_else(|| if isolated { "auto".into() } else { "workspace-write".into() });
    let render = RenderOpts { thinking: o.thinking, max_lines: 6, ..Default::default() };

    let mut harness_session_id: Option<String> = None;
    let mut cost = 0.0f64;
    let mut saw_cost = false;
    let mut session_path = run_dir.join("session.json");
    session_path.set_file_name("session.json");

    for (i, t) in turns.iter().take(max_turns).enumerate() {
        let mut message = t.text.clone();
        let mut simulated: Option<Simulated> = None;
        if i > 0 && (o.user_mode == "simulate" || o.user_mode == "auto") {
            log(&format!("{}simulating user for turn {}…{}", c.magenta, t.turn, c.reset));
            let sim = simulate_user_turn(original, &session, t.turn, &o.llm)?;
            match sim.message {
                None => {
                    log(&format!("{}simulator stopped the session: {}{}", c.magenta, sim.reason, c.reset));
                    session.events.push(Event::system(t.turn, now_iso(), "simulator-stop", sim.reason));
                    break;
                }
                Some(m) => {
                    if !sim.verbatim {
                        log(&format!("{}adapted message: {}{}", c.magenta, first_line(&m).chars().take(120).collect::<String>(), c.reset));
                    }
                    simulated = Some(Simulated { verbatim: sim.verbatim, reason: sim.reason });
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
    write_json(&run_dir.join("diff.json"), &json!({ "files": diff.files, "stat": diff.stat }))?;

    let mut judge = None;
    if o.judge {
        log(&format!("{}asking judge…{}", c.magenta, c.reset));
        match judge_sessions(original, &session, None, Some(&diff), &o.llm) {
            Ok(j) => judge = Some(j),
            Err(err) => log(&format!("{}judge failed: {err}{}", c.red, c.reset)),
        }
    }
    let report = compare_sessions(original, &session, None, Some(diff.clone()), judge);
    fs::write(run_dir.join("report.md"), render_compare_markdown(&report, "original", "rerun"))?;
    write_json(&run_dir.join("report.json"), &report)?;
    meta["endedAt"] = json!(session.ended_at);
    meta["harnessSessionId"] = json!(harness_session_id);
    write_json(&run_dir.join("meta.json"), &meta)?;
    Ok(RerunOutcome { run_dir, session: Some(session), report: Some(report), diff: Some(diff), workspace: ws, dry_run: false })
}
