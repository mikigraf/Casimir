//! Harness adapters: each knows where its logs live, how to normalize them, and how to run a turn.
pub mod claude_code;
pub mod codex;
pub mod copilot;
pub mod gemini;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::model::{Event, Harness, Session, Usage};
use crate::util::{read_json, read_jsonl_head, read_prefix};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub harness: Harness,
    pub id: String,
    pub path: PathBuf,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: String,
    pub title: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RunOpts {
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    /// New session id (Claude Code) for the first turn.
    pub session_id: Option<String>,
    /// Existing session / thread id to resume for later turns.
    pub resume: Option<String>,
    pub permission_mode: Option<String>,
    pub sandbox: Option<String>,
    pub extra_args: Vec<String>,
    pub turn: u32,
    pub bin: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RunResult {
    pub session_id: Option<String>,
    pub events: Vec<Event>,
    pub raw: Vec<Value>,
    pub usage: Option<Usage>,
    pub cost_usd: Option<f64>,
    pub is_error: bool,
    pub model: Option<String>,
    pub stderr: String,
}

pub fn parse_file(h: Harness, path: &Path) -> Result<Session> {
    match h {
        Harness::ClaudeCode => claude_code::parse_file(path),
        Harness::Codex => codex::parse_file(path),
        Harness::Copilot => copilot::parse_file(path),
        Harness::Gemini => gemini::parse_file(path),
    }
}

pub fn find_log_by_id(h: Harness, id: &str) -> Option<PathBuf> {
    match h {
        Harness::ClaudeCode => claude_code::find_log_by_id(id),
        Harness::Codex => codex::find_log_by_id(id),
        Harness::Copilot => copilot::find_log_by_id(id),
        Harness::Gemini => gemini::find_log_by_id(id),
    }
}

pub fn run_turn(h: Harness, opts: &RunOpts, on_event: &mut dyn FnMut(&Event)) -> Result<RunResult> {
    match h {
        Harness::ClaudeCode => claude_code::run_turn(opts, on_event),
        Harness::Codex => codex::run_turn(opts, on_event),
        Harness::Copilot => copilot::run_turn(opts, on_event),
        Harness::Gemini => gemini::run_turn(opts, on_event),
    }
}

/// All known sessions across harnesses, newest first.
pub fn list_all_sessions(harness: Option<Harness>) -> Vec<SessionSummary> {
    let mut out = Vec::new();
    for h in Harness::all() {
        if harness.is_some_and(|w| w != h) {
            continue;
        }
        let items = match h {
            Harness::ClaudeCode => claude_code::list_sessions(),
            Harness::Codex => codex::list_sessions(),
            Harness::Copilot => copilot::list_sessions(),
            Harness::Gemini => gemini::list_sessions(),
        };
        out.extend(items);
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

/// Load a session from a path: raw harness log, casimir run dir, or normalized session.json.
pub fn load_session_file(file: &Path) -> Result<Session> {
    let meta = std::fs::metadata(file).with_context(|| format!("{}", file.display()))?;
    if meta.is_dir() {
        let sj = file.join("session.json");
        if sj.exists() {
            return load_session_file(&sj);
        }
        if file.join("events.jsonl").exists() || file.join("workspace.yaml").exists() {
            return copilot::parse_dir(file);
        }
        bail!("{} is not a casimir run directory (no session.json) or a Copilot session directory", file.display());
    }
    if file.extension().is_some_and(|e| e == "json") {
        let v: Value = read_json(file).with_context(|| format!("parsing {}", file.display()))?;
        if v.get("harness").is_some() && v.get("events").is_some() {
            return serde_json::from_value(v).with_context(|| format!("{} is not a casimir session export", file.display()));
        }
        if gemini::detect(&v) {
            return gemini::parse_file(file);
        }
        bail!("{} is not a casimir session export", file.display());
    }
    // Codex session_meta lines can exceed 100KB (they embed the base instructions), so read generously.
    let head = read_jsonl_head(file, 1024 * 1024)?;
    if head.iter().any(gemini::detect) {
        return gemini::parse_file(file);
    }
    if head.iter().any(copilot::detect) {
        return copilot::parse_file(file);
    }
    if head.iter().any(codex::detect) {
        return codex::parse_file(file);
    }
    if head.iter().any(claude_code::detect) {
        return claude_code::parse_file(file);
    }
    let prefix = read_prefix(file, 4096)?;
    if prefix.contains("\"type\":\"session_meta\"") || prefix.contains("\"payload\":") {
        return codex::parse_file(file);
    }
    if prefix.contains("\"sessionId\":") || prefix.contains("\"parentUuid\":") {
        return claude_code::parse_file(file);
    }
    bail!("cannot determine session format of {}", file.display())
}

/// Resolve a user-supplied session reference: a path, `last`, `claude:last`, `codex:last`,
/// a session id, or a unique id prefix (optionally `codex:<prefix>`).
pub fn resolve_session(reference: &str) -> Result<Session> {
    if reference.is_empty() {
        bail!("session reference required (path, id, id prefix, or last)");
    }
    let p = Path::new(reference);
    if p.exists() {
        return load_session_file(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
    }
    let mut harness = None;
    let mut key = reference;
    if let Some((h, k)) = reference.split_once(':') {
        if let Ok(hh) = Harness::parse(h) {
            harness = Some(hh);
            key = k;
        }
    }
    let all = list_all_sessions(harness);
    if key == "last" || key == "latest" {
        let Some(first) = all.first() else {
            bail!("no {} sessions found", harness.map(|h| h.to_string()).unwrap_or_default());
        };
        return load_session_file(&first.path);
    }
    let hits: Vec<&SessionSummary> = all
        .iter()
        .filter(|s| s.id == key || s.id.starts_with(key) || s.path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(key)))
        .collect();
    match hits.len() {
        1 => load_session_file(&hits[0].path),
        0 => bail!("session not found: {reference}"),
        _ => {
            let exact: Vec<&&SessionSummary> = hits.iter().filter(|s| s.id == key).collect();
            if exact.len() == 1 {
                return load_session_file(&exact[0].path);
            }
            bail!("ambiguous session \"{reference}\": {}", hits.iter().map(|h| format!("{}:{}", h.harness, h.id)).collect::<Vec<_>>().join(", "))
        }
    }
}
