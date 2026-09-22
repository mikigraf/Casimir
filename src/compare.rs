//! Side-by-side comparison of two sessions, plus an optional LLM judge.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::llm::{complete_json, LlmOpts};
use crate::model::{files_touched, final_assistant_text, stats, user_turns, Harness, Session, Usage};
use crate::util::{colors, fmt_duration, fmt_num, indent, pad};
use crate::workspace::Diff;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SideStats {
    pub id: String,
    pub harness: Option<Harness>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub turns: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_errors: usize,
    pub errors: usize,
    pub files_touched: usize,
    pub duration_ms: i64,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
    pub final_message_chars: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileSets {
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
    pub both: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolRow {
    pub name: String,
    pub a: usize,
    pub b: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Judgement {
    pub winner: String,
    pub score_a: f64,
    pub score_b: f64,
    pub summary: String,
    pub differences: Vec<String>,
    pub model: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub a: SideStats,
    pub b: SideStats,
    pub files: FileSets,
    pub tools: Vec<ToolRow>,
    pub final_a: String,
    pub final_b: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_a: Option<Diff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_b: Option<Diff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<Judgement>,
}

/// Harnesses differ in logging absolute vs cwd-relative paths; compare relative to the session cwd.
pub fn relativize(p: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd else { return p.to_string() };
    if !p.starts_with('/') {
        return p.to_string();
    }
    let base = if cwd.ends_with('/') { cwd.to_string() } else { format!("{cwd}/") };
    p.strip_prefix(&base).map(String::from).unwrap_or_else(|| p.to_string())
}

fn describe(session: &Session) -> SideStats {
    let s = stats(session);
    SideStats {
        id: session.id.clone(),
        harness: session.harness,
        model: s.model,
        title: session.title.clone(),
        turns: s.turns,
        assistant_messages: s.assistant_messages,
        tool_calls: s.tool_calls,
        tool_errors: s.tool_errors,
        errors: s.errors,
        files_touched: s.files_touched,
        duration_ms: s.duration_ms,
        usage: s.usage,
        cost_usd: s.cost_usd,
        final_message_chars: s.final_message_chars,
    }
}

pub fn compare_sessions(a: &Session, b: &Session, diff_a: Option<Diff>, diff_b: Option<Diff>, judge: Option<Judgement>) -> Report {
    let fa: Vec<String> = files_touched(a).iter().map(|f| relativize(&f.path, a.cwd.as_deref())).collect();
    let fb: Vec<String> = files_touched(b).iter().map(|f| relativize(&f.path, b.cwd.as_deref())).collect();
    let sa = stats(a);
    let sb = stats(b);
    let mut names: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (n, c) in &sa.tools_by_name {
        names.entry(n.clone()).or_default().0 = *c;
    }
    for (n, c) in &sb.tools_by_name {
        names.entry(n.clone()).or_default().1 = *c;
    }
    let mut tools: Vec<ToolRow> = names.into_iter().map(|(name, (a, b))| ToolRow { name, a, b }).collect();
    tools.sort_by(|x, y| (y.a + y.b).cmp(&(x.a + x.b)).then(x.name.cmp(&y.name)));
    Report {
        a: describe(a),
        b: describe(b),
        files: FileSets {
            only_a: fa.iter().filter(|f| !fb.contains(f)).cloned().collect(),
            only_b: fb.iter().filter(|f| !fa.contains(f)).cloned().collect(),
            both: fa.iter().filter(|f| fb.contains(f)).cloned().collect(),
        },
        tools,
        final_a: final_assistant_text(a, None),
        final_b: final_assistant_text(b, None),
        diff_a,
        diff_b,
        judge,
    }
}

fn rows(r: &Report) -> Vec<(&'static str, String, String)> {
    let (a, b) = (&r.a, &r.b);
    let cost = |c: Option<f64>| c.map(|c| format!("{c:.4}")).unwrap_or_else(|| "-".into());
    vec![
        ("harness", a.harness.map(|h| h.to_string()).unwrap_or_default(), b.harness.map(|h| h.to_string()).unwrap_or_default()),
        ("model", a.model.clone().unwrap_or_else(|| "-".into()), b.model.clone().unwrap_or_else(|| "-".into())),
        ("turns", a.turns.to_string(), b.turns.to_string()),
        ("assistant messages", a.assistant_messages.to_string(), b.assistant_messages.to_string()),
        ("tool calls", a.tool_calls.to_string(), b.tool_calls.to_string()),
        ("tool errors", a.tool_errors.to_string(), b.tool_errors.to_string()),
        ("errors", a.errors.to_string(), b.errors.to_string()),
        ("files touched", a.files_touched.to_string(), b.files_touched.to_string()),
        ("duration", fmt_duration(a.duration_ms), fmt_duration(b.duration_ms)),
        ("input tokens", fmt_num(a.usage.input), fmt_num(b.usage.input)),
        ("output tokens", fmt_num(a.usage.output), fmt_num(b.usage.output)),
        ("cache read tokens", fmt_num(a.usage.cache_read), fmt_num(b.usage.cache_read)),
        ("cost (USD)", cost(a.cost_usd), cost(b.cost_usd)),
        ("final message chars", a.final_message_chars.to_string(), b.final_message_chars.to_string()),
    ]
}

pub fn render_compare_text(r: &Report, label_a: &str, label_b: &str) -> String {
    let c = colors();
    let mut out = vec![format!("{}{}{}{}{}", c.bold, pad("metric", 22), pad(label_a, 28), label_b, c.reset)];
    for (k, va, vb) in rows(r) {
        out.push(format!("{}{}{vb}", pad(k, 22), pad(&va, 28)));
    }
    out.push(String::new());
    out.push(format!("{}tool usage{}", c.bold, c.reset));
    for t in &r.tools {
        out.push(format!("{}{}{}", pad(&format!("  {}", t.name), 22), pad(&t.a.to_string(), 28), t.b));
    }
    out.push(String::new());
    out.push(format!("{}files touched{}", c.bold, c.reset));
    if !r.files.both.is_empty() {
        out.push(format!("  both:     {}", r.files.both.join(", ")));
    }
    if !r.files.only_a.is_empty() {
        out.push(format!("  only {label_a}:   {}", r.files.only_a.join(", ")));
    }
    if !r.files.only_b.is_empty() {
        out.push(format!("  only {label_b}:   {}", r.files.only_b.join(", ")));
    }
    if r.files.both.is_empty() && r.files.only_a.is_empty() && r.files.only_b.is_empty() {
        out.push("  (none)".into());
    }
    let stat_a = r.diff_a.as_ref().filter(|d| !d.stat.is_empty());
    let stat_b = r.diff_b.as_ref().filter(|d| !d.stat.is_empty());
    if stat_a.is_some() || stat_b.is_some() {
        out.push(String::new());
        out.push(format!("{}workspace diff stat{}", c.bold, c.reset));
        if let Some(d) = stat_a {
            out.push(format!("  {label_a}:"));
            out.push(indent(&d.stat, "    "));
        }
        if let Some(d) = stat_b {
            out.push(format!("  {label_b}:"));
            out.push(indent(&d.stat, "    "));
        }
    }
    if let Some(j) = &r.judge {
        out.push(String::new());
        out.push(format!("{}judge ({}){}", c.bold, j.model, c.reset));
        out.push(format!("  winner: {}   scores: {label_a}={}/10  {label_b}={}/10", j.winner, j.score_a, j.score_b));
        out.push(indent(&j.summary, "  "));
        for d in &j.differences {
            out.push(format!("  - {d}"));
        }
    }
    out.join("\n")
}

pub fn render_compare_markdown(r: &Report, label_a: &str, label_b: &str) -> String {
    let mut md: Vec<String> = vec!["# Session comparison".into(), String::new()];
    md.push(format!("| metric | {label_a} | {label_b} |"));
    md.push("|---|---|---|".into());
    for (k, va, vb) in rows(r) {
        md.push(format!("| {k} | {va} | {vb} |"));
    }
    md.extend([String::new(), "## Tool usage".into(), String::new(), format!("| tool | {label_a} | {label_b} |"), "|---|---|---|".into()]);
    for t in &r.tools {
        md.push(format!("| {} | {} | {} |", t.name, t.a, t.b));
    }
    let list = |v: &Vec<String>| if v.is_empty() { "(none)".to_string() } else { v.join(", ") };
    md.extend([String::new(), "## Files touched".into(), String::new()]);
    md.push(format!("- both: {}", list(&r.files.both)));
    md.push(format!("- only {label_a}: {}", list(&r.files.only_a)));
    md.push(format!("- only {label_b}: {}", list(&r.files.only_b)));
    let stat_a = r.diff_a.as_ref().filter(|d| !d.stat.is_empty());
    let stat_b = r.diff_b.as_ref().filter(|d| !d.stat.is_empty());
    if stat_a.is_some() || stat_b.is_some() {
        md.extend([String::new(), "## Workspace diff".into(), String::new()]);
        if let Some(d) = stat_a {
            md.extend([format!("### {label_a}"), String::new(), "```".into(), d.stat.clone(), "```".into(), String::new()]);
        }
        if let Some(d) = stat_b {
            md.extend([format!("### {label_b}"), String::new(), "```".into(), d.stat.clone(), "```".into(), String::new()]);
        }
    }
    if let Some(j) = &r.judge {
        md.extend([String::new(), format!("## Judge ({})", j.model), String::new()]);
        md.push(format!("**Winner:** {} — {label_a} {}/10, {label_b} {}/10", j.winner, j.score_a, j.score_b));
        md.extend([String::new(), j.summary.clone(), String::new()]);
        for d in &j.differences {
            md.push(format!("- {d}"));
        }
    }
    let or_none = |s: &str| if s.is_empty() { "_(none)_".to_string() } else { s.to_string() };
    md.extend([String::new(), format!("## Final message — {label_a}"), String::new(), or_none(&r.final_a), String::new(), format!("## Final message — {label_b}"), String::new(), or_none(&r.final_b), String::new()]);
    md.join("\n")
}

const JUDGE_SYSTEM: &str = "You are an impartial reviewer comparing two runs of a coding agent on the same task.
You see the user's requests, each run's final message, the tools each run used, and the resulting workspace diff.
Judge which run better accomplished what the user asked, weighing correctness and completeness first, then
scope discipline (not doing unrequested work), then efficiency. Be concrete and cite evidence from the diffs.
Reply with a JSON object: {\"winner\": \"A\"|\"B\"|\"tie\", \"scoreA\": 0-10, \"scoreB\": 0-10, \"summary\": \"...\", \"differences\": [\"...\", ...]}";

fn clip_text(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count > n {
        format!("{}\n… [truncated {} chars]", s.chars().take(n).collect::<String>(), count - n)
    } else {
        s.to_string()
    }
}

fn run_block(label: &str, s: &Session, diff: Option<&Diff>) -> String {
    let st = stats(s);
    let files = files_touched(s).iter().map(|f| f.path.clone()).collect::<Vec<_>>().join(", ");
    [
        format!("## Run {label}: {}{}", s.harness(), st.model.as_ref().map(|m| format!(" / {m}")).unwrap_or_default()),
        format!("tool calls: {} ({} errors); files touched: {}", st.tool_calls, st.tool_errors, if files.is_empty() { "none".into() } else { files }),
        "### Final message".into(),
        clip_text(&final_assistant_text(s, None), 6000),
        "### Workspace diff".into(),
        diff.filter(|d| !d.patch.is_empty()).map(|d| clip_text(&d.patch, 30000)).unwrap_or_else(|| "(no diff captured)".into()),
    ]
    .join("\n")
}

/// Ask an LLM to judge A vs B.
pub fn judge_sessions(a: &Session, b: &Session, diff_a: Option<&Diff>, diff_b: Option<&Diff>, llm: &LlmOpts) -> Result<Judgement> {
    let mut prompt: Vec<String> = vec!["# User requests (in order)".into()];
    for t in user_turns(a) {
        prompt.push(format!("{}. {}", t.turn, clip_text(&t.text, 4000)));
    }
    prompt.push(String::new());
    prompt.push(run_block("A", a, diff_a));
    prompt.push(String::new());
    prompt.push(run_block("B", b, diff_b));
    let obj = complete_json(JUDGE_SYSTEM, &prompt.join("\n"), llm)?;
    let num = |k: &str| obj.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    Ok(Judgement {
        winner: obj.get("winner").and_then(Value::as_str).unwrap_or("tie").to_string(),
        score_a: num("scoreA"),
        score_b: num("scoreB"),
        summary: obj.get("summary").and_then(Value::as_str).unwrap_or("").to_string(),
        differences: obj.get("differences").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default(),
        model: llm.model.clone().unwrap_or_else(|| crate::llm::DEFAULT_MODEL.into()),
    })
}
