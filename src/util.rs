use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Parse a JSONL file, skipping malformed lines (a session still being written may end mid-line).
pub fn read_jsonl(file: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.is_empty() {
                None
            } else {
                serde_json::from_str(t).ok()
            }
        })
        .collect())
}

/// Parse only the first `bytes` of a JSONL file (complete lines only).
pub fn read_jsonl_head(file: &Path, bytes: usize) -> Result<Vec<Value>> {
    let mut f = fs::File::open(file)?;
    let mut buf = vec![0u8; bytes];
    let mut read = 0;
    while read < bytes {
        let n = f.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    buf.truncate(read);
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if read == bytes {
        lines.pop(); // last line may be truncated
    }
    Ok(lines
        .into_iter()
        .filter_map(|l| {
            let t = l.trim();
            if t.is_empty() {
                None
            } else {
                serde_json::from_str(t).ok()
            }
        })
        .collect())
}

/// Read the first `bytes` of a file as (lossy) text.
pub fn read_prefix(file: &Path, bytes: usize) -> Result<String> {
    let mut f = fs::File::open(file)?;
    let mut buf = vec![0u8; bytes];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Recursively list files under `dir` for which `pred` holds.
pub fn walk(dir: &Path, pred: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, pred, out);
        } else if pred(&p) {
            out.push(p);
        }
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn casimir_home() -> PathBuf {
    std::env::var_os("CASIMIR_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".casimir"))
}

pub fn write_json<T: serde::Serialize>(file: &Path, data: &T) -> Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut s = serde_json::to_string_pretty(data)?;
    s.push('\n');
    fs::write(file, s).with_context(|| format!("writing {}", file.display()))
}

pub fn read_json<T: serde::de::DeserializeOwned>(file: &Path) -> Result<T> {
    let text = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))
}

pub fn truncate(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count > n {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn fmt_duration(ms: i64) -> String {
    if ms < 0 {
        return "-".into();
    }
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let s = ms as f64 / 1000.0;
    if s < 60.0 {
        return format!("{s:.1}s");
    }
    let m = (s / 60.0).floor() as i64;
    let rs = (s % 60.0).round() as i64;
    if m < 60 {
        return format!("{m}m{rs:02}s");
    }
    format!("{}h{:02}m", m / 60, m % 60)
}

pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in s.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    trimmed.chars().take(40).collect()
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn now_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// Milliseconds since the epoch for an ISO-8601 timestamp.
pub fn ts_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis())
}

/// Extract the first JSON object from free text (tolerates ```json fences).
pub fn extract_json(text: &str) -> Option<Value> {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(start) = text.find("```") {
        let rest = &text[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.find("```") {
            candidates.push(&rest[..end]);
        }
    }
    candidates.push(text);
    for c in candidates {
        let Some(start) = c.find('{') else { continue };
        let bytes = c.as_bytes();
        let mut depth = 0i32;
        let mut in_str = false;
        let mut i = start;
        while i < bytes.len() {
            let ch = bytes[i];
            if in_str {
                if ch == b'\\' {
                    i += 1;
                } else if ch == b'"' {
                    in_str = false;
                }
            } else if ch == b'"' {
                in_str = true;
            } else if ch == b'{' {
                depth += 1;
            } else if ch == b'}' {
                depth -= 1;
                if depth == 0 {
                    if let Ok(v) = serde_json::from_str::<Value>(&c[start..=i]) {
                        return Some(v);
                    }
                    break;
                }
            }
            i += 1;
        }
    }
    None
}

/// Env vars that make nested harness invocations misbehave.
pub fn is_nested_harness_var(key: &str) -> bool {
    key == "CLAUDECODE" || key.starts_with("CLAUDE_CODE_") || key == "CLAUDE_PID" || key == "CLAUDE_AGENT_SDK_VERSION"
}

pub fn clean_command(cmd: &mut std::process::Command) {
    for (k, _) in std::env::vars_os() {
        if let Some(ks) = k.to_str() {
            if is_nested_harness_var(ks) {
                cmd.env_remove(&k);
            }
        }
    }
}

pub fn is_tty() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

/// ANSI color helpers (empty strings when not a TTY).
pub struct Colors {
    pub reset: &'static str,
    pub bold: &'static str,
    pub dim: &'static str,
    pub red: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub blue: &'static str,
    pub magenta: &'static str,
    pub cyan: &'static str,
    pub gray: &'static str,
}

pub fn colors() -> Colors {
    if is_tty() {
        Colors {
            reset: "\x1b[0m",
            bold: "\x1b[1m",
            dim: "\x1b[2m",
            red: "\x1b[31m",
            green: "\x1b[32m",
            yellow: "\x1b[33m",
            blue: "\x1b[34m",
            magenta: "\x1b[35m",
            cyan: "\x1b[36m",
            gray: "\x1b[90m",
        }
    } else {
        Colors { reset: "", bold: "", dim: "", red: "", green: "", yellow: "", blue: "", magenta: "", cyan: "", gray: "" }
    }
}

pub fn pad(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len >= n {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(n - len))
    }
}

pub fn indent(text: &str, prefix: &str) -> String {
    text.lines().map(|l| format!("{prefix}{l}")).collect::<Vec<_>>().join("\n")
}

/// `value.get(path...)` helper for nested JSON access.
pub fn jget<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for p in path {
        cur = cur.get(*p)?;
    }
    Some(cur)
}

pub fn jstr<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    jget(v, path).and_then(Value::as_str)
}

pub fn ju64(v: &Value, path: &[&str]) -> u64 {
    jget(v, path).and_then(Value::as_u64).unwrap_or(0)
}
