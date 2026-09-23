//! OpenAI Codex CLI adapter.
//!
//! Session rollouts: `$CODEX_HOME` (default `~/.codex`)`/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`.
//! Lines: `{timestamp, type: session_meta|turn_context|response_item|event_msg|…, payload}`.
//! `response_item` payloads are the model-facing items; `event_msg` payloads are UI events that
//! mostly duplicate them (we only take token counts and failures from those).
//! Rerun: `codex exec --json` prints thread/turn/item events as JSONL; afterwards we re-read the
//! rollout the CLI wrote so the rerun has the same fidelity as the original.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{RunOpts, RunResult, SessionSummary};
use crate::model::{Event, EventKind, Harness, Session, Usage};
use crate::util::{first_line, home_dir, ju64, now_iso, read_jsonl, read_jsonl_head, truncate, walk};

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".codex"))
}

/// Messages Codex injects with role=user that were not typed by the human.
const INJECTED_PREFIXES: &[&str] = &[
    "<environment_context>",
    "<user_instructions>",
    "<permissions_instructions>",
    "<skills_instructions>",
    "<apps_instructions>",
    "<collaboration_mode",
    "<multi_agent",
    "<turn_aborted>",
    "<context_window_guidance>",
    "<mcp_instructions>",
    "<recommended_plugins>",
    "# AGENTS.md instructions",
];

pub fn is_injected(text: &str) -> bool {
    let t = text.trim_start();
    INJECTED_PREFIXES.iter().any(|p| t.starts_with(p))
}

pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|b| matches!(b.get("type").and_then(Value::as_str), Some("input_text" | "output_text" | "text")))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse_args(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| serde_json::json!({ "raw": s })),
        Some(other) => other.clone(),
    }
}

/// Codex tool outputs are often strings embedding exit codes, or JSON with {output, metadata}.
fn parse_output(out: Option<&Value>) -> (String, bool) {
    let mut is_error = false;
    let text = match out {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            let t = s.trim_start();
            let mut text = s.clone();
            if t.starts_with('{') {
                if let Ok(j) = serde_json::from_str::<Value>(t) {
                    if let Some(o) = j.get("output").and_then(Value::as_str) {
                        text = o.to_string();
                        if j.get("metadata").and_then(|m| m.get("exit_code")).and_then(Value::as_i64).is_some_and(|c| c != 0) {
                            is_error = true;
                        }
                    }
                }
            }
            for line in text.lines() {
                if let Some(code) = line.strip_prefix("Exit code: ") {
                    if code.trim().parse::<i64>().is_ok_and(|c| c != 0) {
                        is_error = true;
                    }
                    break;
                }
            }
            text
        }
        Some(obj) => {
            if obj.get("metadata").and_then(|m| m.get("exit_code")).and_then(Value::as_i64).is_some_and(|c| c != 0) {
                is_error = true;
            }
            if let Some(o) = obj.get("output").and_then(Value::as_str) {
                o.to_string()
            } else if let Some(arr) = obj.get("content").and_then(Value::as_array) {
                arr.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
            } else {
                obj.to_string()
            }
        }
    };
    (text, is_error)
}

fn map_usage(u: &Value) -> Usage {
    Usage {
        input: ju64(u, &["input_tokens"]),
        output: ju64(u, &["output_tokens"]),
        cache_read: ju64(u, &["cached_input_tokens"]),
        cache_write: 0,
        reasoning: ju64(u, &["reasoning_output_tokens"]),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Build a normalized session from parsed rollout records.
pub fn parse_records(records: &[Value], file: Option<&Path>) -> Session {
    let mut session = Session::new(Harness::Codex);
    session.path = file.map(|p| p.display().to_string());
    let mut turn = 0u32;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    for rec in records {
        if !rec.is_object() {
            continue;
        }
        let ts = s(rec, "timestamp").unwrap_or("").to_string();
        let p = rec.get("payload").cloned().unwrap_or(Value::Null);
        if !ts.is_empty() {
            if session.started_at.is_none() {
                session.started_at = Some(ts.clone());
            }
            session.ended_at = Some(ts.clone());
        }
        let cur = turn.max(1);
        match s(rec, "type").unwrap_or("") {
            "session_meta" => {
                if let Some(id) = s(&p, "id").or_else(|| s(&p, "session_id")) {
                    session.id = id.to_string();
                }
                session.cwd = s(&p, "cwd").map(String::from);
                session.version = s(&p, "cli_version").map(String::from);
                if let Some(t) = s(&p, "timestamp") {
                    session.started_at = Some(t.to_string());
                }
                if let Some(git) = p.get("git") {
                    session.git_commit = s(git, "commit_hash").map(String::from);
                    session.git_branch = s(git, "branch").map(String::from);
                }
                session.source = s(&p, "source").map(String::from);
            }
            "turn_context" => {
                if session.model.is_none() {
                    session.model = s(&p, "model").map(String::from);
                }
                if session.cwd.is_none() {
                    session.cwd = s(&p, "cwd").map(String::from);
                }
                if session.sandbox.is_none() {
                    if let Some(sp) = p.get("sandbox_policy") {
                        session.sandbox = Some(s(sp, "type").or_else(|| s(sp, "mode")).map(String::from).unwrap_or_else(|| sp.to_string()));
                    }
                }
                if session.permission_mode.is_none() {
                    if let Some(ap) = p.get("approval_policy") {
                        session.permission_mode = Some(ap.as_str().map(String::from).unwrap_or_else(|| ap.to_string()));
                    }
                }
                if let Some(e) = p.get("collaboration_mode").and_then(|c| c.get("settings")).and_then(|st| st.get("reasoning_effort")).and_then(Value::as_str) {
                    session.effort = Some(e.to_string());
                }
            }
            "response_item" => match s(&p, "type").unwrap_or("") {
                "message" => {
                    let text = text_of(p.get("content").unwrap_or(&Value::Null));
                    match s(&p, "role") {
                        Some("user") => {
                            if text.trim().is_empty() {
                                continue;
                            }
                            let t = text.trim_start();
                            if t.starts_with("<turn_aborted>") {
                                session.events.push(Event::system(cur, ts, "interrupt", "turn aborted by user"));
                            } else if t.starts_with("<user_shell_command>") {
                                let cmd = text.replace("<user_shell_command>", "").replace("</user_shell_command>", "");
                                session.events.push(Event::system(cur, ts, "command", cmd.trim()));
                            } else if is_injected(&text) {
                                // harness-injected context: not a human turn
                            } else {
                                turn += 1;
                                session.events.push(Event::text(turn, ts, EventKind::User, text));
                            }
                        }
                        Some("assistant") if !text.trim().is_empty() => {
                            let mut e = Event::text(cur, ts, EventKind::Assistant, text);
                            e.model = session.model.clone();
                            session.events.push(e);
                        }
                        _ => {} // developer/system roles are harness prompts
                    }
                }
                "reasoning" => {
                    let mut parts: Vec<String> = Vec::new();
                    for key in ["summary", "content"] {
                        if let Some(arr) = p.get(key).and_then(Value::as_array) {
                            parts.extend(arr.iter().filter_map(|x| x.get("text").and_then(Value::as_str)).map(String::from));
                        }
                    }
                    session.events.push(Event::text(cur, ts, EventKind::Thinking, parts.join("\n\n")));
                }
                "function_call" => {
                    let id = s(&p, "call_id").or_else(|| s(&p, "id")).unwrap_or("").to_string();
                    let name = s(&p, "name").unwrap_or("").to_string();
                    tool_names.insert(id.clone(), name.clone());
                    session.events.push(Event::tool_call(cur, ts, id, name, parse_args(p.get("arguments"))));
                }
                "custom_tool_call" => {
                    let id = s(&p, "call_id").or_else(|| s(&p, "id")).unwrap_or("").to_string();
                    let name = s(&p, "name").unwrap_or("").to_string();
                    tool_names.insert(id.clone(), name.clone());
                    session.events.push(Event::tool_call(cur, ts, id, name, serde_json::json!({ "patch": p.get("input").cloned().unwrap_or(Value::Null) })));
                }
                "local_shell_call" => {
                    let id = s(&p, "call_id").or_else(|| s(&p, "id")).unwrap_or("").to_string();
                    tool_names.insert(id.clone(), "shell".into());
                    let action = p.get("action").cloned().unwrap_or(Value::Null);
                    let input = serde_json::json!({ "command": action.get("command").cloned().unwrap_or(Value::Null), "workdir": action.get("working_directory").cloned().unwrap_or(Value::Null) });
                    session.events.push(Event::tool_call(cur, ts, id, "shell", input));
                }
                "web_search_call" => {
                    let id = s(&p, "id").or_else(|| s(&p, "call_id")).unwrap_or("").to_string();
                    tool_names.insert(id.clone(), "web_search".into());
                    session.events.push(Event::tool_call(cur, ts, id, "web_search", p.get("action").cloned().unwrap_or_else(|| serde_json::json!({}))));
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let id = s(&p, "call_id").unwrap_or("").to_string();
                    let (output, is_error) = parse_output(p.get("output"));
                    session.events.push(Event::tool_result(cur, ts, id.clone(), tool_names.get(&id).cloned(), output, is_error));
                }
                "compacted" | "compaction" => session.events.push(Event::system(cur, ts, "compact", "context compacted")),
                _ => {}
            },
            "compacted" => session.events.push(Event::system(cur, ts, "compact", "context compacted")),
            "event_msg" => match s(&p, "type").unwrap_or("") {
                "token_count" => {
                    if let Some(total) = p.get("info").and_then(|i| i.get("total_token_usage")) {
                        session.usage_total = Some(map_usage(total));
                    }
                    if let Some(w) = p.get("info").and_then(|i| i.get("model_context_window")).and_then(Value::as_u64) {
                        session.context_window = Some(w);
                    }
                }
                "task_complete" => {
                    if let Some(msg) = p.get("error").and_then(|e| e.get("message")).and_then(Value::as_str) {
                        // the same failure is usually also logged as an `error` event just before
                        let dup = session.events.last().is_some_and(|l| l.kind == EventKind::Error && l.text_str() == msg);
                        if !dup {
                            session.events.push(Event::text(cur, ts, EventKind::Error, msg));
                        }
                    }
                }
                "error" | "stream_error" => {
                    if let Some(msg) = s(&p, "message") {
                        session.events.push(Event::text(cur, ts, EventKind::Error, msg));
                    }
                }
                "turn_aborted" => {
                    session.events.push(Event::system(cur, ts, "interrupt", format!("turn aborted ({})", s(&p, "reason").unwrap_or("unknown"))));
                }
                _ => {} // user_message / agent_message / agent_reasoning duplicate response_items
            },
            _ => {}
        }
    }
    if session.id.is_empty() {
        if let Some(f) = file {
            session.id = id_from_filename(f).unwrap_or_else(|| f.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string());
        }
    }
    if let Some(u) = session.events.iter().find(|e| e.kind == EventKind::User) {
        session.title = Some(truncate(first_line(u.text_str()), 80));
    }
    session
}

fn id_from_filename(f: &Path) -> Option<String> {
    let name = f.file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?;
    if !stem.starts_with("rollout-") || stem.len() < 36 {
        return None;
    }
    let id = &stem[stem.len() - 36..];
    if id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Some(id.to_string())
    } else {
        None
    }
}

pub fn parse_file(file: &Path) -> Result<Session> {
    Ok(parse_records(&read_jsonl(file)?, Some(file)))
}

fn session_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in [codex_home().join("sessions"), codex_home().join("archived_sessions")] {
        walk(&root, &|p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl")), &mut files);
    }
    files
}

pub fn list_sessions() -> Vec<SessionSummary> {
    let mut out = Vec::new();
    for file in session_files() {
        let Ok(meta) = std::fs::metadata(&file) else { continue };
        let Ok(head) = read_jsonl_head(&file, 1024 * 1024) else { continue };
        let sm = head.iter().find(|r| s(r, "type") == Some("session_meta")).and_then(|r| r.get("payload"));
        let first_user = head.iter().find(|r| {
            s(r, "type") == Some("response_item")
                && r.get("payload").is_some_and(|p| s(p, "type") == Some("message") && s(p, "role") == Some("user") && !is_injected(&text_of(p.get("content").unwrap_or(&Value::Null))))
        });
        let Some(first_user) = first_user else { continue };
        let prompt = text_of(first_user.get("payload").and_then(|p| p.get("content")).unwrap_or(&Value::Null));
        if prompt.is_empty() {
            continue;
        }
        let updated: chrono::DateTime<chrono::Utc> = meta.modified().map(Into::into).unwrap_or_else(|_| chrono::Utc::now());
        out.push(SessionSummary {
            harness: Harness::Codex,
            id: sm.and_then(|m| s(m, "id").or_else(|| s(m, "session_id"))).map(String::from).or_else(|| id_from_filename(&file)).unwrap_or_default(),
            cwd: sm.and_then(|m| s(m, "cwd")).map(String::from),
            git_branch: sm.and_then(|m| m.get("git")).and_then(|g| s(g, "branch")).map(String::from),
            started_at: sm.and_then(|m| s(m, "timestamp")).map(String::from).or_else(|| head.first().and_then(|r| s(r, "timestamp")).map(String::from)),
            updated_at: updated.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            title: truncate(first_line(&prompt), 80),
            size_bytes: meta.len(),
            path: file,
        });
    }
    out
}

pub fn find_log_by_id(id: &str) -> Option<PathBuf> {
    session_files().into_iter().find(|p| p.to_string_lossy().contains(id))
}

#[derive(Default, Debug)]
pub struct ExecState {
    pub thread_id: Option<String>,
    pub usage: Option<Usage>,
    pub n: usize,
}

/// Map one `codex exec --json` event into normalized events.
pub fn exec_event_to_events(rec: &Value, turn: u32, state: &mut ExecState) -> Vec<Event> {
    let ts = now_iso();
    let mut evs = Vec::new();
    match s(rec, "type").unwrap_or("") {
        "thread.started" => {
            state.thread_id = s(rec, "thread_id").map(String::from);
        }
        "turn.completed" => {
            if let Some(u) = rec.get("usage") {
                state.usage = Some(map_usage(u));
            }
        }
        "turn.failed" => {
            let msg = rec.get("error").and_then(|e| s(e, "message")).unwrap_or("turn failed");
            evs.push(Event::text(turn, ts, EventKind::Error, msg));
        }
        "error" => evs.push(Event::text(turn, ts, EventKind::Error, s(rec, "message").unwrap_or("error"))),
        "item.completed" => {
            let it = rec.get("item").cloned().unwrap_or(Value::Null);
            let id = s(&it, "id").map(String::from).unwrap_or_else(|| {
                state.n += 1;
                format!("item_{}", state.n)
            });
            match s(&it, "type").unwrap_or("") {
                "agent_message" => {
                    if let Some(t) = s(&it, "text").filter(|t| !t.trim().is_empty()) {
                        evs.push(Event::text(turn, ts, EventKind::Assistant, t));
                    }
                }
                "reasoning" => evs.push(Event::text(turn, ts, EventKind::Thinking, s(&it, "text").unwrap_or(""))),
                "command_execution" => {
                    evs.push(Event::tool_call(turn, ts.clone(), id.clone(), "shell", serde_json::json!({ "command": it.get("command").cloned().unwrap_or(Value::Null) })));
                    let failed = it.get("exit_code").and_then(Value::as_i64).is_some_and(|c| c != 0) || s(&it, "status") == Some("failed");
                    evs.push(Event::tool_result(turn, ts, id, Some("shell".into()), s(&it, "aggregated_output").unwrap_or(""), failed));
                }
                "file_change" => {
                    let changes = it.get("changes").cloned().unwrap_or_else(|| Value::Array(vec![]));
                    let summary = changes
                        .as_array()
                        .map(|a| a.iter().map(|c| format!("{} {}", s(c, "kind").unwrap_or("edit"), s(c, "path").unwrap_or(""))).collect::<Vec<_>>().join("\n"))
                        .unwrap_or_default();
                    evs.push(Event::tool_call(turn, ts.clone(), id.clone(), "apply_patch", serde_json::json!({ "changes": changes })));
                    evs.push(Event::tool_result(turn, ts, id, Some("apply_patch".into()), summary, s(&it, "status") == Some("failed")));
                }
                "mcp_tool_call" => {
                    let name = format!("{}.{}", s(&it, "server").unwrap_or("mcp"), s(&it, "tool").unwrap_or("tool"));
                    evs.push(Event::tool_call(turn, ts.clone(), id.clone(), name.clone(), it.get("arguments").cloned().unwrap_or_else(|| serde_json::json!({}))));
                    let is_err = it.get("error").is_some_and(|e| !e.is_null()) || s(&it, "status") == Some("failed");
                    let out = it.get("error").filter(|e| !e.is_null()).or_else(|| it.get("result")).map(|v| v.to_string()).unwrap_or_default();
                    evs.push(Event::tool_result(turn, ts, id, Some(name), out, is_err));
                }
                "web_search" => evs.push(Event::tool_call(turn, ts, id, "web_search", serde_json::json!({ "query": it.get("query").cloned().unwrap_or(Value::Null) }))),
                "error" => evs.push(Event::text(turn, ts, EventKind::Error, s(&it, "message").unwrap_or("error"))),
                _ => {}
            }
        }
        _ => {}
    }
    evs
}

fn sandbox_args(mode: Option<&str>) -> Vec<String> {
    match mode {
        None | Some("auto") | Some("preserve") => vec![],
        Some("bypass") | Some("danger-full-access") => vec!["--dangerously-bypass-approvals-and-sandbox".into()],
        Some(m) => vec!["-s".into(), m.to_string()],
    }
}

/// Run one user turn through `codex exec --json`.
pub fn run_turn(opts: &RunOpts, on_event: &mut dyn FnMut(&Event)) -> Result<RunResult> {
    let bin = opts.bin.clone().or_else(|| std::env::var("CASIMIR_CODEX_BIN").ok()).unwrap_or_else(|| "codex".into());
    let mut args: Vec<String> = vec!["exec".into(), "--json".into(), "--skip-git-repo-check".into(), "--color".into(), "never".into()];
    if let Some(cwd) = &opts.cwd {
        args.extend(["-C".into(), cwd.display().to_string()]);
    }
    if let Some(m) = &opts.model {
        args.extend(["-m".into(), m.clone()]);
    }
    args.extend(sandbox_args(opts.sandbox.as_deref()));
    args.extend(opts.extra_args.iter().cloned());
    args.extend(["-c".into(), "forced_login_method=chatgpt".into()]);
    match &opts.resume {
        Some(r) => args.extend(["resume".into(), r.clone(), "-".into()]),
        None => args.push("-".into()),
    }

    let mut cmd = Command::new(&bin);
    cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    crate::util::subscription_command(&mut cmd, Harness::Codex);
    let mut process = crate::process::Process::spawn(&mut cmd, opts.prompt.as_bytes(),
        std::time::Duration::from_secs(opts.timeout_secs.unwrap_or(900)), opts.spool.as_deref())?;


    let mut state = ExecState { thread_id: opts.resume.clone(), ..Default::default() };
    let mut res = RunResult { model: opts.model.clone(), ..Default::default() };
    let turn = opts.turn.max(1);
    let mut retained_bytes = 0;
    while let Some(line) = process.next_line()? {
        let t = line.trim();
        if !t.starts_with('{') {
            continue;
        }
        let rec = serde_json::from_str::<Value>(t).context("malformed harness JSON stream; raw bytes retained")?;
        super::retain_raw(&mut res, &rec, &mut retained_bytes)?;
        for ev in exec_event_to_events(&rec, turn, &mut state) {
            on_event(&ev);
            res.events.push(ev);
        }
    }
    let output = process.finish()?;
    let status = output.status;
    res.stderr = output.stderr;
    let completed = state.thread_id.as_deref().is_some_and(|s| !s.is_empty())
        && res.raw.iter().any(|r| s(r, "type") == Some("turn.completed"));
    res.is_error = !status.success() || !completed || res.events.iter().any(|e| e.kind == EventKind::Error);
    if res.is_error && !res.events.iter().any(|e| e.kind == EventKind::Error) {
        let fallback = if status.success() { "codex ended without turn.completed or a thread ID" } else { "codex failed" };
        let ev = Event::text(turn, now_iso(), EventKind::Error, crate::util::stderr_error_line(&res.stderr, fallback));
        on_event(&ev);
        res.events.push(ev);
    }
    res.session_id = state.thread_id;
    res.usage = state.usage;
    Ok(res)
}

pub fn detect(rec: &Value) -> bool {
    rec.is_object() && rec.get("payload").is_some() && rec.get("type").is_some()
}

/// Fork a recorded rollout at `up_to_turn`: write every line before the user message that starts
/// that turn into a new rollout under a new thread id, so `codex exec resume <new_id>` continues
/// from there. Whether the installed Codex version indexes rollouts it did not write itself is not
/// guaranteed; a resume failure surfaces as a harness error.
pub fn prepare_fork(original: &Session, up_to_turn: u32, new_id: &str, cwd: &Path) -> Result<PathBuf> {
    let src = original.harness_log_path.as_deref().or(original.path.as_deref()).context("original session has no native on-disk transcript to fork from")?;
    let records = read_jsonl(Path::new(src))?;
    let mut kept: Vec<Value> = Vec::new();
    let mut turn = 0u32;
    let cwd_s = cwd.display().to_string();
    for mut rec in records {
        let is_user_turn = s(&rec, "type") == Some("response_item")
            && rec.get("payload").is_some_and(|p| {
                s(p, "type") == Some("message") && s(p, "role") == Some("user") && {
                    let text = text_of(p.get("content").unwrap_or(&Value::Null));
                    !text.trim().is_empty() && !is_injected(&text) && !text.trim_start().starts_with("<turn_aborted>") && !text.trim_start().starts_with("<user_shell_command>")
                }
            });
        if is_user_turn {
            turn += 1;
            if turn >= up_to_turn {
                break;
            }
        }
        let is_meta = s(&rec, "type") == Some("session_meta");
        if let Some(p) = rec.get_mut("payload").and_then(Value::as_object_mut) {
            if p.contains_key("cwd") {
                p.insert("cwd".into(), Value::String(cwd_s.clone()));
            }
        }
        if is_meta {
            if let Some(p) = rec.get_mut("payload").and_then(Value::as_object_mut) {
                p.insert("id".into(), Value::String(new_id.into()));
                p.insert("session_id".into(), Value::String(new_id.into()));
                p.insert("cwd".into(), Value::String(cwd_s.clone()));
            }
        }
        kept.push(rec);
    }
    if turn < up_to_turn {
        bail!("session has only {turn} turn(s) before turn {up_to_turn}");
    }
    let now = chrono::Utc::now();
    let dir = codex_home().join("sessions").join(now.format("%Y/%m/%d").to_string());
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("rollout-{}-{new_id}.jsonl", now.format("%Y-%m-%dT%H-%M-%S")));
    let mut out = String::new();
    for r in &kept {
        out.push_str(&r.to_string());
        out.push('\n');
    }
    std::fs::write(&dest, out)?;
    Ok(dest)
}
