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
    pub timeout_secs: Option<u64>,
    pub spool: Option<PathBuf>,
    pub allow_unrestricted: bool,
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

/// Prepare a harness-native fork of `original` at `up_to_turn` (the forked session then resumes
/// with the turn-`up_to_turn` message). Returns the path of the transcript written for the harness.
pub fn prepare_fork(h: Harness, original: &Session, up_to_turn: u32, new_id: &str, cwd: &Path) -> Result<PathBuf> {
    match h {
        Harness::ClaudeCode => claude_code::prepare_fork(original, up_to_turn, new_id, cwd),
        Harness::Codex => codex::prepare_fork(original, up_to_turn, new_id, cwd),
        Harness::Copilot | Harness::Gemini => bail!("fork-at-turn is not supported for {h}: its CLI cannot resume a truncated transcript"),
    }
}

pub fn run_turn(h: Harness, opts: &RunOpts, on_event: &mut dyn FnMut(&Event)) -> Result<RunResult> {
    validate_permissions(opts)?;
    if h == Harness::Codex && opts.extra_args.iter().any(|arg| {
        let lower = arg.to_ascii_lowercase();
        lower == "--oss" || lower == "--local-provider" || lower.contains("forced_login_method") || lower.contains("model_provider")
    }) { bail!("Codex subscription runs cannot override the provider or login method through passthrough arguments"); }
    if matches!(h, Harness::ClaudeCode | Harness::Codex) && !crate::doctor::subscription_ready(h) {
        bail!("{} subscription login is required: run `{}` and check `casimir doctor --json`", h,
            if h == Harness::Codex { "codex login" } else { "claude auth login" });
    }
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

/// Permission overrides never derive from the choice of working directory.
pub fn validate_permissions(opts: &RunOpts) -> Result<()> {
    let dangerous = ["bypass", "bypassPermissions", "danger-full-access", "yolo", "--dangerously-skip-permissions", "--dangerously-bypass-approvals-and-sandbox", "--allow-all-tools", "--yolo"];
    let explicit = opts.permission_mode.iter().chain(opts.sandbox.iter()).chain(opts.extra_args.iter())
        .any(|v| dangerous.iter().any(|d| v == d || v.split(['=', ' ', '\"', '\'']).any(|p| p == *d)));
    if explicit && !opts.allow_unrestricted {
        anyhow::bail!("unrestricted harness execution requires --allow-unrestricted (also for passthrough arguments)");
    }
    // Never persist or echo inline credentials supplied through passthrough arguments.
    if opts.extra_args.iter().any(|a| {
        let a = a.to_ascii_lowercase();
        ["api_key", "api-key", "auth_token", "auth-token", "authorization", "bearer "].iter().any(|key| a.contains(key))
    }) { anyhow::bail!("credentials must be supplied through the harness environment or credential store, not command-line arguments"); }
    Ok(())
}

/// Restore exactly the recorded native conversation; no inferred historical cutoff.
pub fn restore_conversation(checkpoint: &crate::checkpoint::Checkpoint, new_id: &str, cwd: &Path) -> Result<PathBuf> {
    crate::checkpoint::require_compatible(checkpoint)?;
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("native.jsonl");
    crate::checkpoint::native_copy(checkpoint, &source)?;
    let mut records = crate::util::read_jsonl(&source)?;
    for rec in &mut records {
        match checkpoint.harness {
            Harness::ClaudeCode => {
                if let Some(obj) = rec.as_object_mut() {
                    if obj.contains_key("sessionId") { obj.insert("sessionId".into(), new_id.into()); }
                    if obj.contains_key("cwd") { obj.insert("cwd".into(), cwd.display().to_string().into()); }
                }
            },
            Harness::Codex => {
                let meta = rec.get("type").and_then(Value::as_str) == Some("session_meta");
                if let Some(payload) = rec.get_mut("payload").and_then(Value::as_object_mut) {
                    if meta { payload.insert("id".into(), new_id.into()); payload.insert("session_id".into(), new_id.into()); }
                    if meta || payload.contains_key("cwd") { payload.insert("cwd".into(), cwd.display().to_string().into()); }
                }
            },
            _ => bail!("native checkpoint continuation is unsupported for experimental harnesses"),
        }
    }
    let destination = match checkpoint.harness {
        Harness::ClaudeCode => claude_code::config_dir().join("projects").join(claude_code::project_slug(cwd)).join(format!("{new_id}.jsonl")),
        Harness::Codex => {
            let now = chrono::Utc::now();
            codex::codex_home().join("sessions").join(now.format("%Y/%m/%d").to_string()).join(format!("rollout-{}-{new_id}.jsonl", now.format("%Y-%m-%dT%H-%M-%S")))
        },
        _ => unreachable!(),
    };
    let mut bytes = Vec::new();
    for record in records { serde_json::to_writer(&mut bytes, &record)?; bytes.push(b'\n'); }
    if destination.exists() { bail!("refusing to overwrite an existing native conversation"); }
    crate::util::atomic_write(&destination, &bytes)?;
    Ok(destination)
}

/// Bound the in-memory normalized turn; the supervisor retains the complete byte stream.
pub fn retain_raw(result: &mut RunResult, record: &Value, bytes: &mut usize) -> Result<()> {
    *bytes = bytes.saturating_add(serde_json::to_vec(record)?.len());
    if *bytes > 64 * 1024 * 1024 { bail!("turn exceeds 64 MiB normalization limit; raw stream retained on disk"); }
    result.raw.push(record.clone());
    Ok(())
}
