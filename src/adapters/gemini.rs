//! Google Gemini CLI adapter.
//!
//! Sessions: `$GEMINI_CLI_HOME` (default `~/.gemini`)`/tmp/<project>/chats/session-<ts>-<id8>.jsonl`
//! (legacy `.json`). The first JSONL line is session metadata (`sessionId`, `projectHash`, `startTime`),
//! later lines are message records (`{id, timestamp, type: user|gemini|info|error|warning, content,
//! toolCalls?, thoughts?, tokens?, model?}`), `{"$set": {...}}` metadata updates (which may re-send
//! messages; records are upserted by id), and `{"$rewindTo": id}`. The project directory holds a
//! `.project_root` file with the workspace path; older layouts name the directory by a SHA-256 of
//! that path, which `projects.json` lets us invert.
//! Rerun: `gemini -p … --output-format stream-json --approval-mode yolo`, resumed with `--resume <id>`.
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{RunOpts, RunResult, SessionSummary};
use crate::model::{Event, EventKind, Harness, Session, Usage};
use crate::util::{clean_command, first_line, home_dir, now_iso, read_jsonl, read_jsonl_head, truncate, walk};

pub fn gemini_home() -> PathBuf {
    std::env::var_os("GEMINI_CLI_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".gemini"))
}

const INJECTED_PREFIXES: &[&str] = &["<session_context>", "<environment_context>", "<user_instructions>"];

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Text of a `PartListUnion` (string, part, or array of parts).
pub fn parts_text(v: Option<&Value>) -> String {
    fn part(p: &Value, out: &mut Vec<String>) {
        match p {
            Value::String(t) => out.push(t.clone()),
            Value::Array(items) => items.iter().for_each(|i| part(i, out)),
            Value::Object(_) => {
                if let Some(t) = s(p, "text") {
                    out.push(t.to_string());
                } else if let Some(fr) = p.get("functionResponse") {
                    let resp = fr.get("response").unwrap_or(fr);
                    if let Some(o) = resp.get("output").and_then(Value::as_str) {
                        out.push(o.to_string());
                    } else if let Some(e) = resp.get("error").and_then(Value::as_str) {
                        out.push(e.to_string());
                    } else {
                        out.push(resp.to_string());
                    }
                } else if p.get("inlineData").is_some() || p.get("fileData").is_some() {
                    out.push("[binary part]".into());
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(v) = v {
        part(v, &mut out);
    }
    out.join("\n")
}

fn is_injected(text: &str) -> bool {
    let t = text.trim_start();
    INJECTED_PREFIXES.iter().any(|p| t.starts_with(p))
}

/// Resolve the workspace path for a project directory under `tmp/`.
fn project_cwd(project_dir: &Path, project_hash: Option<&str>) -> Option<String> {
    if let Ok(root) = std::fs::read_to_string(project_dir.join(".project_root")) {
        let root = root.trim();
        if !root.is_empty() {
            return Some(root.to_string());
        }
    }
    let projects: Value = std::fs::read_to_string(gemini_home().join("projects.json")).ok().and_then(|t| serde_json::from_str(&t).ok())?;
    let map = projects.get("projects").and_then(Value::as_object)?;
    let dir_name = project_dir.file_name()?.to_str()?;
    for (path, name) in map {
        if name.as_str() == Some(dir_name) {
            return Some(path.clone());
        }
        if let Some(h) = project_hash {
            use sha2::{Digest, Sha256};
            let digest = format!("{:x}", Sha256::digest(path.as_bytes()));
            if digest == h || digest == dir_name {
                return Some(path.clone());
            }
        }
    }
    None
}

/// Replay JSONL records (or a legacy single JSON object) into the ordered message list plus metadata.
fn collect(records: &[Value]) -> (Value, Vec<Value>) {
    let mut meta = serde_json::Map::new();
    let mut order: Vec<String> = Vec::new();
    let mut msgs: HashMap<String, Value> = HashMap::new();
    let upsert = |m: &Value, order: &mut Vec<String>, msgs: &mut HashMap<String, Value>| {
        let Some(id) = s(m, "id") else { return };
        if !msgs.contains_key(id) {
            order.push(id.to_string());
        }
        msgs.insert(id.to_string(), m.clone());
    };
    for rec in records {
        let Some(obj) = rec.as_object() else { continue };
        if let Some(target) = s(rec, "$rewindTo") {
            if let Some(pos) = order.iter().position(|id| id == target) {
                for id in order.drain(pos + 1..) {
                    msgs.remove(&id);
                }
            }
            continue;
        }
        if let Some(set) = rec.get("$set").and_then(Value::as_object) {
            for (k, v) in set {
                if k == "messages" {
                    for m in v.as_array().into_iter().flatten() {
                        upsert(m, &mut order, &mut msgs);
                    }
                } else {
                    meta.insert(k.clone(), v.clone());
                }
            }
            continue;
        }
        if obj.contains_key("sessionId") || obj.contains_key("projectHash") {
            for (k, v) in obj {
                if k == "messages" {
                    for m in v.as_array().into_iter().flatten() {
                        upsert(m, &mut order, &mut msgs);
                    }
                } else {
                    meta.insert(k.clone(), v.clone());
                }
            }
            continue;
        }
        if obj.contains_key("id") && obj.contains_key("type") {
            upsert(rec, &mut order, &mut msgs);
        }
    }
    (Value::Object(meta), order.into_iter().filter_map(|id| msgs.remove(&id)).collect())
}

fn map_usage(t: &Value) -> Usage {
    let n = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage { input: n("input"), output: n("output"), cache_read: n("cached"), cache_write: 0, reasoning: n("thoughts") }
}

pub fn parse_records(records: &[Value], file: Option<&Path>) -> Session {
    let (meta, messages) = collect(records);
    let mut session = Session::new(Harness::Gemini);
    session.path = file.map(|p| p.display().to_string());
    session.id = s(&meta, "sessionId").unwrap_or("").to_string();
    session.started_at = s(&meta, "startTime").map(String::from);
    session.ended_at = s(&meta, "lastUpdated").map(String::from);
    if let Some(dirs) = meta.get("directories").and_then(Value::as_array) {
        session.cwd = dirs.first().and_then(Value::as_str).map(String::from);
    }
    if session.cwd.is_none() {
        if let Some(project_dir) = file.and_then(|f| f.parent()).and_then(|chats| chats.parent()) {
            session.cwd = project_cwd(project_dir, s(&meta, "projectHash"));
        }
    }
    let mut turn = 0u32;
    for m in &messages {
        let ts = s(m, "timestamp").unwrap_or("").to_string();
        let cur = turn.max(1);
        match s(m, "type").unwrap_or("") {
            "user" => {
                let text = parts_text(m.get("content"));
                if text.trim().is_empty() || is_injected(&text) {
                    continue;
                }
                if text.trim_start().starts_with('/') && !text.contains('\n') {
                    session.events.push(Event::system(cur, ts, "command", text.trim()));
                    continue;
                }
                turn += 1;
                session.events.push(Event::text(turn, ts, EventKind::User, text));
            }
            "gemini" => {
                if session.model.is_none() {
                    session.model = s(m, "model").map(String::from);
                }
                for th in m.get("thoughts").and_then(Value::as_array).into_iter().flatten() {
                    let subject = s(th, "subject").unwrap_or("");
                    let desc = s(th, "description").unwrap_or("");
                    let text = if subject.is_empty() { desc.to_string() } else { format!("**{subject}** {desc}") };
                    let mut e = Event::text(cur, s(th, "timestamp").unwrap_or(&ts).to_string(), EventKind::Thinking, text);
                    e.msg_id = s(m, "id").map(String::from);
                    session.events.push(e);
                }
                let text = parts_text(m.get("content"));
                let mut usage_attached = false;
                if !text.trim().is_empty() {
                    let mut e = Event::text(cur, ts.clone(), EventKind::Assistant, text);
                    e.model = s(m, "model").map(String::from).or_else(|| session.model.clone());
                    e.msg_id = s(m, "id").map(String::from);
                    e.usage = m.get("tokens").filter(|t| t.is_object()).map(map_usage);
                    usage_attached = e.usage.is_some();
                    session.events.push(e);
                }
                for tc in m.get("toolCalls").and_then(Value::as_array).into_iter().flatten() {
                    let id = s(tc, "id").unwrap_or("").to_string();
                    let name = s(tc, "name").unwrap_or("tool").to_string();
                    let tts = s(tc, "timestamp").unwrap_or(&ts).to_string();
                    let mut call = Event::tool_call(cur, tts.clone(), id.clone(), name.clone(), tc.get("args").cloned().unwrap_or_else(|| serde_json::json!({})));
                    if !usage_attached {
                        call.usage = m.get("tokens").filter(|t| t.is_object()).map(map_usage);
                        call.msg_id = s(m, "id").map(String::from);
                        usage_attached = call.usage.is_some();
                    }
                    session.events.push(call);
                    let status = s(tc, "status").unwrap_or("");
                    let mut out = parts_text(tc.get("result"));
                    if out.is_empty() {
                        if let Some(d) = tc.get("resultDisplay").and_then(Value::as_str) {
                            out = d.to_string();
                        }
                    }
                    session.events.push(Event::tool_result(cur, tts, id, Some(name), out, matches!(status, "error" | "cancelled" | "failed")));
                }
            }
            "error" => session.events.push(Event::text(cur, ts, EventKind::Error, parts_text(m.get("content")))),
            "info" | "warning" => session.events.push(Event::system(cur, ts, s(m, "type").unwrap_or("info"), parts_text(m.get("content")))),
            _ => {}
        }
    }
    if session.id.is_empty() {
        if let Some(f) = file {
            session.id = f.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
        }
    }
    if let Some(u) = session.events.iter().find(|e| e.kind == EventKind::User) {
        session.title = Some(truncate(first_line(u.text_str()), 80));
    }
    session
}

pub fn parse_file(file: &Path) -> Result<Session> {
    let records = if file.extension().is_some_and(|e| e == "json") {
        let text = std::fs::read_to_string(file)?;
        vec![serde_json::from_str::<Value>(&text).with_context(|| format!("parsing {}", file.display()))?]
    } else {
        read_jsonl(file)?
    };
    Ok(parse_records(&records, Some(file)))
}

fn session_files() -> Vec<PathBuf> {
    let root = gemini_home().join("tmp");
    let mut files = Vec::new();
    // top-level chats/session-*.json[l] only; nested directories hold subagent transcripts
    walk(
        &root,
        &|p: &Path| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with("session-") && (name.ends_with(".jsonl") || name.ends_with(".json")) && p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()) == Some("chats")
        },
        &mut files,
    );
    files
}

pub fn list_sessions() -> Vec<SessionSummary> {
    let mut out = Vec::new();
    for file in session_files() {
        let Ok(meta) = std::fs::metadata(&file) else { continue };
        let records = if file.extension().is_some_and(|e| e == "json") {
            std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| vec![v]).unwrap_or_default()
        } else {
            read_jsonl_head(&file, 512 * 1024).unwrap_or_default()
        };
        let (m, messages) = collect(&records);
        let Some(first) = messages.iter().find(|x| s(x, "type") == Some("user") && !is_injected(&parts_text(x.get("content")))) else { continue };
        let prompt = parts_text(first.get("content"));
        if prompt.trim().is_empty() {
            continue;
        }
        let project_dir = file.parent().and_then(|c| c.parent());
        let updated: chrono::DateTime<chrono::Utc> = meta.modified().map(Into::into).unwrap_or_else(|_| chrono::Utc::now());
        out.push(SessionSummary {
            harness: Harness::Gemini,
            id: s(&m, "sessionId").unwrap_or("").to_string(),
            cwd: project_dir.and_then(|d| project_cwd(d, s(&m, "projectHash"))),
            git_branch: None,
            started_at: s(&m, "startTime").map(String::from),
            updated_at: updated.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            title: truncate(first_line(&prompt), 80),
            size_bytes: meta.len(),
            path: file,
        });
    }
    out
}

pub fn find_log_by_id(id: &str) -> Option<PathBuf> {
    let short: String = id.chars().take(8).collect();
    let mut candidates: Vec<PathBuf> = session_files().into_iter().filter(|p| p.to_string_lossy().contains(&short)).collect();
    candidates.sort();
    candidates.into_iter().rev().find(|p| read_jsonl_head(p, 65536).ok().and_then(|h| h.iter().find_map(|r| s(r, "sessionId").map(String::from))).as_deref() == Some(id))
}

/// Run one user turn through `gemini -p … --output-format stream-json`.
pub fn run_turn(opts: &RunOpts, on_event: &mut dyn FnMut(&Event)) -> Result<RunResult> {
    let bin = opts.bin.clone().or_else(|| std::env::var("CASIMIR_GEMINI_BIN").ok()).unwrap_or_else(|| "gemini".into());
    let mut args: Vec<String> = vec!["-p".into(), opts.prompt.clone(), "--output-format".into(), "stream-json".into(), "--approval-mode".into(), "yolo".into()];
    if let Some(m) = &opts.model {
        args.extend(["-m".into(), m.clone()]);
    }
    if let Some(r) = &opts.resume {
        args.extend(["--resume".into(), r.clone()]);
    }
    args.extend(opts.extra_args.iter().cloned());
    let mut cmd = Command::new(&bin);
    cmd.args(&args).env("GEMINI_CLI_TRUST_WORKSPACE", "true").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    clean_command(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("spawning {bin}"))?;
    let stderr = child.stderr.take().context("stderr")?;
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        s
    });
    let turn = opts.turn.max(1);
    let mut res = RunResult { session_id: opts.resume.clone(), model: opts.model.clone(), ..Default::default() };
    let mut buffer = String::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let flush = |buffer: &mut String, res: &mut RunResult, on_event: &mut dyn FnMut(&Event)| {
        if buffer.trim().is_empty() {
            buffer.clear();
            return;
        }
        let mut e = Event::text(turn, now_iso(), EventKind::Assistant, buffer.clone());
        e.model = res.model.clone();
        on_event(&e);
        res.events.push(e);
        buffer.clear();
    };
    let stdout = child.stdout.take().context("stdout")?;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        let t = line.trim();
        if !t.starts_with('{') {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<Value>(t) else { continue };
        res.raw.push(rec.clone());
        let ts = s(&rec, "timestamp").map(String::from).unwrap_or_else(now_iso);
        match s(&rec, "type").unwrap_or("") {
            "init" => {
                res.session_id = s(&rec, "session_id").map(String::from);
                if let Some(m) = s(&rec, "model") {
                    res.model = Some(m.to_string());
                }
            }
            "message" => {
                if s(&rec, "role") == Some("assistant") {
                    buffer.push_str(s(&rec, "content").unwrap_or(""));
                }
            }
            "tool_use" => {
                flush(&mut buffer, &mut res, on_event);
                let id = s(&rec, "tool_id").unwrap_or("").to_string();
                let name = s(&rec, "tool_name").unwrap_or("tool").to_string();
                tool_names.insert(id.clone(), name.clone());
                let e = Event::tool_call(turn, ts, id, name, rec.get("parameters").cloned().unwrap_or_else(|| serde_json::json!({})));
                on_event(&e);
                res.events.push(e);
            }
            "tool_result" => {
                let id = s(&rec, "tool_id").unwrap_or("").to_string();
                let is_error = s(&rec, "status") == Some("error");
                let out = s(&rec, "output").map(String::from).or_else(|| rec.get("error").and_then(|e| s(e, "message")).map(String::from)).unwrap_or_default();
                let e = Event::tool_result(turn, ts, id.clone(), tool_names.get(&id).cloned(), out, is_error);
                on_event(&e);
                res.events.push(e);
            }
            "error" => {
                flush(&mut buffer, &mut res, on_event);
                let msg = s(&rec, "message").unwrap_or("error").to_string();
                let e = if s(&rec, "severity") == Some("warning") { Event::system(turn, ts, "warning", msg) } else { Event::text(turn, ts, EventKind::Error, msg) };
                on_event(&e);
                res.events.push(e);
            }
            "result" => {
                flush(&mut buffer, &mut res, on_event);
                if let Some(stats) = rec.get("stats") {
                    let n = |k: &str| stats.get(k).and_then(Value::as_u64).unwrap_or(0);
                    res.usage = Some(Usage { input: n("input"), output: n("output_tokens"), cache_read: n("cached"), cache_write: 0, reasoning: 0 });
                }
                if s(&rec, "status") == Some("error") {
                    res.is_error = true;
                }
            }
            _ => {}
        }
    }
    flush(&mut buffer, &mut res, on_event);
    let status = child.wait()?;
    res.stderr = err_thread.join().unwrap_or_default();
    let completed = res.session_id.as_deref().is_some_and(|s| !s.is_empty())
        && res.raw.iter().any(|r| s(r, "type") == Some("result") && s(r, "status") == Some("success"));
    if res.is_error || !status.success() || !completed || res.events.iter().any(|e| e.kind == EventKind::Error) {
        res.is_error = true;
        if !res.events.iter().any(|e| e.kind == EventKind::Error) {
            let msg = crate::util::stderr_error_line(&res.stderr, "gemini ended without a successful result");
            let e = Event::text(turn, now_iso(), EventKind::Error, msg);
            on_event(&e);
            res.events.push(e);
        }
    }
    Ok(res)
}

pub fn detect(rec: &Value) -> bool {
    rec.is_object() && rec.get("sessionId").is_some() && rec.get("projectHash").is_some()
}
