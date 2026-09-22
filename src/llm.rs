//! Small LLM client used by the user simulator and the judge.
//!
//! Backends:
//!   api        — Anthropic Messages API over HTTPS via `curl` (there is no official Rust SDK);
//!                credentials from ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, or an `ant auth login` profile
//!   claude-cli — `claude -p` with tools disabled; reuses the local Claude Code login
//!   auto       — api when Anthropic credentials are visible, otherwise claude-cli
//!   cmd        — run $CASIMIR_LLM_CMD with the prompt on stdin and the system prompt in
//!                $CASIMIR_LLM_SYSTEM; the reply is its stdout (for tests and custom gateways)
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::util::{clean_command, extract_json, home_dir};

pub const DEFAULT_MODEL: &str = "claude-opus-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Clone, Debug)]
pub struct LlmOpts {
    pub model: Option<String>,
    pub backend: String,
    pub max_tokens: u64,
}

impl Default for LlmOpts {
    fn default() -> Self {
        LlmOpts { model: None, backend: "auto".into(), max_tokens: 16000 }
    }
}

enum Auth {
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

fn resolve_auth() -> Result<Auth> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Ok(Auth::ApiKey(k));
        }
    }
    if let Ok(t) = std::env::var("ANTHROPIC_AUTH_TOKEN") {
        if !t.is_empty() {
            return Ok(Auth::Bearer(t));
        }
    }
    // `ant auth login` profile: short-lived token, sent as Bearer with the oauth beta header
    if let Ok(out) = Command::new("ant").args(["auth", "print-credentials", "--access-token"]).output() {
        let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !tok.is_empty() {
            return Ok(Auth::OAuth(tok));
        }
    }
    bail!("no Anthropic credentials: set ANTHROPIC_API_KEY, run `ant auth login`, or use --llm claude-cli")
}

fn complete_api(system: &str, prompt: &str, model: &str, max_tokens: u64) -> Result<String> {
    let auth = resolve_auth()?;
    let body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": prompt }],
        "fallbacks": "default",
    });
    let mut betas = vec!["server-side-fallback-2026-07-01"];
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--max-time", "600", "-X", "POST", API_URL, "-H", "content-type: application/json", "-H", "anthropic-version: 2023-06-01"]);
    match &auth {
        Auth::ApiKey(k) => {
            cmd.args(["-H", &format!("x-api-key: {k}")]);
        }
        Auth::Bearer(t) => {
            cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
        }
        Auth::OAuth(t) => {
            betas.push("oauth-2025-04-20");
            cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
        }
    }
    cmd.args(["-H", &format!("anthropic-beta: {}", betas.join(","))]);
    cmd.args(["--data-binary", "@-"]);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawning curl")?;
    child.stdin.take().context("stdin")?.write_all(body.to_string().as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("curl failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let resp: Value = serde_json::from_slice(&out.stdout).context("parsing API response")?;
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
    Ok(text)
}

fn complete_cli(system: &str, prompt: &str, model: &str) -> Result<String> {
    let bin = std::env::var("CASIMIR_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
    let mut cmd = Command::new(&bin);
    cmd.args(["-p", "--output-format", "json", "--tools", "", "--no-session-persistence", "--model", model, "--system-prompt", system]);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    clean_command(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("spawning {bin}"))?;
    child.stdin.take().context("stdin")?.write_all(prompt.as_bytes())?;
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout.trim().lines().last().unwrap_or("");
    let parsed: Value = serde_json::from_str(last).with_context(|| format!("claude -p returned no JSON ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()))?;
    if parsed.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        bail!("claude -p error: {}", parsed.get("result").and_then(Value::as_str).unwrap_or(""));
    }
    Ok(parsed.get("result").and_then(Value::as_str).unwrap_or("").to_string())
}

fn complete_cmd(system: &str, prompt: &str, model: &str) -> Result<String> {
    let bin = std::env::var("CASIMIR_LLM_CMD").context("--llm cmd needs CASIMIR_LLM_CMD")?;
    let mut child = Command::new(&bin)
        .env("CASIMIR_LLM_SYSTEM", system)
        .env("CASIMIR_LLM_MODEL", model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {bin}"))?;
    child.stdin.take().context("stdin")?.write_all(prompt.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("{bin} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Effective model name for the selected backend (what will be recorded in reports).
pub fn effective_model(o: &LlmOpts) -> String {
    o.model.clone().unwrap_or_else(|| DEFAULT_MODEL.into())
}

/// Text completion through the selected backend.
pub fn complete(system: &str, prompt: &str, o: &LlmOpts) -> Result<String> {
    let model = effective_model(o);
    match pick_backend(&o.backend).as_str() {
        "api" => complete_api(system, prompt, &model, o.max_tokens),
        "claude-cli" => complete_cli(system, prompt, &model),
        "cmd" => complete_cmd(system, prompt, &model),
        other => bail!("unknown llm backend {other}"),
    }
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
