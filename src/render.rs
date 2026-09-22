//! Terminal, markdown, and list renderers.
use crate::adapters::SessionSummary;
use crate::model::{files_touched, sorted_counts, stats, tool_one_liner, Event, EventKind, Session};
use crate::util::{colors, fmt_duration, fmt_num, home_dir, indent, one_line, pad, truncate, ts_ms};

#[derive(Clone, Debug)]
pub struct RenderOpts {
    pub thinking: bool,
    pub full: bool,
    pub sidechains: bool,
    pub max_lines: usize,
    pub max_chars: usize,
    pub turn: Option<u32>,
}

impl Default for RenderOpts {
    fn default() -> Self {
        RenderOpts { thinking: false, full: false, sidechains: false, max_lines: 12, max_chars: 4000, turn: None }
    }
}

fn clip(text: &str, o: &RenderOpts, max_lines: usize) -> String {
    if o.full {
        return text.to_string();
    }
    let c = colors();
    let mut lines: Vec<&str> = text.lines().collect();
    let mut cut = false;
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        cut = true;
    }
    let mut out = lines.join("\n");
    if out.chars().count() > o.max_chars {
        out = out.chars().take(o.max_chars).collect();
        cut = true;
    }
    if cut {
        out.push_str(&format!("\n{}… (truncated; use --full){}", c.dim, c.reset));
    }
    out
}

fn clock(ev: &Event, start: Option<i64>) -> String {
    match (ts_ms(&ev.ts), start) {
        (Some(t), Some(s)) => pad(&format!("+{}", fmt_duration(t - s)), 8),
        _ => "        ".into(),
    }
}

/// Render one event (may be multi-line). None when hidden by options.
pub fn format_event(ev: &Event, o: &RenderOpts, start: Option<i64>) -> Option<String> {
    if ev.sidechain && !o.sidechains {
        return None;
    }
    let c = colors();
    let tag = format!("{}{}{} ", c.gray, clock(ev, start), c.reset);
    let side = if ev.sidechain { format!("{}[subagent] {}", c.magenta, c.reset) } else { String::new() };
    let text = ev.text_str();
    Some(match ev.kind {
        EventKind::User => format!("\n{tag}{}{}▶ user (turn {}){}\n{}\n", c.bold, c.cyan, ev.turn, c.reset, indent(text, "  ")),
        EventKind::Assistant => {
            let model = ev.model.as_ref().map(|m| format!(" {}{}{}", c.dim, m, c.reset)).unwrap_or_default();
            format!("{tag}{side}{}●{}{model}\n{}", c.green, c.reset, indent(&clip(text, o, o.max_lines), "  "))
        }
        EventKind::Thinking => {
            if !o.thinking {
                return None;
            }
            if text.trim().is_empty() {
                format!("{tag}{side}{}∴ thinking (hidden by provider){}", c.gray, c.reset)
            } else {
                format!("{tag}{side}{}∴ thinking{}\n{}{}{}", c.gray, c.reset, c.gray, indent(&clip(text, o, o.max_lines), "  "), c.reset)
            }
        }
        EventKind::ToolCall => format!("{tag}{side}{}⚙ {}{}", c.yellow, tool_one_liner(ev, if o.full { 100_000 } else { 160 }), c.reset),
        EventKind::ToolResult => {
            let r = ev.result.as_ref()?;
            let body = clip(&r.output, o, if o.full { usize::MAX } else { o.max_lines.min(6) });
            if body.trim().is_empty() {
                format!("{tag}{side}{}  ↳ (empty result){}", c.dim, c.reset)
            } else {
                format!("{}{}{}", if r.is_error { c.red } else { c.dim }, indent(&body, "    │ "), c.reset)
            }
        }
        EventKind::System => format!("{tag}{}◇ {}{} {}{}{}", c.blue, ev.subtype.as_deref().unwrap_or("system"), c.reset, c.dim, truncate(&one_line(text), 160), c.reset),
        EventKind::Error => format!("{tag}{}✖ {}{}", c.red, truncate(text, 500), c.reset),
    })
}

pub fn session_start_ms(session: &Session) -> Option<i64> {
    session.started_at.as_deref().and_then(ts_ms).or_else(|| session.events.first().and_then(|e| ts_ms(&e.ts)))
}

pub fn render_header(session: &Session) -> String {
    let c = colors();
    let s = stats(session);
    let mut lines = vec![format!("{}{}{} session {}{}{}", c.bold, session.harness(), c.reset, c.dim, session.id, c.reset)];
    if let Some(t) = &session.title {
        lines.push(format!("  title:    {t}"));
    }
    if let Some(m) = &session.model {
        lines.push(format!("  model:    {m}"));
    }
    if let Some(cwd) = &session.cwd {
        let git = match (&session.git_branch, &session.git_commit) {
            (Some(b), Some(cm)) => format!(" ({b}@{})", &cm[..cm.len().min(8)]),
            (Some(b), None) => format!(" ({b})"),
            _ => String::new(),
        };
        lines.push(format!("  cwd:      {cwd}{git}"));
    }
    if let Some(st) = &session.started_at {
        lines.push(format!("  started:  {st}  duration: {}", fmt_duration(s.duration_ms)));
    }
    lines.push(format!(
        "  turns: {}  assistant msgs: {}  tool calls: {} ({} errors)  files touched: {}",
        s.turns, s.assistant_messages, s.tool_calls, s.tool_errors, s.files_touched
    ));
    let cost = s.cost_usd.map(|c| format!("  cost ${c:.4}")).unwrap_or_default();
    lines.push(format!("  tokens: in {}  out {}  cache read {}{}", fmt_num(s.usage.input), fmt_num(s.usage.output), fmt_num(s.usage.cache_read), cost));
    if let Some(p) = &session.path {
        lines.push(format!("  {}{}{}", c.dim, p, c.reset));
    }
    lines.join("\n")
}

pub fn render_transcript(session: &Session, o: &RenderOpts) -> String {
    let start = session_start_ms(session);
    let mut out = vec![render_header(session), String::new()];
    for ev in &session.events {
        if o.turn.is_some_and(|t| ev.turn != t) {
            continue;
        }
        if let Some(s) = format_event(ev, o, start) {
            out.push(s);
        }
    }
    out.join("\n")
}

pub fn render_stats(session: &Session) -> String {
    let s = stats(session);
    let rows: Vec<(&str, String)> = vec![
        ("harness", session.harness().to_string()),
        ("model", s.model.clone().unwrap_or_else(|| "-".into())),
        ("turns", s.turns.to_string()),
        ("assistant messages", s.assistant_messages.to_string()),
        ("thinking blocks", s.thinking_blocks.to_string()),
        ("tool calls", s.tool_calls.to_string()),
        ("tool errors", s.tool_errors.to_string()),
        ("files touched", s.files_touched.to_string()),
        ("duration", fmt_duration(s.duration_ms)),
        ("input tokens", fmt_num(s.usage.input)),
        ("output tokens", fmt_num(s.usage.output)),
        ("cache read tokens", fmt_num(s.usage.cache_read)),
        ("cache write tokens", fmt_num(s.usage.cache_write)),
        ("cost (USD)", s.cost_usd.map(|c| format!("{c:.4}")).unwrap_or_else(|| "-".into())),
    ];
    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let mut lines: Vec<String> = rows.iter().map(|(k, v)| format!("{}  {v}", pad(k, w))).collect();
    lines.push(String::new());
    lines.push("tools by name:".into());
    for (n, k) in sorted_counts(&s.tools_by_name) {
        lines.push(format!("  {} {k}", pad(&n, 24)));
    }
    let files = files_touched(session);
    if !files.is_empty() {
        lines.push(String::new());
        lines.push("files touched:".into());
        for f in files {
            lines.push(format!("  {}  ({})", f.path, f.ops.join(",")));
        }
    }
    lines.join("\n")
}

fn quote(text: &str) -> String {
    text.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n")
}

/// Markdown export of a session.
pub fn render_markdown(session: &Session, o: &RenderOpts) -> String {
    let o = RenderOpts { max_lines: 40, ..o.clone() };
    let s = stats(session);
    let mut md: Vec<String> = Vec::new();
    md.push(format!("# {}", session.title.clone().unwrap_or_else(|| session.id.clone())));
    md.push(String::new());
    md.push(format!("- harness: {}", session.harness()));
    md.push(format!("- session: {}", session.id));
    if let Some(m) = &session.model {
        md.push(format!("- model: {m}"));
    }
    if let Some(cwd) = &session.cwd {
        md.push(format!("- cwd: {cwd}{}", session.git_branch.as_ref().map(|b| format!(" ({b})")).unwrap_or_default()));
    }
    if let Some(st) = &session.started_at {
        md.push(format!("- started: {st}, duration {}", fmt_duration(s.duration_ms)));
    }
    md.push(format!("- turns: {}, tool calls: {}, tokens in/out: {}/{}", s.turns, s.tool_calls, fmt_num(s.usage.input), fmt_num(s.usage.output)));
    md.push(String::new());
    for ev in &session.events {
        if ev.sidechain && !o.sidechains {
            continue;
        }
        let text = ev.text_str();
        match ev.kind {
            EventKind::User => {
                md.push(format!("## Turn {} — user", ev.turn));
                md.push(String::new());
                md.push(quote(text));
                md.push(String::new());
            }
            EventKind::Assistant => {
                md.push(format!("**assistant**{}:", ev.model.as_ref().map(|m| format!(" ({m})")).unwrap_or_default()));
                md.push(String::new());
                md.push(text.to_string());
                md.push(String::new());
            }
            EventKind::Thinking => {
                if o.thinking && !text.trim().is_empty() {
                    md.extend(["<details><summary>thinking</summary>".into(), String::new(), text.to_string(), String::new(), "</details>".into(), String::new()]);
                }
            }
            EventKind::ToolCall => md.push(format!("- 🔧 `{}`", tool_one_liner(ev, 200).replace('`', "'"))),
            EventKind::ToolResult => {
                if let Some(r) = &ev.result {
                    let body = if o.full { r.output.clone() } else { clip(&r.output, &o, 20) };
                    let body = strip_ansi(&body);
                    if !body.trim().is_empty() {
                        md.extend([String::new(), "  ```".into(), indent(&body, "  "), "  ```".into(), String::new()]);
                    }
                }
            }
            EventKind::System => {
                md.push(format!("> _{}_: {}", ev.subtype.as_deref().unwrap_or("system"), truncate(&one_line(text), 200)));
                md.push(String::new());
            }
            EventKind::Error => {
                md.push(format!("> ❌ {text}"));
                md.push(String::new());
            }
        }
    }
    md.join("\n")
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn render_session_list(items: &[SessionSummary]) -> String {
    if items.is_empty() {
        return "(no sessions found)".into();
    }
    let c = colors();
    let home = home_dir().display().to_string();
    let mut lines = vec![format!("{}{}{}{}{}title{}", c.bold, pad("harness", 12), pad("id", 38), pad("updated", 21), pad("cwd", 34), c.reset)];
    for it in items {
        let cwd = it.cwd.as_deref().map(|d| truncate(&d.replacen(&home, "~", 1), 32)).unwrap_or_default();
        let updated = it.updated_at.chars().take(19).collect::<String>().replace('T', " ");
        lines.push(format!("{}{}{}{}{}", pad(it.harness.as_str(), 12), pad(&it.id, 38), pad(&updated, 21), pad(&cwd, 34), truncate(&it.title, 60)));
    }
    lines.join("\n")
}
