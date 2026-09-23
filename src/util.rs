use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Read a bounded transcript, tolerating only an interrupted final JSONL record.
pub fn read_jsonl(file: &Path) -> Result<Vec<Value>> {
    let file = fs::File::open(file).with_context(|| format!("reading {}", file.display()))?;
    if file.metadata()?.len() > 256 * 1024 * 1024 { anyhow::bail!("transcript exceeds the 256 MiB inspection limit"); }
    let mut reader = BufReader::new(file);
    let mut records = Vec::new();
    let mut line = Vec::new();
    let mut number = 0;
    loop {
        line.clear();
        let n = reader.by_ref().take(4 * 1024 * 1024 + 1).read_until(b'\n', &mut line)?;
        if n == 0 { break; }
        number += 1;
        if n > 4 * 1024 * 1024 { anyhow::bail!("JSONL record {number} exceeds 4 MiB"); }
        if line.iter().all(u8::is_ascii_whitespace) { continue; }
        match serde_json::from_slice(&line) {
            Ok(record) => records.push(record),
            Err(_) if !line.ends_with(b"\n") && reader.fill_buf()?.is_empty() => break,
            Err(err) => return Err(err).with_context(|| format!("malformed JSONL record {number}")),
        }
    }
    Ok(records)
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
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&p, pred, out);
        } else if pred(&p) {
            out.push(p);
        }
    }
}

pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOMEDRIVE").zip(std::env::var_os("HOMEPATH")).map(|(mut drive, path)| { drive.push(path); drive }));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

pub fn casimir_home() -> PathBuf {
    std::env::var_os("CASIMIR_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".casimir"))
}

pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)] private_windows_acl(path, true)?;
    Ok(())
}

pub fn private_file(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    Ok(options.open(path)?)
}

/// Replace only after the complete new file is flushed. tempfile uses native replacement
/// on Windows; readers see either the previous complete document or the new one.
pub fn atomic_write(file: &Path, data: &[u8]) -> Result<()> {
    let parent = file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(windows)] private_windows_acl(temp.path(), false)?;
    temp.write_all(data)?;
    temp.as_file().sync_all()?;
    let started = std::time::Instant::now();
    loop {
        match temp.persist(file) {
            Ok(_) => break,
            Err(error) if cfg!(windows) && matches!(error.error.raw_os_error(), Some(5 | 32 | 33)) && started.elapsed() < std::time::Duration::from_secs(2) => {
                // Antivirus and concurrent readers can briefly hold a Windows sharing lock.
                // Keep the old file intact and retry the same complete temporary file.
                temp = error.file;
                std::thread::sleep(std::time::Duration::from_millis(10));
            },
            Err(error) => return Err(error).with_context(|| format!("replacing {}", file.display())),
        }
    }
    #[cfg(unix)] fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn write_json<T: serde::Serialize>(file: &Path, data: &T) -> Result<()> {
    let mut s = serde_json::to_vec_pretty(data)?;
    s.push(b'\n');
    atomic_write(file, &s)
}

/// Locks are held by the open handle and released by the OS on crashes.
pub struct RunLock { _file: fs::File }
impl RunLock {
    pub fn acquire_wait(dir: &Path) -> Result<Self> {
        let start = std::time::Instant::now();
        loop {
            match Self::acquire(dir) {
                Ok(lock) => return Ok(lock),
                Err(err) if start.elapsed() >= std::time::Duration::from_secs(30) => return Err(err),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }
    pub fn acquire(dir: &Path) -> Result<Self> {
        if dir.join(".deleting").exists() { anyhow::bail!("run is being cleaned up"); }
        private_dir(dir)?;
        let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(".lock"))?;
        fs2::FileExt::try_lock_exclusive(&file).context("run is locked by another Casimir process")?;
        if dir.join(".deleting").exists() { anyhow::bail!("run is being cleaned up"); }
        Ok(Self { _file: file })
    }
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
    // Credentials and user configuration are not nesting markers. In particular,
    // CLAUDE_CODE_OAUTH_TOKEN is the only authentication source in some CI/cloud setups.
    matches!(key, "CLAUDECODE" | "CLAUDE_PID" | "CLAUDE_AGENT_SDK_VERSION" | "CLAUDE_CODE_ENTRYPOINT")
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

/// Pick the most informative line of a failed process's stderr: the first line mentioning an error,
/// else the last non-empty line.
pub fn stderr_error_line(stderr: &str, fallback: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let pick = lines.iter().find(|l| l.to_ascii_lowercase().contains("error")).or_else(|| lines.last()).copied().unwrap_or(fallback);
    truncate(pick, 500)
}

impl Drop for RunLock {
    fn drop(&mut self) { let _ = fs2::FileExt::unlock(&self._file); }
}

/// Protect raw evidence from inherited broad ACLs. Owner rights and SYSTEM retain access;
/// this is local confidentiality, not isolation from the harness running as the same user.
#[cfg(windows)]
fn private_windows_acl(path: &Path, directory: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{Foundation::LocalFree, Security::{Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION}};
    let text = if directory { "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)" } else { "D:P(A;;FA;;;OW)(A;;FA;;;SY)" };
    let sddl: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) == 0 { return Err(std::io::Error::last_os_error().into()); }
        let success = SetFileSecurityW(path.as_ptr(), DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, descriptor);
        let error = if success == 0 { Some(std::io::Error::last_os_error()) } else { None };
        LocalFree(descriptor);
        if let Some(error) = error { return Err(error).context("protecting local artifact ACL"); }
    }
    Ok(())
}
