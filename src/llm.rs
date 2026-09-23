//! Small LLM client used by the user simulator and the judge.
//!
//! Backends:
//!   api        — Anthropic Messages API over HTTPS in process;
//!                credentials from an explicitly supplied ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN
//!   claude-cli — `claude -p` with tools disabled; reuses the local Claude Code login
//!   codex-cli  — `codex exec --json` with read-only permissions; reuses the local ChatGPT login
//!   auto       — a logged-in subscription CLI (Claude first, then Codex); never an API key
//!   cmd        — run $CASIMIR_LLM_CMD with the prompt on stdin and the system prompt in
//!                $CASIMIR_LLM_SYSTEM; the reply is its stdout (for tests and custom gateways)
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::process::Command;
use std::time::Duration;
use serde::{Serialize, Deserialize};

use crate::util::extract_json;
use crate::model::Harness;

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
}

pub fn pick_backend(requested: &str) -> String {
    if requested != "auto" && !requested.is_empty() {
        return requested.to_string();
    }
    if crate::doctor::subscription_ready(Harness::ClaudeCode) { return "claude-cli".into(); }
    if crate::doctor::subscription_ready(Harness::Codex) { return "codex-cli".into(); }
    // Keep the failure tied to a concrete provider with actionable login guidance.
    "claude-cli".into()
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
    bail!("the optional Anthropic API backend needs ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN; subscription users should use --llm auto")
}

fn complete_api(system: &str, prompt: &str, model: &str, max_tokens: u64, timeout: u64, spool: &std::path::Path) -> Result<Completion> {
    let body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": prompt }],
        "fallbacks": "default",
    });
    request_api(resolve_auth()?, API_URL, body, timeout, spool)
}

fn request_api(auth: Credential, url: &str, body: Value, timeout: u64, spool: &std::path::Path) -> Result<Completion> {
    let betas = ["server-side-fallback-2026-07-01"];
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(timeout)).redirects(0).build();
    let mut request = agent.post(url).set("content-type", "application/json").set("anthropic-version", "2023-06-01");
    match &auth {
        Credential::ApiKey(k) => request = request.set("x-api-key", k),
        Credential::Bearer(t) => request = request.set("Authorization", &format!("Bearer {t}")),
    }
    let response = request.set("anthropic-beta", &betas.join(",")).send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(status, _) => anyhow::anyhow!("Anthropic API HTTP {status} (response omitted for privacy)"),
            ureq::Error::Transport(_) => anyhow::anyhow!("Anthropic API transport failed or timed out"),
        })?;
    let mut bytes = Vec::new();
    let read = response.into_reader().take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes);
    crate::util::atomic_write(&spool.join("response.raw"), &bytes)?;
    read.context("reading API response (partial bytes retained privately)")?;
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
    let system_file = spool.join("system.txt");
    crate::util::atomic_write(&system_file, system.as_bytes())?;
    let working = tempfile::tempdir()?;
    cmd.current_dir(working.path());
    cmd.args(["-p", "--safe-mode", "--output-format", "json", "--tools", "", "--no-session-persistence", "--model", model, "--system-prompt-file"]).arg(std::fs::canonicalize(&system_file)?);
    crate::util::subscription_command(&mut cmd, Harness::ClaudeCode);
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

fn complete_codex_cli(system: &str, prompt: &str, model: Option<&str>, timeout: u64, spool: &std::path::Path) -> Result<Completion> {
    let bin = std::env::var("CASIMIR_CODEX_BIN").unwrap_or_else(|_| "codex".into());
    let working = tempfile::tempdir()?;
    let mut cmd = Command::new(&bin);
    cmd.current_dir(working.path()).args(["exec", "--json", "--ephemeral", "--ignore-user-config", "--ignore-rules", "--skip-git-repo-check", "--sandbox", "read-only", "--color", "never", "-c", "forced_login_method=chatgpt"]);
    if let Some(model) = model { cmd.args(["--model", model]); }
    cmd.arg("-");
    crate::util::subscription_command(&mut cmd, Harness::Codex);
    let input = format!("Follow these evaluation instructions. Do not use tools or access files. Return only the requested answer.\n\n<evaluation_instructions>\n{system}\n</evaluation_instructions>\n\n<input>\n{prompt}\n</input>\n");
    let out = crate::process::capture(&mut cmd, input.as_bytes(), Duration::from_secs(timeout), Some(&spool.join("process")))?;
    if !out.status.success() { bail!("codex exec failed: {}", crate::util::stderr_error_line(&out.stderr, "check `codex login status` and your subscription")); }
    let mut final_text = None;
    let mut usage = None;
    let mut completed = false;
    for line in out.stdout.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let value: Value = serde_json::from_slice(line).context("malformed Codex JSON stream; raw bytes retained privately")?;
        match value.get("type").and_then(Value::as_str) {
            Some("item.completed") if value.get("item").and_then(|item| item.get("type")).and_then(Value::as_str) == Some("agent_message") => {
                final_text = value.get("item").and_then(|item| item.get("text")).and_then(Value::as_str).map(String::from);
            }
            Some("turn.completed") => { completed = true; usage = value.get("usage").cloned(); }
            Some("turn.failed") | Some("error") => bail!("Codex judge/simulator call failed; inspect private process log"),
            _ => {}
        }
    }
    if !completed { bail!("Codex judge/simulator call ended without turn.completed; inspect private process log"); }
    let text = final_text.filter(|text| !text.trim().is_empty()).context("Codex returned no final agent message")?;
    Ok(Completion { text, usage, cost_usd: None, model: model.map(String::from) })
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
    o.model.clone().unwrap_or_else(|| if pick_backend(&o.backend) == "codex-cli" { "codex-cli-default".into() } else { DEFAULT_MODEL.into() })
}

/// Text completion through the selected backend.
pub fn complete(system: &str, prompt: &str, o: &LlmOpts) -> Result<String> {
    let model = effective_model(o);
    let backend = pick_backend(&o.backend);
    if backend == "claude-cli" && !crate::doctor::subscription_ready(Harness::ClaudeCode) {
        bail!("Claude Code subscription login is required: run `claude auth login` or select --llm codex-cli after `codex login`");
    }
    if backend == "codex-cli" && !crate::doctor::subscription_ready(Harness::Codex) {
        bail!("Codex subscription login is required: run `codex login` or select --llm claude-cli after `claude auth login`");
    }
    let spool = o.recording_dir.clone().unwrap_or_else(|| crate::util::casimir_home().join("llm")).join(uuid::Uuid::new_v4().to_string());
    crate::util::private_dir(&spool)?;
    let start = std::time::Instant::now();
    let result = match backend.as_str() {
        "api" => complete_api(system, prompt, &model, o.max_tokens, o.timeout_secs, &spool),
        "claude-cli" => complete_cli(system, prompt, &model, o.timeout_secs, &spool),
        "codex-cli" => complete_codex_cli(system, prompt, o.model.as_deref(), o.timeout_secs, &spool),
        "cmd" => complete_cmd(system, prompt, &model, o.timeout_secs, &spool),
        other => bail!("unknown llm backend {other}"),
    };
    crate::util::write_json(&spool.join("call.json"), &json!({"schemaVersion":1,"backend":backend,"requestedModel":model,"durationMs":start.elapsed().as_millis(),
        "execution":if result.is_ok(){"completed"}else{"failed"},"model":result.as_ref().ok().and_then(|r|r.model.clone()),
        "usage":result.as_ref().ok().and_then(|r|r.usage.clone()),"costUsd":result.as_ref().ok().and_then(|r|r.cost_usd),
        "costBasis":if backend=="claude-cli" {"provider_client_estimate"} else if backend=="codex-cli" {"unknown"} else {"provider_reported_or_unknown"}}))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    fn server(response: Vec<u8>, delay: Duration) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/messages",listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream,_) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut received = Vec::new();
            let mut byte = [0];
            while !received.ends_with(b"\r\n\r\n") { stream.read_exact(&mut byte).unwrap(); received.push(byte[0]); }
            let header = String::from_utf8(received).unwrap();
            let length = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|n|n.trim().parse::<usize>().unwrap())).unwrap_or(0);
            let mut body = vec![0;length];stream.read_exact(&mut body).unwrap();
            std::thread::sleep(delay);
            let _ = stream.write_all(&response);
            header
        });
        (url,handle)
    }
    #[test]
    fn authentication_and_rate_limit_errors_omit_response_secrets() {
        for status in [401,403,429] {
            let (url,server) = server(format!("HTTP/1.1 {status} Error\r\nContent-Length: 13\r\nConnection: close\r\n\r\nsecret-token!").into_bytes(),Duration::ZERO);
            let directory = tempfile::tempdir().unwrap();
            let error = request_api(Credential::ApiKey("test-credential".into()),&url,json!({}),2,directory.path()).err().unwrap().to_string();
            assert!(error.contains(&status.to_string()));assert!(!error.contains("secret-token"));assert!(!error.contains("test-credential"));
            let request = server.join().unwrap();assert!(request.contains("x-api-key: test-credential"));
        }
    }
    #[test]
    fn malformed_and_partial_api_responses_are_retained_and_fail() {
        for response in [b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot-json".to_vec(),b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"partial\":".to_vec()] {
            let (url,server) = server(response,Duration::ZERO);let directory = tempfile::tempdir().unwrap();
            assert!(request_api(Credential::Bearer("test".into()),&url,json!({}),2,directory.path()).is_err());
            assert!(!std::fs::read(directory.path().join("response.raw")).unwrap().is_empty());server.join().unwrap();
        }
    }
    #[test]
    fn api_deadline_and_usage_are_reported() {
        let (url,slow) = server(Vec::new(),Duration::from_millis(1200));let directory = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        assert!(request_api(Credential::Bearer("test".into()),&url,json!({}),1,directory.path()).is_err());
        assert!(start.elapsed()<Duration::from_secs(2));slow.join().unwrap();
        let body = json!({"model":"observed-model","usage":{"input_tokens":12,"output_tokens":3},"content":[{"type":"text","text":"reply"}]}).to_string();
        let (url,server) = server(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).into_bytes(),Duration::ZERO);
        let reply = request_api(Credential::ApiKey("test".into()),&url,json!({}),2,directory.path()).unwrap();
        assert_eq!(reply.text,"reply");assert_eq!(reply.usage.unwrap()["input_tokens"],12);assert!(reply.cost_usd.is_none());server.join().unwrap();
    }
}
