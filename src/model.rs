//! Normalized session model shared by all harness adapters.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
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
        "shell" | "shell_command" | "local_shell" | "exec_command" | "container.exec" | "command_execution" | "run_shell_command" | "bash" | "exec" | "execute" | "run_command" | "powershell" | "terminal" | "run_terminal_cmd" | "execute_command" => {
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
    /// Counts per canonical action kind.
    #[serde(default)]
    pub actions: BTreeMap<String, usize>,
    #[serde(default)]
    pub anti_patterns: AntiPatterns,
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
    s.actions = action_counts(session);
    s.anti_patterns = anti_patterns(session);
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

// ---------------------------------------------------------------------------------------------
// Canonical action taxonomy and trajectory anti-patterns
//
// After "process metrics for coding agents" (arXiv 2607.06184): every tool call is mapped onto a
// small, harness-independent action vocabulary so trajectories from different harnesses can be
// compared, and a few mechanically detectable anti-patterns are labelled with deterministic rules.

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    FileRead,
    FileWrite,
    Search,
    Command,
    Plan,
    Navigate,
    Fetch,
    AgentSpawn,
    Reason,
    Other,
}

impl ActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::FileRead => "file_read",
            ActionKind::FileWrite => "file_write",
            ActionKind::Search => "search",
            ActionKind::Command => "command",
            ActionKind::Plan => "plan",
            ActionKind::Navigate => "navigate",
            ActionKind::Fetch => "fetch",
            ActionKind::AgentSpawn => "agent_spawn",
            ActionKind::Reason => "reason",
            ActionKind::Other => "other",
        }
    }
}

/// One classified step of a trajectory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub turn: u32,
    pub kind: ActionKind,
    /// A command that validates work (tests, build, lint, type-check).
    pub validation: bool,
    /// Canonical file the action reads or writes, when known.
    pub file: Option<String>,
    pub is_error: bool,
    pub tool: String,
}

const VALIDATION_MARKERS: &[&str] = &[
    "pytest", "python -m pytest", "python3 -m pytest", "unittest", "tox", "nox",
    "cargo test", "cargo check", "cargo build", "cargo clippy", "cargo fmt --check",
    "npm test", "npm run test", "npm run build", "npm run lint", "npm run typecheck", "pnpm test", "pnpm build", "yarn test", "yarn build", "bun test",
    "jest", "vitest", "mocha", "node --test", "tsc", "eslint", "prettier --check",
    "go test", "go build", "go vet", "golangci-lint",
    "mvn test", "mvn verify", "gradle test", "gradlew test", "./gradlew",
    "make test", "make check", "make build", "ctest", "cmake --build",
    "rspec", "rake test", "bundle exec rspec", "phpunit", "composer test", "dotnet test", "dotnet build",
    "mix test", "sbt test", "swift test", "flutter test", "ruff", "flake8", "mypy", "pyright", "black --check",
];

/// Whether a shell command validates the work (runs tests, builds, lints, type-checks).
pub fn is_validation_command(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    VALIDATION_MARKERS.iter().any(|m| c.contains(m))
}

fn first_path_arg(cmd: &str, after: &[&str]) -> Option<String> {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        if after.contains(t) {
            return toks.iter().skip(i + 1).find(|a| !a.starts_with('-') && !a.starts_with('|') && !a.starts_with('>')).map(|s| s.trim_matches(|c| c == '"' || c == '\'').to_string());
        }
    }
    None
}

/// Classify a shell command into a canonical action.
pub fn classify_shell(cmd: &str) -> (ActionKind, bool, Option<String>) {
    // unwrap `bash -lc "<cmd>"`, `sh -c <cmd>` and friends
    let mut trimmed = cmd.trim();
    loop {
        let mut toks = trimmed.splitn(3, char::is_whitespace);
        let (Some(a), Some(b), Some(rest)) = (toks.next(), toks.next(), toks.next()) else { break };
        let shell = a.rsplit('/').next().unwrap_or(a);
        if matches!(shell, "bash" | "sh" | "zsh" | "dash" | "fish") && b.starts_with('-') && b.contains('c') {
            trimmed = rest.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        } else {
            break;
        }
    }
    let first = trimmed.split_whitespace().next().unwrap_or("").rsplit('/').next().unwrap_or("");
    let has_write = trimmed.contains('>') || trimmed.contains("tee ") || trimmed.contains("sed -i") || trimmed.starts_with("mv ") || trimmed.starts_with("cp ") || trimmed.starts_with("rm ") || trimmed.starts_with("mkdir ") || trimmed.starts_with("touch ") || trimmed.contains("git apply") || trimmed.contains("patch ");
    if has_write && !trimmed.contains("2>&1") || (has_write && (trimmed.contains("cat >") || trimmed.contains("tee ") || trimmed.contains("sed -i") || trimmed.starts_with("mv ") || trimmed.starts_with("rm ") || trimmed.starts_with("touch "))) {
        let file = first_path_arg(trimmed, &[">", ">>", "tee", "touch", "rm", "-i"]).or_else(|| trimmed.split('>').nth(1).and_then(|r| r.split_whitespace().next()).map(String::from));
        return (ActionKind::FileWrite, false, file);
    }
    if is_validation_command(trimmed) {
        return (ActionKind::Command, true, None);
    }
    match first {
        "grep" | "rg" | "ag" | "ack" | "find" | "fd" | "locate" | "ls" | "tree" | "git" if first != "git" || trimmed.starts_with("git grep") || trimmed.starts_with("git log") || trimmed.starts_with("git ls-files") || trimmed.starts_with("git status") || trimmed.starts_with("git diff") || trimmed.starts_with("git show") || trimmed.starts_with("git blame") => (ActionKind::Search, false, None),
        "cat" | "head" | "tail" | "less" | "more" | "bat" | "sed" | "awk" | "wc" | "nl" | "od" | "xxd" | "jq" | "yq" => (ActionKind::FileRead, false, first_path_arg(trimmed, &[first]).filter(|p| !p.starts_with('-'))),
        "cd" | "pushd" | "popd" | "pwd" => (ActionKind::Navigate, false, None),
        "curl" | "wget" | "http" | "gh" => (ActionKind::Fetch, false, None),
        _ => (ActionKind::Command, false, None),
    }
}

/// Map a tool call onto the canonical taxonomy. Harness-specific tool names are handled by name;
/// shell-like tools are classified by their command text.
pub fn classify_action(e: &Event) -> Option<Action> {
    if e.kind == EventKind::Thinking {
        return Some(Action { turn: e.turn, kind: ActionKind::Reason, validation: false, file: None, is_error: false, tool: "thinking".into() });
    }
    let tool = e.tool.as_ref()?;
    let inp = &tool.input;
    let s = |k: &str| inp.get(k).and_then(Value::as_str).map(String::from);
    let file = || s("file_path").or_else(|| s("filePath")).or_else(|| s("path")).or_else(|| s("notebook_path")).or_else(|| s("target_file")).or_else(|| s("file")).or_else(|| s("absolute_path"));
    let name = tool.name.as_str();
    let lower = name.to_ascii_lowercase();
    let (kind, validation, f) = if let Some(cmd) = shell_command(e) {
        classify_shell(&cmd)
    } else {
        match lower.as_str() {
            // Claude Code
            "read" | "notebookread" => (ActionKind::FileRead, false, file()),
            "edit" | "write" | "multiedit" | "notebookedit" => (ActionKind::FileWrite, false, file()),
            "glob" | "grep" | "ls" => (ActionKind::Search, false, None),
            "task" | "agent" | "spawn_agent" | "spawnagent" => (ActionKind::AgentSpawn, false, None),
            "webfetch" | "websearch" | "web_search" | "web_fetch" | "google_web_search" | "fetch" | "web-fetch" => (ActionKind::Fetch, false, None),
            "todowrite" | "todoread" | "enterplanmode" | "exitplanmode" | "update_plan" | "plan" | "todo_list" => (ActionKind::Plan, false, None),
            // Codex
            "apply_patch" | "file_change" => (ActionKind::FileWrite, false, None),
            "read_file" | "view_image" | "view" | "read_many_files" | "cat" => (ActionKind::FileRead, false, file()),
            // Copilot / Gemini / generic
            "create" | "write_file" | "replace" | "str_replace_editor" | "str_replace_based_edit_tool" | "edit_file" | "create_file" | "write_to_file" | "insert" | "save_memory" => (ActionKind::FileWrite, false, file()),
            "search" | "search_file_content" | "list_directory" | "list_dir" | "codebase_search" | "grep_search" | "file_search" | "find" => (ActionKind::Search, false, None),
            "run_shell_command" | "bash" | "shell" | "exec" | "execute" | "run_command" | "powershell" | "terminal" => (ActionKind::Command, false, None),
            "ask_user" | "askuserquestion" | "ask" => (ActionKind::Other, false, None),
            _ => {
                if lower.contains("read") || lower.contains("view") || lower.contains("open") {
                    (ActionKind::FileRead, false, file())
                } else if lower.contains("write") || lower.contains("edit") || lower.contains("create") || lower.contains("patch") || lower.contains("replace") {
                    (ActionKind::FileWrite, false, file())
                } else if lower.contains("search") || lower.contains("grep") || lower.contains("glob") || lower.contains("list") || lower.contains("find") {
                    (ActionKind::Search, false, None)
                } else if lower.contains("fetch") || lower.contains("http") || lower.contains("browse") {
                    (ActionKind::Fetch, false, None)
                } else if lower.contains("agent") || lower.contains("subtask") || lower.contains("delegate") {
                    (ActionKind::AgentSpawn, false, None)
                } else if lower.contains("plan") || lower.contains("todo") {
                    (ActionKind::Plan, false, None)
                } else {
                    (ActionKind::Other, false, None)
                }
            }
        }
    };
    Some(Action { turn: e.turn, kind, validation, file: f, is_error: false, tool: name.to_string() })
}

/// The main-thread action stream of a session (tool calls with their error status, plus reasoning).
pub fn actions(session: &Session) -> Vec<Action> {
    let mut out: Vec<Action> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for e in &session.events {
        if e.sidechain {
            continue;
        }
        match e.kind {
            EventKind::ToolCall | EventKind::Thinking => {
                if let Some(a) = classify_action(e) {
                    if let Some(t) = &e.tool {
                        by_id.insert(t.id.clone(), out.len());
                    }
                    out.push(a);
                }
            }
            EventKind::ToolResult => {
                if let Some(r) = &e.result {
                    if r.is_error {
                        if let Some(i) = by_id.get(&r.id) {
                            out[*i].is_error = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Deterministic trajectory anti-patterns (arXiv 2607.06184 rules).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AntiPatterns {
    /// Maximal runs of >= 10 consecutive search/read actions with no write and no validation command.
    pub search_loops: usize,
    /// Files read >= 3 times within a 10-action window without an intervening write to that file.
    pub reread_churn_files: Vec<String>,
    /// No validation command in the overlap of the post-last-source-write region and the final 5 actions.
    pub verification_skip: bool,
    /// Tool calls that returned an error, as a share of all tool calls.
    pub failed_action_share: f64,
    /// Search + read actions as a share of all tool actions.
    pub exploration_share: f64,
    /// Number of tool actions (excluding reasoning).
    pub tool_actions: usize,
    /// Index (0-based) of the first file write, if any.
    pub first_write_index: Option<usize>,
}

impl AntiPatterns {
    pub fn any(&self) -> bool {
        self.search_loops > 0 || !self.reread_churn_files.is_empty() || self.verification_skip
    }
}

pub fn anti_patterns(session: &Session) -> AntiPatterns {
    anti_patterns_of(&actions(session))
}

pub fn anti_patterns_of(acts: &[Action]) -> AntiPatterns {
    let acts: Vec<&Action> = acts.iter().filter(|a| a.kind != ActionKind::Reason).collect();
    let n = acts.len();
    let mut ap = AntiPatterns { tool_actions: n, ..Default::default() };
    if n == 0 {
        return ap;
    }
    // search loops: maximal runs of search/read without write or validation
    let mut run = 0usize;
    for a in &acts {
        let exploratory = matches!(a.kind, ActionKind::Search | ActionKind::FileRead);
        let breaks = a.kind == ActionKind::FileWrite || a.validation;
        if exploratory {
            run += 1;
        } else if breaks {
            if run >= 10 {
                ap.search_loops += 1;
            }
            run = 0;
        }
        // other kinds (command without validation, fetch, plan…) neither extend nor break the run
    }
    if run >= 10 {
        ap.search_loops += 1;
    }
    // re-read churn: same file read >= 3 times in a 10-action window with no intervening write
    let mut churn: Vec<String> = Vec::new();
    for (i, a) in acts.iter().enumerate() {
        if a.kind != ActionKind::FileRead {
            continue;
        }
        let Some(f) = &a.file else { continue };
        if churn.contains(f) {
            continue;
        }
        let end = (i + 10).min(n);
        let mut reads = 0;
        for b in &acts[i..end] {
            if b.kind == ActionKind::FileWrite && b.file.as_deref() == Some(f.as_str()) {
                break;
            }
            if b.kind == ActionKind::FileRead && b.file.as_deref() == Some(f.as_str()) {
                reads += 1;
            }
        }
        if reads >= 3 {
            churn.push(f.clone());
        }
    }
    ap.reread_churn_files = churn;
    // verification skip
    let last_write = acts.iter().rposition(|a| a.kind == ActionKind::FileWrite);
    ap.first_write_index = acts.iter().position(|a| a.kind == ActionKind::FileWrite);
    if let Some(lw) = last_write {
        let start = lw.max(n.saturating_sub(5));
        ap.verification_skip = !acts[start..].iter().any(|a| a.validation);
    }
    ap.failed_action_share = acts.iter().filter(|a| a.is_error).count() as f64 / n as f64;
    ap.exploration_share = acts.iter().filter(|a| matches!(a.kind, ActionKind::Search | ActionKind::FileRead)).count() as f64 / n as f64;
    ap
}

/// Counts per canonical action kind (including reasoning blocks).
pub fn action_counts(session: &Session) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for a in actions(session) {
        *m.entry(a.kind.as_str().to_string()).or_insert(0) += 1;
    }
    m
}

/// Canonical action-kind sequence (tool actions only), for cross-harness trajectory comparison.
pub fn action_sequence(session: &Session) -> Vec<String> {
    actions(session).into_iter().filter(|a| a.kind != ActionKind::Reason).map(|a| if a.validation { "validate".to_string() } else { a.kind.as_str().to_string() }).collect()
}
