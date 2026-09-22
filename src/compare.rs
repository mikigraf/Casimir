//! Side-by-side comparison of two sessions, end-state similarity, and an order-swapped LLM judge.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use crate::llm::{complete_json, effective_model, LlmOpts};
use crate::model::{action_sequence, actions, files_touched, final_assistant_text, stats, tool_sequence, user_turns, ActionKind, AntiPatterns, Harness, Session, Usage};
use crate::util::{colors, fmt_duration, fmt_num, indent, pad};
use crate::workspace::{parse_patch, Diff};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SideStats {
    pub id: String,
    pub harness: Option<Harness>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub turns: usize,
    pub simulated_turns: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_errors: usize,
    pub errors: usize,
    pub files_touched: usize,
    pub duration_ms: i64,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
    pub final_message_chars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulator_model: Option<String>,
    #[serde(default)]
    pub actions: BTreeMap<String, usize>,
    #[serde(default)]
    pub anti_patterns: AntiPatterns,
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

/// One judge call in one candidate order. Scores are already mapped back to the real A and B.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct JudgePass {
    /// "AB" (A shown first) or "BA" (B shown first)
    pub order: String,
    pub winner: String,
    pub score_a: f64,
    pub score_b: f64,
    pub summary: String,
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
    /// The two orderings disagreed on the winner; the combined verdict is a tie.
    #[serde(default)]
    pub order_sensitive: bool,
    /// Scores within one point: position bias is worst on close comparisons.
    #[serde(default)]
    pub close: bool,
    #[serde(default)]
    pub passes: Vec<JudgePass>,
}

/// How similar two workspace end states are (0..1). Files: Jaccard over changed paths.
/// Content: mean per-file Jaccard over added/removed lines. Score multiplies the two.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct EndState {
    pub score: f64,
    pub files_jaccard: f64,
    pub content_similarity: f64,
    /// Share of the reference (A) diff's added/removed lines reproduced by B. Recall rather than a
    /// symmetric measure, so harmless extra edits in B are not penalized (after arXiv 2606.17454).
    #[serde(default)]
    pub recall: f64,
    pub files_a: usize,
    pub files_b: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_a: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_b: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub a: SideStats,
    pub b: SideStats,
    pub files: FileSets,
    pub tools: Vec<ToolRow>,
    /// LCS ratio over tool-name sequences; descriptive only, not a correctness signal.
    pub tool_sequence_similarity: f64,
    /// LCS ratio over canonical action kinds (comparable across harnesses).
    #[serde(default)]
    pub action_sequence_similarity: f64,
    /// First user turn whose canonical action sequence differs (None = identical trajectories).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_divergent_turn: Option<u32>,
    pub final_a: String,
    pub final_b: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_state: Option<EndState>,
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
        simulated_turns: s.simulated_turns,
        assistant_messages: s.assistant_messages,
        tool_calls: s.tool_calls,
        tool_errors: s.tool_errors,
        errors: s.errors,
        files_touched: s.files_touched,
        duration_ms: s.duration_ms,
        usage: s.usage,
        cost_usd: s.cost_usd,
        final_message_chars: s.final_message_chars,
        simulator_model: session.simulator.as_ref().map(|si| si.model.clone()),
        actions: s.actions,
        anti_patterns: s.anti_patterns,
    }
}

/// Canonical action-kind sequence per user turn.
pub fn action_sequence_by_turn(session: &Session) -> BTreeMap<u32, Vec<String>> {
    let mut m: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for a in actions(session) {
        if a.kind == ActionKind::Reason {
            continue;
        }
        m.entry(a.turn).or_default().push(if a.validation { "validate".into() } else { a.kind.as_str().into() });
    }
    m
}

/// First user turn whose canonical action sequence differs between two sessions (None = identical).
pub fn first_divergent_turn(a: &Session, b: &Session) -> Option<u32> {
    let sa = action_sequence_by_turn(a);
    let sb = action_sequence_by_turn(b);
    let ta = user_turns(a).len() as u32;
    let tb = user_turns(b).len() as u32;
    let empty: Vec<String> = Vec::new();
    for t in 1..=ta.max(tb) {
        if t > ta || t > tb {
            return Some(t);
        }
        if sa.get(&t).unwrap_or(&empty) != sb.get(&t).unwrap_or(&empty) {
            return Some(t);
        }
    }
    None
}

fn jaccard<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

/// Compare two workspace diffs by end state rather than by trajectory.
pub fn end_state_similarity(a: &Diff, b: &Diff) -> EndState {
    let pa = parse_patch(&a.patch);
    let pb = parse_patch(&b.patch);
    let fa: BTreeSet<String> = pa.keys().cloned().collect();
    let fb: BTreeSet<String> = pb.keys().cloned().collect();
    let files_jaccard = jaccard(&fa, &fb);
    let union: BTreeSet<&String> = fa.union(&fb).collect();
    let content_similarity = if union.is_empty() {
        1.0
    } else {
        let mut total = 0.0;
        for f in &union {
            let la: BTreeSet<String> = pa.get(*f).map(|c| c.added.iter().map(|l| format!("+{l}")).chain(c.removed.iter().map(|l| format!("-{l}"))).collect()).unwrap_or_default();
            let lb: BTreeSet<String> = pb.get(*f).map(|c| c.added.iter().map(|l| format!("+{l}")).chain(c.removed.iter().map(|l| format!("-{l}"))).collect()).unwrap_or_default();
            total += jaccard(&la, &lb);
        }
        total / union.len() as f64
    };
    let mut ref_lines = 0usize;
    let mut hit = 0usize;
    for (f, c) in &pa {
        let la: BTreeSet<String> = c.added.iter().map(|l| format!("+{l}")).chain(c.removed.iter().map(|l| format!("-{l}"))).collect();
        let lb: BTreeSet<String> = pb.get(f).map(|c| c.added.iter().map(|l| format!("+{l}")).chain(c.removed.iter().map(|l| format!("-{l}"))).collect()).unwrap_or_default();
        ref_lines += la.len();
        hit += la.intersection(&lb).count();
    }
    let recall = if ref_lines == 0 { 1.0 } else { hit as f64 / ref_lines as f64 };
    EndState { score: files_jaccard * content_similarity, files_jaccard, content_similarity, recall, files_a: fa.len(), files_b: fb.len(), source_a: a.source.clone(), source_b: b.source.clone() }
}

/// Longest-common-subsequence ratio between two sequences (2·lcs / (|a|+|b|)).
pub fn sequence_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    for x in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        prev = cur;
    }
    2.0 * prev[b.len()] as f64 / (a.len() + b.len()) as f64
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
    let end_state = match (&diff_a, &diff_b) {
        (Some(da), Some(db)) => Some(end_state_similarity(da, db)),
        _ => None,
    };
    Report {
        a: describe(a),
        b: describe(b),
        files: FileSets {
            only_a: fa.iter().filter(|f| !fb.contains(f)).cloned().collect(),
            only_b: fb.iter().filter(|f| !fa.contains(f)).cloned().collect(),
            both: fa.iter().filter(|f| fb.contains(f)).cloned().collect(),
        },
        tools,
        tool_sequence_similarity: sequence_similarity(&tool_sequence(a), &tool_sequence(b)),
        action_sequence_similarity: sequence_similarity(&action_sequence(a), &action_sequence(b)),
        first_divergent_turn: first_divergent_turn(a, b),
        final_a: final_assistant_text(a, None),
        final_b: final_assistant_text(b, None),
        end_state,
        diff_a,
        diff_b,
        judge,
    }
}

fn rows(r: &Report) -> Vec<(&'static str, String, String)> {
    let (a, b) = (&r.a, &r.b);
    let cost = |c: Option<f64>| c.map(|c| format!("{c:.4}")).unwrap_or_else(|| "-".into());
    let mut rows = vec![
        ("harness", a.harness.map(|h| h.to_string()).unwrap_or_default(), b.harness.map(|h| h.to_string()).unwrap_or_default()),
        ("model", a.model.clone().unwrap_or_else(|| "-".into()), b.model.clone().unwrap_or_else(|| "-".into())),
        ("turns", a.turns.to_string(), b.turns.to_string()),
    ];
    if a.simulated_turns > 0 || b.simulated_turns > 0 || a.simulator_model.is_some() || b.simulator_model.is_some() {
        rows.push(("simulated turns", a.simulated_turns.to_string(), b.simulated_turns.to_string()));
        rows.push(("simulator model", a.simulator_model.clone().unwrap_or_else(|| "-".into()), b.simulator_model.clone().unwrap_or_else(|| "-".into())));
    }
    rows.extend([
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
    ]);
    rows
}

fn process_rows(r: &Report) -> Vec<(&'static str, String, String)> {
    let (a, b) = (&r.a.anti_patterns, &r.b.anti_patterns);
    let yn = |v: bool| if v { "yes".to_string() } else { "no".to_string() };
    let pct = |v: f64| format!("{:.0}%", v * 100.0);
    let kinds = ["search", "file_read", "file_write", "command", "fetch", "agent_spawn", "plan", "reason"];
    let mut rows: Vec<(&'static str, String, String)> = vec![
        ("search loops (≥10 reads, no write)", a.search_loops.to_string(), b.search_loops.to_string()),
        ("re-read churn (files)", a.reread_churn_files.len().to_string(), b.reread_churn_files.len().to_string()),
        ("verification skipped", yn(a.verification_skip), yn(b.verification_skip)),
        ("failed-action share", pct(a.failed_action_share), pct(b.failed_action_share)),
        ("exploration share", pct(a.exploration_share), pct(b.exploration_share)),
    ];
    for k in kinds {
        let va = r.a.actions.get(k).copied().unwrap_or(0);
        let vb = r.b.actions.get(k).copied().unwrap_or(0);
        if va + vb > 0 {
            let label: &'static str = match k {
                "search" => "  actions: search",
                "file_read" => "  actions: file_read",
                "file_write" => "  actions: file_write",
                "command" => "  actions: command",
                "fetch" => "  actions: fetch",
                "agent_spawn" => "  actions: agent_spawn",
                "plan" => "  actions: plan",
                _ => "  actions: reason",
            };
            rows.push((label, va.to_string(), vb.to_string()));
        }
    }
    rows
}

fn end_state_lines(r: &Report, label_a: &str, label_b: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(es) = &r.end_state {
        out.push(format!("  end-state similarity: {:.2}  (files {:.2} × content {:.2}; {} vs {} changed files)  recall of {label_a}'s changes: {:.2}", es.score, es.files_jaccard, es.content_similarity, es.files_a, es.files_b, es.recall));
        if let Some(s) = &es.source_a {
            out.push(format!("    {label_a} diff: {s}"));
        }
        if let Some(s) = &es.source_b {
            out.push(format!("    {label_b} diff: {s}"));
        }
    } else {
        out.push("  end-state similarity: n/a (need a workspace diff for both sides)".into());
    }
    out.push(format!("  tool-sequence similarity: {:.2}  action-sequence similarity: {:.2}  (descriptive only)", r.tool_sequence_similarity, r.action_sequence_similarity));
    out.push(match r.first_divergent_turn {
        Some(t) => format!("  first divergent turn: {t}"),
        None => "  first divergent turn: none (identical action sequences)".into(),
    });
    out
}

pub fn render_compare_text(r: &Report, label_a: &str, label_b: &str) -> String {
    let c = colors();
    let mut out = vec![format!("{}{}{}{}{}", c.bold, pad("metric", 22), pad(label_a, 28), label_b, c.reset)];
    for (k, va, vb) in rows(r) {
        out.push(format!("{}{}{vb}", pad(k, 22), pad(&va, 28)));
    }
    out.push(String::new());
    out.push(format!("{}outcome{}", c.bold, c.reset));
    out.extend(end_state_lines(r, label_a, label_b));
    out.push(String::new());
    out.push(format!("{}process (trajectory anti-patterns, arXiv 2607.06184 rules){}", c.bold, c.reset));
    for (k, va, vb) in process_rows(r) {
        out.push(format!("  {}{}{vb}", pad(k, 36), pad(&va, 14)));
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
        out.push(format!("  winner: {}   scores: {label_a}={:.1}/10  {label_b}={:.1}/10", j.winner, j.score_a, j.score_b));
        if j.order_sensitive {
            out.push(format!("  {}⚠ order-sensitive: the two candidate orders disagreed, so the verdict is a tie{}", c.yellow, c.reset));
        }
        if j.close {
            out.push(format!("  {}⚠ close call: scores within one point; position bias is strongest here{}", c.yellow, c.reset));
        }
        for p in &j.passes {
            out.push(format!("  {}order {}: winner {}, {label_a}={:.1} {label_b}={:.1}{}", c.dim, p.order, p.winner, p.score_a, p.score_b, c.reset));
        }
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
    md.extend([String::new(), "## Outcome".into(), String::new()]);
    for l in end_state_lines(r, label_a, label_b) {
        md.push(format!("- {}", l.trim()));
    }
    md.extend([String::new(), "## Process".into(), String::new(), format!("| metric | {label_a} | {label_b} |"), "|---|---|---|".into()]);
    for (k, va, vb) in process_rows(r) {
        md.push(format!("| {} | {va} | {vb} |", k.trim()));
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
        md.push(format!("**Winner:** {} — {label_a} {:.1}/10, {label_b} {:.1}/10", j.winner, j.score_a, j.score_b));
        if j.order_sensitive {
            md.push(String::new());
            md.push("> ⚠ Order-sensitive: the two candidate orders disagreed on the winner, so the combined verdict is a tie.".into());
        }
        if j.close {
            md.push(String::new());
            md.push("> ⚠ Close call: scores within one point. Position bias is strongest on close comparisons.".into());
        }
        if !j.passes.is_empty() {
            md.extend([String::new(), "| order | winner | score A | score B |".into(), "|---|---|---|---|".into()]);
            for p in &j.passes {
                md.push(format!("| {} | {} | {:.1} | {:.1} |", p.order, p.winner, p.score_a, p.score_b));
            }
        }
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
Give each run an absolute score from 0 to 10 first, independently, then decide the winner; a tie is acceptable.
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

/// One judge call with `first` shown as Run A and `second` as Run B. Returns (winner, score_first, score_second, summary, differences).
fn judge_once(turns_block: &str, first: (&Session, Option<&Diff>), second: (&Session, Option<&Diff>), llm: &LlmOpts) -> Result<(String, f64, f64, String, Vec<String>)> {
    let prompt = [turns_block.to_string(), String::new(), run_block("A", first.0, first.1), String::new(), run_block("B", second.0, second.1)].join("\n");
    let obj = complete_json(JUDGE_SYSTEM, &prompt, llm)?;
    let num = |k: &str| obj.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    Ok((
        obj.get("winner").and_then(Value::as_str).unwrap_or("tie").to_string(),
        num("scoreA"),
        num("scoreB"),
        obj.get("summary").and_then(Value::as_str).unwrap_or("").to_string(),
        obj.get("differences").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default(),
    ))
}

fn swap_label(w: &str) -> String {
    match w {
        "A" => "B".into(),
        "B" => "A".into(),
        other => other.to_string(),
    }
}

/// Ask an LLM to judge A vs B in both candidate orders. Verdicts that flip on order swap become a tie
/// flagged `order_sensitive`; scores are averaged across the two passes.
pub fn judge_sessions(a: &Session, b: &Session, diff_a: Option<&Diff>, diff_b: Option<&Diff>, llm: &LlmOpts) -> Result<Judgement> {
    let mut turns_block: Vec<String> = vec!["# User requests (in order)".into()];
    for t in user_turns(a) {
        turns_block.push(format!("{}. {}", t.turn, clip_text(&t.text, 4000)));
    }
    let tb = turns_block.join("\n");
    let (w1, sa1, sb1, sum1, diffs1) = judge_once(&tb, (a, diff_a), (b, diff_b), llm)?;
    let (w2_raw, sb2, sa2, sum2, diffs2) = judge_once(&tb, (b, diff_b), (a, diff_a), llm)?;
    let w2 = swap_label(&w2_raw);
    let passes = vec![
        JudgePass { order: "AB".into(), winner: w1.clone(), score_a: sa1, score_b: sb1, summary: sum1.clone() },
        JudgePass { order: "BA".into(), winner: w2.clone(), score_a: sa2, score_b: sb2, summary: sum2.clone() },
    ];
    let score_a = (sa1 + sa2) / 2.0;
    let score_b = (sb1 + sb2) / 2.0;
    let order_sensitive = w1 != w2;
    let winner = if order_sensitive { "tie".to_string() } else { w1.clone() };
    let mut differences = diffs1;
    for d in diffs2 {
        if !differences.contains(&d) {
            differences.push(d);
        }
    }
    let summary = if order_sensitive { format!("[order AB] {sum1}\n[order BA] {sum2}") } else { sum1 };
    Ok(Judgement { winner, score_a, score_b, summary, differences, model: effective_model(llm), order_sensitive, close: (score_a - score_b).abs() <= 1.0, passes })
}
