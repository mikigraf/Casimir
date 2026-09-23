//! Small LLM client used by the user simulator and the judge.
//!
//! Backends:
//!   api        — Anthropic Messages API over HTTPS in process;
//!                credentials from ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, or an `ant auth login` profile
//!   claude-cli — `claude -p` with tools disabled; reuses the local Claude Code login
//!   auto       — api when Anthropic credentials are visible, otherwise claude-cli
//!   cmd        — run $CASIMIR_LLM_CMD with the prompt on stdin and the system prompt in
//!                $CASIMIR_LLM_SYSTEM; the reply is its stdout (for tests and custom gateways)
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::process::Command;
use std::time::Duration;
use serde::{Serialize, Deserialize};

use crate::util::{clean_command, extract_json, home_dir};

pub const DEFAULT_MODEL: &str = "claude-opus-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmOpts {
    pub model: Option<String>,
    pub backend: String,
    pub max_tokens: u64,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub recording_dir: Option<std::path::PathBuf>,
}

impl Default for LlmOpts {
    fn default() -> Self {
        LlmOpts { model: None, backend: "auto".into(), max_tokens: 16000, timeout_secs: 300, recording_dir: None }
    }
}

fn default_timeout() -> u64 { 300 }

struct Completion { text: String, usage: Option<Value>, cost_usd: Option<f64>, model: Option<String> }

enum Credential {
    ApiKey(String),
    Bearer(String),
    OAuth(String),
}

fn has_api_credentials() -> bool {
    std::env::var_os("ANTHROPIC_API_KEY").is_some() || std::env::var_os("ANTHROPIC_AUTH_TOKEN").is_some() || home_dir().join(".config/anthropic").exists()
}

pub fn pick_backend(requested: &str) -> String {
    if requested != "auto" && !requested.is_empty() {
        return requested.to_string();
    }
    if has_api_credentials() {
        "api".into()
    } else {
        "claude-cli".into()
    }
}

fn resolve_auth() -> Result<Credential> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Ok(Credential::ApiKey(k));
        }
    }
    if let Ok(t) = std::env::var("ANTHROPIC_AUTH_TOKEN") {
        if !t.is_empty() {
            return Ok(Credential::Bearer(t));
        }
    }
    // `ant auth login` profile: short-lived token, sent as Bearer with the oauth beta header
    let credential_output = tempfile::tempdir()?;
    if let Ok(out) = crate::process::capture(Command::new("ant").args(["auth", "print-credentials", "--access-token"]), b"", Duration::from_secs(30), Some(&credential_output.path().join("auth"))) {
        let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !tok.is_empty() {
            return Ok(Credential::OAuth(tok));
        }
    }
    bail!("no Anthropic credentials: set ANTHROPIC_API_KEY, run `ant auth login`, or use --llm claude-cli")
}

fn complete_api(system: &str, prompt: &str, model: &str, max_tokens: u64, timeout: u64, spool: &std::path::Path) -> Result<Completion> {
    let auth = resolve_auth()?;
    let body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": prompt }],
        "fallbacks": "default",
    });
    let mut betas = vec!["server-side-fallback-2026-07-01"];
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(timeout)).redirects(0).build();
    let mut request = agent.post(API_URL).set("content-type", "application/json").set("anthropic-version", "2023-06-01");
    match &auth {
        Credential::ApiKey(k) => request = request.set("x-api-key", k),
        Credential::Bearer(t) => request = request.set("Authorization", &format!("Bearer {t}")),
        Credential::OAuth(t) => {
            betas.push("oauth-2025-04-20");
            request = request.set("Authorization", &format!("Bearer {t}"));
        }
    }
    let response = request.set("anthropic-beta", &betas.join(",")).send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(status, _) => anyhow::anyhow!("Anthropic API HTTP {status} (response omitted for privacy)"),
            ureq::Error::Transport(_) => anyhow::anyhow!("Anthropic API transport failed or timed out"),
        })?;
    let mut bytes = Vec::new();
    response.into_reader().take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 { bail!("API response exceeds 4 MiB"); }
    let resp: Value = serde_json::from_slice(&bytes).context("parsing API response")?;
    crate::util::write_json(&spool.join("response.json"), &resp)?;
    if resp.get("type").and_then(Value::as_str) == Some("error") {
        bail!("API error: {}", resp.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("unknown"));
    }
    if resp.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        let d = resp.get("stop_details").cloned().unwrap_or(Value::Null);
        bail!("model refused ({}): {}", d.get("category").and_then(Value::as_str).unwrap_or("unknown"), d.get("explanation").and_then(Value::as_str).unwrap_or(""));
    }
    let text = resp
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("text")).filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    Ok(Completion { text, usage: resp.get("usage").cloned(), cost_usd: None, model: resp.get("model").and_then(Value::as_str).map(String::from) })
}

fn complete_cli(system: &str, prompt: &str, model: &str, timeout: u64, spool: &std::path::Path) -> Result<Completion> {
    let bin = std::env::var("CASIMIR_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
    let mut cmd = Command::new(&bin);
    cmd.args(["-p", "--output-format", "json", "--tools", "", "--no-session-persistence", "--model", model, "--system-prompt", system]);
    clean_command(&mut cmd);
    let out = crate::process::capture(&mut cmd, prompt.as_bytes(), Duration::from_secs(timeout), Some(&spool.join("process")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!("claude -p exited with {}: {}", out.status, out.stderr.trim());
    }
    let last = stdout.trim().lines().last().unwrap_or("");
    let parsed: Value = serde_json::from_str(last).with_context(|| format!("claude -p returned no JSON ({}): {}", out.status, out.stderr.trim()))?;
    if parsed.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        bail!("claude -p error: {}", parsed.get("result").and_then(Value::as_str).unwrap_or(""));
    }
    Ok(Completion { text: parsed.get("result").and_then(Value::as_str).unwrap_or("").to_string(), usage: parsed.get("usage").cloned(), cost_usd: parsed.get("total_cost_usd").and_then(Value::as_f64), model: parsed.get("model").and_then(Value::as_str).map(String::from) })
}

fn complete_cmd(system: &str, prompt: &str, model: &str, timeout: u64, spool: &std::path::Path) -> Result<Completion> {
    let bin = std::env::var("CASIMIR_LLM_CMD").context("--llm cmd needs CASIMIR_LLM_CMD")?;
    let mut cmd = Command::new(&bin);
    cmd.env("CASIMIR_LLM_SYSTEM", system).env("CASIMIR_LLM_MODEL", model);
    let out = crate::process::capture(&mut cmd, prompt.as_bytes(), Duration::from_secs(timeout), Some(&spool.join("process")))?;
    if !out.status.success() {
        bail!("{bin} failed: {}", out.stderr.trim());
    }
    Ok(Completion { text: String::from_utf8_lossy(&out.stdout).into_owned(), usage: None, cost_usd: None, model: None })
}

/// Effective model name for the selected backend (what will be recorded in reports).
pub fn effective_model(o: &LlmOpts) -> String {
    o.model.clone().unwrap_or_else(|| DEFAULT_MODEL.into())
}

/// Text completion through the selected backend.
pub fn complete(system: &str, prompt: &str, o: &LlmOpts) -> Result<String> {
    let model = effective_model(o);
    let backend = pick_backend(&o.backend);
    let spool = o.recording_dir.clone().unwrap_or_else(|| crate::util::casimir_home().join("llm")).join(uuid::Uuid::new_v4().to_string());
    crate::util::private_dir(&spool)?;
    let start = std::time::Instant::now();
    let result = match backend.as_str() {
        "api" => complete_api(system, prompt, &model, o.max_tokens, o.timeout_secs, &spool),
        "claude-cli" => complete_cli(system, prompt, &model, o.timeout_secs, &spool),
        "cmd" => complete_cmd(system, prompt, &model, o.timeout_secs, &spool),
        other => bail!("unknown llm backend {other}"),
    };
    crate::util::write_json(&spool.join("call.json"), &json!({"schemaVersion":1,"backend":backend,"requestedModel":model,"durationMs":start.elapsed().as_millis(),
        "execution":if result.is_ok(){"completed"}else{"failed"},"model":result.as_ref().ok().and_then(|r|r.model.clone()),
        "usage":result.as_ref().ok().and_then(|r|r.usage.clone()),"costUsd":result.as_ref().ok().and_then(|r|r.cost_usd)}))?;
    Ok(result?.text)
}

/// Like `complete`, but parses a JSON object out of the reply (retries once on parse failure).
pub fn complete_json(system: &str, prompt: &str, o: &LlmOpts) -> Result<Value> {
    let text = complete(system, prompt, o)?;
    if let Some(v) = extract_json(&text) {
        return Ok(v);
    }
    let retry = format!("{prompt}\n\nRespond with a single JSON object and nothing else.");
    let text2 = complete(system, &retry, o)?;
    extract_json(&text2).with_context(|| format!("LLM did not return JSON: {}", text2.chars().take(300).collect::<String>()))
}
