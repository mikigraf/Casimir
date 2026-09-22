//! Normalized session model shared by all harness adapters.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fmt;

use crate::util::{one_line, ts_ms, truncate};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    #[serde(rename = "claude-code")]
    ClaudeCode,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "copilot")]
    Copilot,
    #[serde(rename = "gemini")]
    Gemini,
}

impl Harness {
    pub fn parse(name: &str) -> anyhow::Result<Harness> {
        match name.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" | "cc" => Ok(Harness::ClaudeCode),
            "codex" | "openai" | "codex-cli" => Ok(Harness::Codex),
            "copilot" | "copilot-cli" | "gh-copilot" | "github-copilot" => Ok(Harness::Copilot),
            "gemini" | "gemini-cli" => Ok(Harness::Gemini),
            _ => anyhow::bail!("unknown harness \"{name}\" (expected claude-code, codex, copilot or gemini)"),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::Copilot => "copilot",
            Harness::Gemini => "gemini",
        }
    }
    pub fn all() -> [Harness; 4] {
        [Harness::ClaudeCode, Harness::Codex, Harness::Copilot, Harness::Gemini]
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    User,
    Assistant,
    Thinking,
    ToolCall,
    ToolResult,
    System,
    Error,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default)]
    pub reasoning: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.reasoning += o.reasoning;
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub output: String,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Simulated {
    pub verbatim: bool,
    pub reason: String,
    /// Original turn numbers the simulator says it drew the message from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounded_in: Vec<u32>,
}

/// Which model played the user in a rerun (recorded so simulator-induced variance is visible).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SimulatorInfo {
    pub model: String,
    pub backend: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub turn: u32,
    pub ts: String,
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidechain: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulated: Option<Simulated>,
}

impl Event {
    pub fn new(turn: u32, ts: impl Into<String>, kind: EventKind) -> Event {
        Event {
            turn,
            ts: ts.into(),
            kind,
            text: None,
            tool: None,
            result: None,
            model: None,
            usage: None,
            sidechain: false,
            subtype: None,
            msg_id: None,
            simulated: None,
        }
    }
    pub fn text(turn: u32, ts: impl Into<String>, kind: EventKind, text: impl Into<String>) -> Event {
        let mut e = Event::new(turn, ts, kind);
        e.text = Some(text.into());
        e
    }
    pub fn system(turn: u32, ts: impl Into<String>, subtype: &str, text: impl Into<String>) -> Event {
        let mut e = Event::text(turn, ts, EventKind::System, text);
        e.subtype = Some(subtype.to_string());
        e
    }
    pub fn tool_call(turn: u32, ts: impl Into<String>, id: impl Into<String>, name: impl Into<String>, input: Value) -> Event {
        let mut e = Event::new(turn, ts, EventKind::ToolCall);
        e.tool = Some(ToolCall { id: id.into(), name: name.into(), input });
        e
    }
    pub fn tool_result(turn: u32, ts: impl Into<String>, id: impl Into<String>, name: Option<String>, output: impl Into<String>, is_error: bool) -> Event {
        let mut e = Event::new(turn, ts, EventKind::ToolResult);
        e.result = Some(ToolResult { id: id.into(), name, output: output.into(), is_error });
        e
    }
    pub fn text_str(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RerunOf {
    pub harness: Harness,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub harness: Option<Harness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Cumulative usage when the harness reports it that way (Codex); otherwise summed per message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_total: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun_of: Option<RerunOf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_log_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulator: Option<SimulatorInfo>,
    #[serde(default)]
    pub events: Vec<Event>,
}

impl Session {
    pub fn new(harness: Harness) -> Session {
        Session { harness: Some(harness), ..Default::default() }
    }
    pub fn harness(&self) -> Harness {
        self.harness.unwrap_or(Harness::ClaudeCode)
    }
}

#[derive(Clone, Debug)]
pub struct UserTurn {
    pub turn: u32,
    pub ts: String,
    pub text: String,
}

/// Real user prompts in order (excludes tool results, harness-injected messages, sidechains).
pub fn user_turns(session: &Session) -> Vec<UserTurn> {
    session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::User && !e.sidechain)
        .map(|e| UserTurn { turn: e.turn, ts: e.ts.clone(), text: e.text_str().to_string() })
        .collect()
}

pub fn events_for_turn(session: &Session, turn: u32) -> Vec<&Event> {
    session.events.iter().filter(|e| e.turn == turn).collect()
}

/// Last assistant text of a turn (or of the whole session).
pub fn final_assistant_text(session: &Session, turn: Option<u32>) -> String {
    session
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Assistant && !e.sidechain && !e.text_str().trim().is_empty() && turn.is_none_or(|t| e.turn == t))
        .next_back()
        .map(|e| e.text_str().to_string())
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TouchedFile {
    pub path: String,
    pub ops: Vec<String>,
}

/// Command string for shell-like tool calls.
pub fn shell_command(e: &Event) -> Option<String> {
    let tool = e.tool.as_ref()?;
    let inp = &tool.input;
    match tool.name.as_str() {
        "Bash" => inp.get("command").and_then(Value::as_str).map(String::from),
        "shell" | "shell_command" | "local_shell" | "exec_command" | "container.exec" | "command_execution" => {
            if let Some(arr) = inp.get("command").and_then(Value::as_array) {
                Some(arr.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
            } else if let Some(s) = inp.get("command").and_then(Value::as_str) {
                Some(s.to_string())
            } else {
                inp.get("cmd").and_then(Value::as_str).map(String::from)
            }
        }
        _ => None,
    }
}

fn patch_files(patch: &str) -> Vec<(String, String)> {
    patch
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("*** ")?;
            for op in ["Add", "Update", "Delete"] {
                if let Some(p) = rest.strip_prefix(&format!("{op} File: ")) {
                    return Some((p.trim().to_string(), op.to_ascii_lowercase()));
                }
            }
            None
        })
        .collect()
}

/// Files a shell command writes via `cat/echo/printf > file` or `tee file`.
fn shell_written_files(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    // split into simple commands on ; & | ( and newlines
    for part in cmd.split(|c| c == ';' || c == '&' || c == '|' || c == '(' || c == '\n') {
        let t = part.trim_start();
        let is_writer = ["cat", "echo", "printf"].iter().any(|w| t.starts_with(w) && t[w.len()..].starts_with(|c: char| c.is_whitespace() || c == '>'));
        if is_writer {
            if let Some(idx) = t.find('>') {
                let after = t[idx..].trim_start_matches('>').trim_start();
                let target: String = after.chars().take_while(|c| !c.is_whitespace() && !"<>".contains(*c)).collect();
                if !target.is_empty() {
                    out.push(target);
                }
            }
        }
        let mut toks = t.split_whitespace().peekable();
        while let Some(tok) = toks.next() {
            if tok == "tee" {
                let mut nxt = toks.next();
                if nxt == Some("-a") {
                    nxt = toks.next();
                }
                if let Some(f) = nxt {
                    let f: String = f.chars().take_while(|c| !"<>;&|".contains(*c)).collect();
                    if !f.is_empty() {
                        out.push(f);
                    }
                }
            }
        }
    }
    out
}

/// Files written by the agent, inferred from tool calls.
pub fn files_touched(session: &Session) -> Vec<TouchedFile> {
    let mut order: Vec<String> = Vec::new();
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut add = |p: &str, op: &str| {
        if p.is_empty() {
            return;
        }
        let ops = files.entry(p.to_string()).or_insert_with(|| {
            order.push(p.to_string());
            Vec::new()
        });
        if !ops.iter().any(|o| o == op) {
            ops.push(op.to_string());
        }
    };
    for e in &session.events {
        let Some(tool) = e.tool.as_ref() else { continue };
        let inp = &tool.input;
        let s = |k: &str| inp.get(k).and_then(Value::as_str).unwrap_or("");
        match tool.name.as_str() {
            "Write" => add(s("file_path"), "write"),
            "Edit" | "MultiEdit" | "NotebookEdit" => {
                let p = if s("file_path").is_empty() { s("notebook_path") } else { s("file_path") };
                add(p, "edit");
            }
            "apply_patch" => {
                let patch = inp.get("patch").and_then(Value::as_str).or_else(|| inp.as_str()).unwrap_or("");
                for (p, op) in patch_files(patch) {
                    add(&p, &op);
                }
                if let Some(changes) = inp.get("changes").and_then(Value::as_array) {
                    for ch in changes {
                        add(ch.get("path").and_then(Value::as_str).unwrap_or(""), ch.get("kind").and_then(Value::as_str).unwrap_or("edit"));
                    }
                }
            }
            "file_change" => {
                if let Some(changes) = inp.get("changes").and_then(Value::as_array) {
                    for ch in changes {
                        add(ch.get("path").and_then(Value::as_str).unwrap_or(""), ch.get("kind").and_then(Value::as_str).unwrap_or("edit"));
                    }
                }
            }
            other => {
                if let Some(cmd) = shell_command(e) {
                    for f in shell_written_files(&cmd) {
                        add(&f, "shell-write");
                    }
                    continue;
                }
                // Generic file tools (Copilot `edit`/`create`, Gemini `write_file`/`replace`, MCP editors…)
                let lower = other.to_ascii_lowercase();
                let writes = ["edit", "write", "create", "replace", "patch", "insert", "delete", "remove"].iter().any(|w| lower.contains(w));
                if writes {
                    let p = ["file_path", "filePath", "path", "file", "target_file", "filename"].iter().find_map(|k| inp.get(*k).and_then(Value::as_str)).unwrap_or("");
                    let op = if lower.contains("create") || lower.contains("write") { "write" } else if lower.contains("delete") || lower.contains("remove") { "delete" } else { "edit" };
                    add(p, op);
                }
            }
        }
    }
    order.into_iter().map(|p| TouchedFile { ops: files.remove(&p).unwrap_or_default(), path: p }).collect()
}

/// One-line human summary of a tool call.
pub fn tool_one_liner(e: &Event, max: usize) -> String {
    let Some(tool) = e.tool.as_ref() else { return String::new() };
    let inp = &tool.input;
    let s = |k: &str| inp.get(k).and_then(Value::as_str);
    let detail = if let Some(cmd) = shell_command(e) {
        one_line(&cmd)
    } else if let Some(p) = s("file_path") {
        p.to_string()
    } else if let Some(p) = s("notebook_path") {
        p.to_string()
    } else if let Some(p) = s("pattern") {
        match s("path") {
            Some(d) => format!("{p} in {d}"),
            None => p.to_string(),
        }
    } else if tool.name == "apply_patch" {
        let patch = s("patch").unwrap_or("");
        let mut files: Vec<String> = patch_files(patch).into_iter().map(|(p, _)| p).collect();
        if files.is_empty() {
            if let Some(changes) = inp.get("changes").and_then(Value::as_array) {
                files = changes.iter().filter_map(|c| c.get("path").and_then(Value::as_str)).map(String::from).collect();
            }
        }
        files.join(", ")
    } else if let Some(q) = s("query") {
        one_line(q)
    } else if let Some(u) = s("url") {
        u.to_string()
    } else if let Some(d) = s("description") {
        one_line(d)
    } else if let Some(p) = s("prompt") {
        one_line(p)
    } else if let Some(raw) = inp.as_str() {
        one_line(raw)
    } else {
        one_line(&inp.to_string())
    };
    truncate(&format!("{}: {}", tool.name, detail), max)
}

pub fn duration_ms(session: &Session) -> i64 {
    let ts: Vec<i64> = session.events.iter().filter_map(|e| ts_ms(&e.ts)).collect();
    if ts.len() < 2 {
        return 0;
    }
    ts.iter().max().unwrap() - ts.iter().min().unwrap()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub harness: Option<Harness>,
    pub model: Option<String>,
    pub turns: usize,
    pub assistant_messages: usize,
    pub thinking_blocks: usize,
    pub tool_calls: usize,
    pub tool_errors: usize,
    pub tools_by_name: BTreeMap<String, usize>,
    pub files_touched: usize,
    pub duration_ms: i64,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
    pub errors: usize,
    pub sidechain_events: usize,
    pub final_message_chars: usize,
    /// User turns produced by the simulator rather than taken verbatim from the original.
    pub simulated_turns: usize,
}

/// Per-session aggregate statistics. Subagent (sidechain) traffic is reported separately.
pub fn stats(session: &Session) -> Stats {
    let mut s = Stats {
        harness: session.harness,
        model: session.model.clone(),
        files_touched: files_touched(session).len(),
        duration_ms: duration_ms(session),
        cost_usd: session.cost_usd,
        final_message_chars: final_assistant_text(session, None).chars().count(),
        ..Default::default()
    };
    let mut seen_msg: HashSet<String> = HashSet::new();
    let mut turns: HashSet<u32> = HashSet::new();
    for e in &session.events {
        if e.sidechain {
            s.sidechain_events += 1;
            continue;
        }
        match e.kind {
            EventKind::User => {
                turns.insert(e.turn);
                if e.simulated.as_ref().is_some_and(|sim| !sim.verbatim) {
                    s.simulated_turns += 1;
                }
            }
            EventKind::Assistant => s.assistant_messages += 1,
            EventKind::Thinking => s.thinking_blocks += 1,
            EventKind::ToolCall => {
                s.tool_calls += 1;
                let n = e.tool.as_ref().map(|t| t.name.clone()).unwrap_or_else(|| "?".into());
                *s.tools_by_name.entry(n).or_insert(0) += 1;
            }
            EventKind::ToolResult => {
                if e.result.as_ref().is_some_and(|r| r.is_error) {
                    s.tool_errors += 1;
                }
            }
            EventKind::Error => s.errors += 1,
            EventKind::System => {}
        }
        if let (Some(u), None) = (&e.usage, &session.usage_total) {
            let key = e.msg_id.clone().unwrap_or_else(|| format!("{}-{:?}", e.ts, e.kind));
            if seen_msg.insert(key) {
                s.usage.add(u);
            }
        }
    }
    if let Some(total) = &session.usage_total {
        s.usage = total.clone();
    }
    s.turns = turns.len();
    s
}

/// Recompute turn numbers from user events (used after merging).
pub fn renumber_turns(events: &mut [Event]) {
    let mut turn = 0u32;
    for e in events.iter_mut() {
        if e.kind == EventKind::User && !e.sidechain {
            turn += 1;
        }
        e.turn = turn.max(1);
    }
}

/// Sort key helper: tool counts sorted by count desc.
pub fn sorted_counts(m: &BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = m.iter().map(|(k, c)| (k.clone(), *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Tool names in call order (main thread only).
pub fn tool_sequence(session: &Session) -> Vec<String> {
    session.events.iter().filter(|e| e.kind == EventKind::ToolCall && !e.sidechain).filter_map(|e| e.tool.as_ref().map(|t| t.name.clone())).collect()
}
