//! GitHub Copilot CLI adapter.
//!
//! Sessions: `$COPILOT_HOME` (default `~/.copilot`)`/session-state/<session-id>/` holding
//! `workspace.yaml` (id, cwd, git_root, branch, created_at, updated_at, summary) and
//! `events.jsonl` (`{id, timestamp, type, parentId, data}`; types like `user.message`,
//! `assistant.message`, `tool.execution_start`, `tool.execution_complete`, `session.shutdown`).
//! The SQLite index next to it is lossy, so the JSONL is the authoritative transcript.
//! Rerun: `copilot -p … --allow-all-tools --output-format json` prints the same event shape.
//! GitHub documents the schema as internal and subject to change: treat this adapter as experimental.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{RunOpts, RunResult, SessionSummary};
use crate::model::{Event, EventKind, Harness, Session, Usage};
use crate::util::{clean_command, first_line, home_dir, now_iso, read_jsonl, truncate};

pub fn copilot_home() -> PathBuf {
    std::env::var_os("COPILOT_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".copilot"))
}

fn session_roots() -> Vec<PathBuf> {
    vec![copilot_home().join("session-state"), copilot_home().join("history-session-state")]
}

/// Minimal YAML reader for the flat `workspace.yaml` (scalars plus `|`/`>` block strings).
pub fn parse_workspace_yaml(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with(' ') || line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        if v == "|" || v == ">" || v == "|-" || v == ">-" {
            let mut block = Vec::new();
            while let Some(next) = lines.peek() {
                if next.starts_with(' ') || next.is_empty() {
                    block.push(lines.next().unwrap().trim().to_string());
                } else {
                    break;
                }
            }
            out.insert(k.trim().to_string(), block.join("\n").trim().to_string());
        } else {
            let v = v.trim_matches('"').trim_matches('\'').to_string();
            out.insert(k.trim().to_string(), v);
        }
    }
    out
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn content_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(items)) => items.iter().filter_map(|b| b.get("text").and_then(Value::as_str).or_else(|| b.as_str())).collect::<Vec<_>>().join("\n"),
        Some(Value::Object(_)) => v.and_then(|o| s(o, "text")).map(String::from).unwrap_or_else(|| v.unwrap().to_string()),
        _ => String::new(),
    }
}

fn result_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(t)) => t.clone(),
        Some(obj @ Value::Object(_)) => {
            if let Some(t) = s(obj, "detailedContent").or_else(|| s(obj, "content")) {
                t.to_string()
            } else if let Some(arr) = obj.get("contents").and_then(Value::as_array) {
                arr.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
            } else {
                obj.to_string()
            }
        }
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn args_of(v: &Value) -> Value {
    let mut merged = serde_json::Map::new();
    for key in ["args", "arguments"] {
        if let Some(obj) = v.get(key).and_then(Value::as_object) {
            for (k, val) in obj {
                merged.insert(k.clone(), val.clone());
            }
        }
    }
    Value::Object(merged)
}

/// Shared state while mapping an event stream (log or live) into events.
#[derive(Default)]
pub struct MapState {
    pub turn: u32,
    tool_names: HashMap<String, String>,
    emitted_calls: HashSet<String>,
    pub model: Option<String>,
    pub usage: Option<Usage>,
}

/// Map one Copilot event (from events.jsonl or `--output-format json`) into normalized events.
pub fn event_to_events(rec: &Value, st: &mut MapState) -> Vec<Event> {
    let mut evs = Vec::new();
    let ts = s(rec, "timestamp").map(String::from).unwrap_or_else(now_iso);
    let data = rec.get("data").cloned().unwrap_or(Value::Null);
    let etype = s(rec, "type").unwrap_or("");
    if let Some(m) = s(&data, "selectedModel").or_else(|| s(&data, "currentModel")) {
        if st.model.is_none() || etype == "session.start" {
            st.model = Some(m.to_string());
        }
    }
    let cur = st.turn.max(1);
    match etype {
        "user.message" => {
            let text = content_text(data.get("content").or_else(|| data.get("transformedContent")));
            if text.trim().is_empty() {
                return evs;
            }
            st.turn += 1;
            evs.push(Event::text(st.turn, ts, EventKind::User, text));
        }
        "assistant.message" => {
            let text = content_text(data.get("content"));
            if !text.trim().is_empty() {
                let mut e = Event::text(cur, ts.clone(), EventKind::Assistant, text);
                e.model = st.model.clone();
                e.msg_id = s(rec, "id").map(String::from);
                evs.push(e);
            }
            if let Some(reqs) = data.get("toolRequests").and_then(Value::as_array) {
                for (i, r) in reqs.iter().enumerate() {
                    let id = s(r, "toolCallId").or_else(|| s(r, "id")).map(String::from).unwrap_or_else(|| format!("{}:{i}", s(rec, "id").unwrap_or("req")));
                    let name = s(r, "name").or_else(|| s(r, "toolName")).unwrap_or("tool").to_string();
                    st.tool_names.insert(id.clone(), name.clone());
                    st.emitted_calls.insert(id.clone());
                    evs.push(Event::tool_call(cur, ts.clone(), id, name, args_of(r)));
                }
            }
        }
        "assistant.reasoning" | "assistant.thinking" => {
            let text = content_text(data.get("content").or_else(|| data.get("text")));
            evs.push(Event::text(cur, ts, EventKind::Thinking, text));
        }
        "tool.execution_start" => {
            let id = s(&data, "toolCallId").or_else(|| s(rec, "id")).unwrap_or("").to_string();
            let name = s(&data, "toolName").unwrap_or("tool").to_string();
            st.tool_names.insert(id.clone(), name.clone());
            if st.emitted_calls.insert(id.clone()) {
                evs.push(Event::tool_call(cur, ts, id, name, args_of(&data)));
            }
        }
        "tool.execution_complete" | "tool.execution_error" => {
            let id = s(&data, "toolCallId").or_else(|| s(rec, "parentId")).unwrap_or("").to_string();
            let is_error = etype == "tool.execution_error" || data.get("success").and_then(Value::as_bool) == Some(false);
            let out = result_text(data.get("result").or_else(|| data.get("error")));
            evs.push(Event::tool_result(cur, ts, id.clone(), st.tool_names.get(&id).cloned(), out, is_error));
        }
        "session.shutdown" => {
            if let Some(metrics) = data.get("modelMetrics").and_then(Value::as_object) {
                let mut total = Usage::default();
                for (model, m) in metrics {
                    if st.model.is_none() {
                        st.model = Some(model.clone());
                    }
                    let n = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let cache_read = n("cacheReadTokens");
                    let cache_write = n("cacheWriteTokens");
                    total.input += n("inputTokens").saturating_sub(cache_read + cache_write);
                    total.output += n("outputTokens");
                    total.cache_read += cache_read;
                    total.cache_write += cache_write;
                    total.reasoning += n("reasoningTokens");
                }
                st.usage = Some(total);
            }
        }
        "session.error" | "error" => {
            let msg = s(&data, "message").map(String::from).unwrap_or_else(|| data.to_string());
            evs.push(Event::text(cur, ts, EventKind::Error, msg));
        }
        "session.compaction_complete" | "session.compacted" => evs.push(Event::system(cur, ts, "compact", "context compacted")),
        _ => {}
    }
    evs
}

/// Parse a session directory (workspace.yaml + events.jsonl).
pub fn parse_dir(dir: &Path) -> Result<Session> {
    let ws_path = dir.join("workspace.yaml");
    let ws = std::fs::read_to_string(&ws_path).map(|t| parse_workspace_yaml(&t)).unwrap_or_default();
    let events_path = dir.join("events.jsonl");
    let records = if events_path.exists() { read_jsonl(&events_path)? } else { Vec::new() };
    let mut session = Session::new(Harness::Copilot);
    session.path = Some(dir.display().to_string());
    session.id = ws.get("id").cloned().unwrap_or_else(|| dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string());
    session.cwd = ws.get("cwd").cloned();
    session.git_branch = ws.get("branch").cloned();
    session.started_at = ws.get("created_at").cloned();
    session.ended_at = ws.get("updated_at").cloned();
    if let Some(sum) = ws.get("summary").filter(|v| !v.is_empty()) {
        session.title = Some(truncate(first_line(sum), 80));
    }
    let mut st = MapState::default();
    for rec in &records {
        if let Some(ts) = s(rec, "timestamp") {
            if session.started_at.is_none() {
                session.started_at = Some(ts.to_string());
            }
            session.ended_at = Some(ts.to_string());
        }
        session.events.extend(event_to_events(rec, &mut st));
    }
    session.model = st.model;
    session.usage_total = st.usage;
    if session.title.is_none() {
        if let Some(u) = session.events.iter().find(|e| e.kind == EventKind::User) {
            session.title = Some(truncate(first_line(u.text_str()), 80));
        }
    }
    Ok(session)
}

pub fn parse_file(file: &Path) -> Result<Session> {
    if file.is_dir() {
        return parse_dir(file);
    }
    match file.parent() {
        Some(dir) if file.file_name().and_then(|n| n.to_str()) == Some("events.jsonl") => parse_dir(dir),
        _ => bail!("{} is not a Copilot session directory or events.jsonl", file.display()),
    }
}

pub fn list_sessions() -> Vec<SessionSummary> {
    let mut out = Vec::new();
    for root in session_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for e in entries.flatten() {
            let dir = e.path();
            let ws_path = dir.join("workspace.yaml");
            if !ws_path.exists() {
                continue;
            }
            let ws = std::fs::read_to_string(&ws_path).map(|t| parse_workspace_yaml(&t)).unwrap_or_default();
            let events_path = dir.join("events.jsonl");
            let Ok(meta) = std::fs::metadata(&events_path) else { continue };
            let first_user = crate::util::read_jsonl_head(&events_path, 262144).ok().and_then(|head| {
                head.iter().find(|r| s(r, "type") == Some("user.message")).map(|r| content_text(r.get("data").and_then(|d| d.get("content").or_else(|| d.get("transformedContent")))))
            });
            let Some(prompt) = first_user.filter(|p| !p.trim().is_empty()) else { continue };
            let updated: chrono::DateTime<chrono::Utc> = meta.modified().map(Into::into).unwrap_or_else(|_| chrono::Utc::now());
            out.push(SessionSummary {
                harness: Harness::Copilot,
                id: ws.get("id").cloned().unwrap_or_else(|| dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()),
                cwd: ws.get("cwd").cloned(),
                git_branch: ws.get("branch").cloned(),
                started_at: ws.get("created_at").cloned(),
                updated_at: updated.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                title: ws.get("summary").filter(|v| !v.is_empty()).map(|v| truncate(first_line(v), 80)).unwrap_or_else(|| truncate(first_line(&prompt), 80)),
                size_bytes: meta.len(),
                path: dir,
            });
        }
    }
    out
}

pub fn find_log_by_id(id: &str) -> Option<PathBuf> {
    session_roots().into_iter().map(|r| r.join(id)).find(|d| d.join("events.jsonl").exists())
}

/// Run one user turn through `copilot -p`. The same session id resumes the session on later turns.
pub fn run_turn(opts: &RunOpts, on_event: &mut dyn FnMut(&Event)) -> Result<RunResult> {
    let bin = opts.bin.clone().or_else(|| std::env::var("CASIMIR_COPILOT_BIN").ok()).unwrap_or_else(|| "copilot".into());
    let sid = opts.resume.clone().or_else(|| opts.session_id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut args: Vec<String> = vec!["-p".into(), opts.prompt.clone(), "--output-format".into(), "json".into(), "--log-level".into(), "none".into(), "--session-id".into(), sid.clone()];
    if opts.allow_unrestricted { args.push("--allow-all-tools".into()); }
    if let Some(m) = &opts.model {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(cwd) = &opts.cwd {
        args.extend(["-C".into(), cwd.display().to_string()]);
    }
    args.extend(opts.extra_args.iter().cloned());
    let mut cmd = Command::new(&bin);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    clean_command(&mut cmd);
    let mut process = crate::process::Process::spawn(&mut cmd, opts.prompt.as_bytes(),
        std::time::Duration::from_secs(opts.timeout_secs.unwrap_or(900)), opts.spool.as_deref())?;

    let mut st = MapState { turn: opts.turn.max(1).saturating_sub(1), ..Default::default() };
    let mut res = RunResult { session_id: Some(sid), model: opts.model.clone(), ..Default::default() };
    let mut retained_bytes = 0;
    while let Some(line) = process.next_line()? {
        let t = line.trim();
        if !t.starts_with('{') {
            if !t.is_empty() {
                let ev = Event::text(opts.turn.max(1), now_iso(), EventKind::Error, t);
                on_event(&ev);
                res.events.push(ev);
            }
            continue;
        }
        let rec = serde_json::from_str::<Value>(t).context("malformed harness JSON stream; raw bytes retained")?;
        super::retain_raw(&mut res, &rec, &mut retained_bytes)?;
        for ev in event_to_events(&rec, &mut st) {
            if ev.kind == EventKind::User {
                continue; // our own prompt echo
            }
            on_event(&ev);
            res.events.push(ev);
        }
    }
    let output = process.finish()?;
    let status = output.status;
    res.stderr = output.stderr;
    if st.model.is_some() {
        res.model = st.model;
    }
    res.usage = st.usage;
    let completed = res.raw.iter().any(|r| matches!(s(r, "type"), Some("session.shutdown" | "session.idle" | "assistant.turn_end")))
        && res.events.iter().any(|e| e.kind == EventKind::Assistant || e.kind == EventKind::ToolResult);
    res.is_error = !status.success() || !completed || res.events.iter().any(|e| e.kind == EventKind::Error);
    if res.is_error && !res.events.iter().any(|e| e.kind == EventKind::Error) {
        let ev = Event::text(opts.turn.max(1), now_iso(), EventKind::Error, crate::util::stderr_error_line(&res.stderr, "copilot ended without a completed session"));
        on_event(&ev);
        res.events.push(ev);
    }
    Ok(res)
}

pub fn detect(rec: &Value) -> bool {
    rec.is_object() && rec.get("data").is_some_and(Value::is_object) && s(rec, "type").is_some_and(|t| t.contains('.'))
}
