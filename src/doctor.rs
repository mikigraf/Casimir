//! Local setup inspection. Version and login-status commands never request a completion.
use anyhow::Result;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub fn executable(name: &std::ffi::OsStr) -> Option<PathBuf> {
    let path = Path::new(name);
    let candidates: Vec<PathBuf> = if path.components().count() > 1 || path.is_absolute() {
        vec![path.to_path_buf()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(path))
            .collect()
    };
    for candidate in candidates {
        #[cfg(windows)]
        let variants = std::iter::once(candidate.clone()).chain(
            std::env::var("PATHEXT")
                .unwrap_or_else(|_| ".EXE;.COM;.CMD;.BAT".into())
                .split(';')
                .map(|ext| PathBuf::from(format!("{}{ext}", candidate.display())))
                .collect::<Vec<_>>(),
        );
        #[cfg(not(windows))]
        let variants = std::iter::once(candidate);
        for variant in variants {
            if !variant.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if std::fs::metadata(&variant).ok()?.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return std::fs::canonicalize(&variant).ok();
        }
    }
    None
}

fn probe(bin: &Path, args: &[&str]) -> Result<crate::process::Output> {
    let temp = tempfile::tempdir()?;
    crate::process::capture(
        Command::new(bin).args(args),
        b"",
        Duration::from_secs(15),
        Some(&temp.path().join("probe")),
    )
}

fn probe_subscription(
    bin: &Path,
    args: &[&str],
    harness: crate::model::Harness,
) -> Result<crate::process::Output> {
    let temp = tempfile::tempdir()?;
    let mut command = Command::new(bin);
    command.args(args);
    crate::util::subscription_command(&mut command, harness);
    crate::process::capture(
        &mut command,
        b"",
        Duration::from_secs(15),
        Some(&temp.path().join("probe")),
    )
}

#[cfg(target_os = "linux")]
fn codex_sandbox_probe(bin: &Path) -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let mut command = Command::new(bin);
    command
        .args(["sandbox", "--", "/usr/bin/true"])
        .current_dir(temp.path());
    crate::util::subscription_command(&mut command, crate::model::Harness::Codex);
    crate::process::capture(
        &mut command,
        b"",
        Duration::from_secs(8),
        Some(&temp.path().join("sandbox")),
    )
    .is_ok_and(|output| output.status.success())
}

fn subscription_method(
    harness: crate::model::Harness,
    output: &crate::process::Output,
) -> &'static str {
    if !output.status.success() {
        return "not_authenticated";
    }
    match harness {
        crate::model::Harness::ClaudeCode => {
            let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
                return "unknown";
            };
            if value.get("loggedIn").and_then(Value::as_bool) != Some(true) {
                return "not_authenticated";
            }
            match value.get("authMethod").and_then(Value::as_str) {
                Some("oauth_token" | "oauth") => "subscription",
                Some("api_key") => "api_key",
                _ => "unknown",
            }
        }
        crate::model::Harness::Codex => {
            // Codex currently writes `login status` to stderr even on success.
            let status = format!(
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                output.stderr
            )
            .to_ascii_lowercase();
            if status.contains("not logged in") || status.contains("signed out") {
                return "not_authenticated";
            }
            if status.contains("chatgpt") {
                "subscription"
            } else if status.contains("api key") || status.contains("api-key") {
                "api_key"
            } else {
                "unknown"
            }
        }
        _ => "unknown",
    }
}

pub fn subscription_ready(harness: crate::model::Harness) -> bool {
    let (name, variable, args) = match harness {
        crate::model::Harness::ClaudeCode => (
            "claude",
            "CASIMIR_CLAUDE_BIN",
            &["auth", "status", "--json"][..],
        ),
        crate::model::Harness::Codex => ("codex", "CASIMIR_CODEX_BIN", &["login", "status"][..]),
        _ => return false,
    };
    let name = std::env::var_os(variable).unwrap_or_else(|| name.into());
    executable(&name)
        .and_then(|bin| probe_subscription(&bin, args, harness).ok())
        .is_some_and(|output| subscription_method(harness, &output) == "subscription")
}

pub fn report() -> Value {
    let harnesses: Vec<Value> = [
        ("claude-code", "claude", "CASIMIR_CLAUDE_BIN", vec!["auth", "status", "--json"]),
        ("codex", "codex", "CASIMIR_CODEX_BIN", vec!["login", "status"]),
        ("copilot", "copilot", "CASIMIR_COPILOT_BIN", vec![]),
        ("gemini", "gemini", "CASIMIR_GEMINI_BIN", vec![]),
    ].into_iter().map(|(id, bin, variable, auth_args)| {
        let name = std::env::var_os(variable).unwrap_or_else(|| bin.into());
        let path = executable(&name);
        let version = path.as_ref().and_then(|p| probe(p, &["--version"]).ok())
            .filter(|o| o.status.success()).map(|o| crate::util::truncate(String::from_utf8_lossy(&o.stdout).trim(), 120));
        let method = path.as_ref().filter(|_| !auth_args.is_empty()).map(|p| {
            let harness = if id == "codex" { crate::model::Harness::Codex } else { crate::model::Harness::ClaudeCode };
            probe_subscription(p, &auth_args, harness).map(|output| subscription_method(harness, &output)).unwrap_or("unknown")
        }).unwrap_or("unknown");
        let auth = match method { "subscription" | "api_key" => "authenticated", "not_authenticated" => "not_authenticated", _ => "unknown_or_not_authenticated" };
        #[cfg(target_os = "linux")]
        let sandbox_ready = if id == "codex" { path.as_ref().is_some_and(|p| codex_sandbox_probe(p)) } else { true };
        #[cfg(not(target_os = "linux"))]
        let sandbox_ready = true;
        let runtime_ready = path.is_some() && sandbox_ready;
        json!({"id":id,"executable":path,"version":version,"authentication":auth,
            "authenticationMethod":method,"subscriptionReady":method=="subscription","runtimeReady":runtime_ready,
            "sandboxProbe":if id=="codex" && cfg!(target_os="linux") {Some(if sandbox_ready {"passed"} else {"failed"})} else {None},
            "support":if matches!(id,"copilot"|"gemini") {"experimental"} else {"candidate"},
            "permissions": {"default":"preserve harness configuration", "osSandbox": match id {
                "codex" if cfg!(target_os="linux") && !sandbox_ready => "native Linux sandbox self-test failed; check bubblewrap and process capabilities",
                "codex" => "native sandbox available; configuration dependent",
                "claude-code" if cfg!(windows) => "not available on native Windows",
                "claude-code" => "platform sandbox available; configuration dependent",
                _ => "unvalidated"
            }}})
    }).collect();
    let storage = crate::util::casimir_home();
    let writable = std::fs::create_dir_all(&storage)
        .and_then(|_| tempfile::NamedTempFile::new_in(&storage).map(|_| ()))
        .is_ok();
    let git = executable(std::ffi::OsStr::new("git"));
    json!({"schemaVersion":1,"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
        "harnesses":harnesses,"git":git,"storage":{"path":storage,"writable":writable},
        "paidCalls":false,"notes":["Authentication status is local; expired or revoked credentials may only be detected at execution.","A worktree isolates repository edits, not the process or external services."],
        "compatibility":serde_json::from_str::<Value>(include_str!("../compatibility/harnesses.json")).unwrap()})
}

pub fn harness_version(harness: crate::model::Harness) -> Option<String> {
    let (bin, variable) = match harness {
        crate::model::Harness::ClaudeCode => ("claude", "CASIMIR_CLAUDE_BIN"),
        crate::model::Harness::Codex => ("codex", "CASIMIR_CODEX_BIN"),
        crate::model::Harness::Copilot => ("copilot", "CASIMIR_COPILOT_BIN"),
        crate::model::Harness::Gemini => ("gemini", "CASIMIR_GEMINI_BIN"),
    };
    let name = std::env::var_os(variable).unwrap_or_else(|| bin.into());
    let path = executable(&name)?;
    let output = probe(&path, &["--version"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    if text.contains("casimir-fixture") {
        Some("casimir-fixture 1.0.0".into())
    } else {
        text.split_whitespace()
            .find(|s| s.starts_with(|c: char| c.is_ascii_digit()))
            .map(|s| {
                s.trim_matches(|c: char| !c.is_ascii_digit() && c != '.')
                    .to_string()
            })
    }
}
