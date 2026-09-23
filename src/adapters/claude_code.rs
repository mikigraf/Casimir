//! Claude Code adapter.
//!
//! Session logs: `$CLAUDE_CONFIG_DIR` (default `~/.claude`)`/projects/<cwd-slug>/<session-id>.jsonl`.
//! Each line is a record; `user` / `assistant` records carry an API-shaped `message`.
//! Rerun: `claude -p --output-format stream-json --verbose` emits records with the same
//! `message` shape, so one mapper serves both the on-disk log and the live stream.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use anyhow::Context as _;
use std::process::{Command, Stdio};

use super::{RunOpts, RunResult, SessionSummary};
use crate::model::{Event, EventKind, Harness, Session, Usage};
use crate::util::{clean_command, first_line, home_dir, jstr, ju64, now_iso, read_jsonl, read_jsonl_head, truncate, walk};

pub fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".claude"))
}

const SKIP_TYPES: &[&str] = &["queue-operation", "atis-latch", "last-prompt", "attachment", "file-history-snapshot", "progress"];

/// Remove `<system-reminder>…</system-reminder>` blocks the harness appends to user text.
pub fn strip_reminders(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</system-reminder>") {
            Some(end) => rest = &rest[start + end + "</system-reminder>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let s = text.find(open)? + open.len();
    let e = text[s..].find(close)? + s;
    Some(&text[s..e])
}

pub fn text_of_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn map_usage(u: Option<&Value>) -> Option<Usage> {
    let u = u?;
    if !u.is_object() {
        return None;
    }
    Some(Usage {
        input: ju64(u, &["input_tokens"]),
        output: ju64(u, &["output_tokens"]),
        cache_read: ju64(u, &["cache_read_input_tokens"]),
        cache_write: ju64(u, &["cache_creation_input_tokens"]),
        reasoning: ju64(u, &["output_tokens_details", "thinking_tokens"]),
    })
}

/// Classify a user-role record's text. Returns (kind, subtype, text) or None for "nothing to emit".
fn classify_user_text(raw: &str, rec: &Value) -> Option<(EventKind, Option<&'static str>, String)> {
    let is_flag = |k: &str| rec.get(k).and_then(Value::as_bool).unwrap_or(false);
    if is_flag("isCompactSummary") {
        return Some((EventKind::System, Some("compact"), raw.to_string()));
    }
    if let Some(cmd) = between(raw, "<command-name>", "</command-name>") {
        let args = between(raw, "<command-args>", "</command-args>").unwrap_or("");
        return Some((EventKind::System, Some("command"), format!("{} {}", cmd.trim(), args.trim()).trim().to_string()));
    }
    if let Some(out) = between(raw, "<local-command-stdout>", "</local-command-stdout>") {
        return Some((EventKind::System, Some("command-output"), out.trim().to_string()));
    }
    if raw.trim_start().starts_with("[Request interrupted by user") {
        return Some((EventKind::System, Some("interrupt"), raw.trim().to_string()));
    }
    let clean = strip_reminders(raw);
    if clean.is_empty() {
        return None;
    }
    if is_flag("isMeta") {
        return Some((EventKind::System, Some("meta"), clean));
    }
    Some((EventKind::User, None, clean))
}

/// Convert one user/assistant record (log line or stream-json line) into events.
pub fn record_to_events(rec: &Value, turn: u32) -> Vec<Event> {
    let mut events = Vec::new();
    let ts = rec.get("timestamp").and_then(Value::as_str).map(String::from).unwrap_or_else(now_iso);
    let sidechain = rec.get("isSidechain").and_then(Value::as_bool).unwrap_or(false);
    let msg = rec.get("message").cloned().unwrap_or(Value::Null);
    let rtype = rec.get("type").and_then(Value::as_str).unwrap_or("");
    let mk = |kind: EventKind| {
        let mut e = Event::new(turn, ts.clone(), kind);
        e.sidechain = sidechain;
        e
    };

    if rtype == "user" {
        let content = msg.get("content").cloned().unwrap_or(Value::Null);
        if let Some(items) = content.as_array() {
            for b in items {
                if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                    let output = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(arr @ Value::Array(_)) => text_of_content(arr),
                        Some(Value::Null) | None => String::new(),
                        Some(other) => other.to_string(),
                    };
                    let mut e = mk(EventKind::ToolResult);
                    e.result = Some(crate::model::ToolResult {
                        id: b.get("tool_use_id").and_then(Value::as_str).unwrap_or("").to_string(),
                        name: None,
                        output,
                        is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    });
                    events.push(e);
                }
            }
        }
        let text = text_of_content(&content);
        if let Some((kind, subtype, text)) = classify_user_text(&text, rec) {
            let mut e = mk(kind);
            e.text = Some(text);
            e.subtype = subtype.map(String::from);
            events.push(e);
        }
        return events;
    }

    if rtype == "assistant" {
        let usage = map_usage(msg.get("usage"));
        // e.g. "<synthetic>": harness-generated API error text, not a model message
        let synthetic = msg.get("model").and_then(Value::as_str).is_some_and(|m| m.starts_with('<'));
        let model = if synthetic { None } else { msg.get("model").and_then(Value::as_str).map(String::from) };
        let msg_id = msg.get("id").and_then(Value::as_str).map(String::from);
        let mut first = true;
        if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
            for b in blocks {
                let btype = b.get("type").and_then(Value::as_str).unwrap_or("");
                let btext = b.get("text").and_then(Value::as_str).unwrap_or("");
                if synthetic && btype == "text" && !btext.trim().is_empty() {
                    let mut e = mk(EventKind::Error);
                    e.text = Some(btext.to_string());
                    events.push(e);
                    continue;
                }
                let mut e = match btype {
                    "text" => {
                        if btext.trim().is_empty() {
                            continue;
                        }
                        let mut e = mk(EventKind::Assistant);
                        e.text = Some(btext.to_string());
                        e
                    }
                    "thinking" | "redacted_thinking" => {
                        let mut e = mk(EventKind::Thinking);
                        e.text = Some(b.get("thinking").and_then(Value::as_str).unwrap_or("").to_string());
                        e
                    }
                    "tool_use" => {
                        let mut e = mk(EventKind::ToolCall);
                        e.tool = Some(crate::model::ToolCall {
                            id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                            name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                            input: b.get("input").cloned().unwrap_or(Value::Null),
                        });
                        e
                    }
                    _ => continue,
                };
                e.model = model.clone();
                e.msg_id = msg_id.clone();
                if first {
                    e.usage = usage.clone();
                    first = false;
                }
                events.push(e);
            }
        }
        if msg.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
            let mut e = mk(EventKind::Error);
            e.text = Some("model refused (stop_reason=refusal)".into());
            events.push(e);
        }
    }
    events
}

/// Build a normalized session from parsed log records.
pub fn parse_records(records: &[Value], file: Option<&Path>) -> Session {
    let mut session = Session::new(Harness::ClaudeCode);
    session.path = file.map(|p| p.display().to_string());
    let mut turn = 0u32;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    for rec in records {
        if !rec.is_object() {
            continue;
        }
        let t = rec.get("type").and_then(Value::as_str).unwrap_or("");
        if SKIP_TYPES.contains(&t) {
            continue;
        }
        if session.id.is_empty() {
            if let Some(id) = rec.get("sessionId").and_then(Value::as_str) {
                session.id = id.to_string();
            }
        }
        match t {
            "ai-title" => {
                if let Some(v) = rec.get("aiTitle").and_then(Value::as_str) {
                    session.title = Some(v.to_string());
                }
            }
            "custom-title" => {
                if let Some(v) = rec.get("customTitle").and_then(Value::as_str) {
                    session.title = Some(v.to_string());
                }
            }
            "summary" => {
                if session.title.is_none() {
                    session.title = rec.get("summary").and_then(Value::as_str).map(String::from);
                }
            }
            "system" => {
                let ts = rec.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string();
                let subtype = rec.get("subtype").and_then(Value::as_str).unwrap_or("");
                if subtype.contains("compact") {
                    session.events.push(Event::system(turn.max(1), ts, "compact", "context compacted"));
                } else if rec.get("level").and_then(Value::as_str) == Some("error") {
                    let text = rec.get("content").and_then(Value::as_str).unwrap_or(subtype).to_string();
                    session.events.push(Event::text(turn.max(1), ts, EventKind::Error, text));
                }
            }
            "user" | "assistant" => {
                let set = |slot: &mut Option<String>, key: &str| {
                    if slot.is_none() {
                        if let Some(v) = rec.get(key).and_then(Value::as_str) {
                            *slot = Some(v.to_string());
                        }
                    }
                };
                set(&mut session.cwd, "cwd");
                set(&mut session.git_branch, "gitBranch");
                set(&mut session.version, "version");
                set(&mut session.permission_mode, "permissionMode");
                set(&mut session.started_at, "timestamp");
                if let Some(ts) = rec.get("timestamp").and_then(Value::as_str) {
                    session.ended_at = Some(ts.to_string());
                }
                for mut ev in record_to_events(rec, turn.max(1)) {
                    if ev.kind == EventKind::User && !ev.sidechain {
                        turn += 1;
                        ev.turn = turn;
                    }
                    if let Some(tool) = &ev.tool {
                        tool_names.insert(tool.id.clone(), tool.name.clone());
                    }
                    if let Some(r) = ev.result.as_mut() {
                        if r.name.is_none() {
                            r.name = tool_names.get(&r.id).cloned();
                        }
                    }
                    if session.model.is_none() {
                        session.model = ev.model.clone();
                    }
                    session.events.push(ev);
                }
            }
            _ => {}
        }
    }
    if session.title.is_none() {
        if let Some(u) = session.events.iter().find(|e| e.kind == EventKind::User) {
            session.title = Some(truncate(first_line(u.text_str()), 80));
        }
    }
    if session.id.is_empty() {
        if let Some(f) = file {
            session.id = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        }
    }
    session
}

pub fn parse_file(file: &Path) -> Result<Session> {
    Ok(parse_records(&read_jsonl(file)?, Some(file)))
}

/// Cheap metadata for listing (reads the head of each file only).
pub fn list_sessions() -> Vec<SessionSummary> {
    let root = config_dir().join("projects");
    let mut files = Vec::new();
    walk(&root, &|p: &Path| p.extension().is_some_and(|e| e == "jsonl") && p.parent().and_then(|d| d.parent()) == Some(root.as_path()), &mut files);
    let mut out = Vec::new();
    for file in files {
        let Ok(meta) = std::fs::metadata(&file) else { continue };
        if meta.len() == 0 {
            continue;
        }
        let Ok(head) = read_jsonl_head(&file, 65536) else { continue };
        let first = head.iter().find(|r| r.get("type").and_then(Value::as_str) == Some("user") && jstr(r, &["message", "content"]).is_some());
        let any = head.iter().find(|r| r.get("sessionId").is_some());
        let title = head.iter().find(|r| r.get("type").and_then(Value::as_str) == Some("ai-title")).and_then(|r| r.get("aiTitle").and_then(Value::as_str));
        if first.is_none() && title.is_none() {
            continue;
        }
        let prompt = first.and_then(|r| jstr(r, &["message", "content"])).map(strip_reminders).unwrap_or_default();
        let get = |r: Option<&Value>, k: &str| r.and_then(|r| r.get(k)).and_then(Value::as_str).map(String::from);
        let updated: chrono::DateTime<chrono::Utc> = meta.modified().map(Into::into).unwrap_or_else(|_| chrono::Utc::now());
        out.push(SessionSummary {
            harness: Harness::ClaudeCode,
            id: get(any, "sessionId").unwrap_or_else(|| file.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()),
            cwd: get(first, "cwd").or_else(|| get(any, "cwd")),
            git_branch: get(first, "gitBranch"),
            started_at: get(first, "timestamp").or_else(|| get(any, "timestamp")),
            updated_at: updated.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            title: title.map(String::from).unwrap_or_else(|| truncate(first_line(&prompt), 80)),
            size_bytes: meta.len(),
            path: file,
        });
    }
    out
}

/// Locate the on-disk log for a session id written by this harness.
pub fn find_log_by_id(id: &str) -> Option<PathBuf> {
    let root = config_dir().join("projects");
    let want = format!("{id}.jsonl");
    let mut hits = Vec::new();
    walk(&root, &|p: &Path| p.file_name().and_then(|n| n.to_str()) == Some(want.as_str()), &mut hits);
    hits.into_iter().next()
}

fn permission_args(mode: Option<&str>) -> Vec<String> {
    match mode {
        None | Some("auto") | Some("bypassPermissions") | Some("bypass") => vec!["--dangerously-skip-permissions".into()],
        Some(m) => vec!["--permission-mode".into(), m.to_string()],
    }
}

/// Run one user turn through `claude -p` and stream normalized events to `on_event`.
pub fn run_turn(opts: &RunOpts, on_event: &mut dyn FnMut(&Event)) -> Result<RunResult> {
    let bin = opts.bin.clone().or_else(|| std::env::var("CASIMIR_CLAUDE_BIN").ok()).unwrap_or_else(|| "claude".into());
    let mut args: Vec<String> = vec!["-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into()];
    if let Some(m) = &opts.model {
        args.extend(["--model".into(), m.clone()]);
    }
    match &opts.resume {
        Some(r) => args.extend(["--resume".into(), r.clone()]),
        None => args.extend(["--session-id".into(), opts.session_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string())]),
    }
    args.extend(permission_args(opts.permission_mode.as_deref()));
    args.extend(opts.extra_args.iter().cloned());

    let mut cmd = Command::new(&bin);
    cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    clean_command(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("spawning {bin}"))?;

    let mut stdin = child.stdin.take().context("stdin")?;
    let prompt = opts.prompt.clone();
    std::thread::spawn(move || {
        let _ = stdin.write_all(prompt.as_bytes());
    });
    let stderr = child.stderr.take().context("stderr")?;
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        s
    });

    let mut res = RunResult { session_id: opts.resume.clone().or_else(|| opts.session_id.clone()), ..Default::default() };
    let mut result_rec: Option<Value> = None;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let turn = opts.turn.max(1);
    let stdout = child.stdout.take().context("stdout")?;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Ok(mut rec) = serde_json::from_str::<Value>(t) else { continue };
        res.raw.push(rec.clone());
        if let Some(sid) = rec.get("session_id").and_then(Value::as_str) {
            res.session_id = Some(sid.to_string());
        }
        let rtype = rec.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        if rtype == "result" {
            if rec.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                if let Some(text) = rec.get("result").and_then(Value::as_str) {
                    let ev = Event::text(turn, now_iso(), EventKind::Error, text);
                    on_event(&ev);
                    res.events.push(ev);
                }
            }
            result_rec = Some(rec);
            continue;
        }
        if rtype != "user" && rtype != "assistant" {
            continue;
        }
        if rec.get("timestamp").is_none() {
            rec["timestamp"] = Value::String(now_iso());
        }
        for mut ev in record_to_events(&rec, turn) {
            if ev.kind == EventKind::User {
                continue; // our own prompt echo; we record it ourselves
            }
            if let Some(tool) = &ev.tool {
                tool_names.insert(tool.id.clone(), tool.name.clone());
            }
            if let Some(r) = ev.result.as_mut() {
                r.name = tool_names.get(&r.id).cloned();
            }
            on_event(&ev);
            res.events.push(ev);
        }
    }
    let status = child.wait()?;
    res.stderr = err_thread.join().unwrap_or_default();
    if let Some(r) = &result_rec {
        res.cost_usd = r.get("total_cost_usd").and_then(Value::as_f64);
        res.is_error = r.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        res.model = r.get("modelUsage").and_then(Value::as_object).and_then(|m| m.keys().next().cloned());
    }
    if res.model.is_none() {
        res.model = res.events.iter().find_map(|e| e.model.clone());
    }
    if !status.success() || result_rec.is_none() {
        res.is_error = true;
        let ev = Event::text(turn, now_iso(), EventKind::Error, format!("claude exited with {status}{}: {}", if result_rec.is_none() { " without a result record" } else { "" }, truncate(res.stderr.trim(), 2000)));
        on_event(&ev);
        res.events.push(ev);
    }
    Ok(res)
}

pub fn detect(rec: &Value) -> bool {
    rec.is_object() && ((rec.get("sessionId").is_some() && rec.get("type").is_some()) || rec.get("parentUuid").is_some())
}

/// Claude Code's project directory name for a working directory (every non-alphanumeric byte → '-').
pub fn project_slug(cwd: &Path) -> String {
    cwd.display().to_string().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Fork a recorded session at `up_to_turn`: write a transcript holding every record before the user
/// message that starts that turn, under a new session id and the given working directory, where
/// `claude --resume <new_id>` will find it. Returns the new transcript path.
pub fn prepare_fork(original: &Session, up_to_turn: u32, new_id: &str, cwd: &Path) -> Result<PathBuf> {
    let src = original.path.as_deref().context("original session has no on-disk path to fork from")?;
    let records = read_jsonl(Path::new(src))?;
    let mut kept: Vec<Value> = Vec::new();
    let mut turn = 0u32;
    let cwd_s = cwd.display().to_string();
    for rec in records {
        let t = rec.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(t, "queue-operation" | "last-prompt" | "atis-latch") {
            continue;
        }
        if t == "user" || t == "assistant" {
            let starts_turn = record_to_events(&rec, turn.max(1)).iter().any(|e| e.kind == EventKind::User && !e.sidechain);
            if starts_turn {
                turn += 1;
                if turn >= up_to_turn {
                    break;
                }
            }
        }
        let mut rec = rec;
        if let Some(obj) = rec.as_object_mut() {
            if obj.contains_key("sessionId") {
                obj.insert("sessionId".into(), Value::String(new_id.into()));
            }
            if obj.contains_key("cwd") {
                obj.insert("cwd".into(), Value::String(cwd_s.clone()));
            }
        }
        kept.push(rec);
    }
    if turn + 1 < up_to_turn {
        bail!("session has only {turn} turn(s) before turn {up_to_turn}");
    }
    let dir = config_dir().join("projects").join(project_slug(cwd));
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("{new_id}.jsonl"));
    let mut out = String::new();
    for r in &kept {
        out.push_str(&r.to_string());
        out.push('\n');
    }
    std::fs::write(&dest, out)?;
    Ok(dest)
}
