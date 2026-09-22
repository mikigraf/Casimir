import { stats, filesTouched, finalAssistantText, userTurns } from "./model.js";
import { fmtDuration, fmtNum, pad, c } from "./util.js";
import { completeJson } from "./llm.js";

/** Compare two sessions (typically the original and a rerun). */
export function compareSessions(a, b, extra = {}) {
  const sa = stats(a);
  const sb = stats(b);
  const fa = filesTouched(a).map((f) => relativize(f.path, a.cwd));
  const fb = filesTouched(b).map((f) => relativize(f.path, b.cwd));
  const setA = new Set(fa);
  const setB = new Set(fb);
  return {
    a: describe(a, sa),
    b: describe(b, sb),
    files: {
      onlyA: fa.filter((f) => !setB.has(f)),
      onlyB: fb.filter((f) => !setA.has(f)),
      both: fa.filter((f) => setB.has(f)),
    },
    tools: mergeCounts(sa.toolsByName, sb.toolsByName),
    finalA: finalAssistantText(a),
    finalB: finalAssistantText(b),
    diffA: extra.diffA || null,
    diffB: extra.diffB || null,
    judge: extra.judge || null,
  };
}

/** Harnesses differ in logging absolute vs cwd-relative paths; compare them relative to the session cwd. */
export function relativize(p, cwd) {
  if (!p || !cwd || !p.startsWith("/")) return p;
  const base = cwd.endsWith("/") ? cwd : cwd + "/";
  return p.startsWith(base) ? p.slice(base.length) : p;
}

function describe(session, s) {
  return {
    id: session.id,
    harness: session.harness,
    model: s.model,
    title: session.title,
    turns: s.turns,
    assistantMessages: s.assistantMessages,
    toolCalls: s.toolCalls,
    toolErrors: s.toolErrors,
    errors: s.errors,
    filesTouched: s.filesTouched,
    durationMs: s.durationMs,
    usage: s.usage,
    costUsd: s.costUsd,
    finalMessageChars: s.finalMessageChars,
  };
}

function mergeCounts(x, y) {
  const names = new Set([...Object.keys(x), ...Object.keys(y)]);
  return [...names].map((n) => ({ name: n, a: x[n] || 0, b: y[n] || 0 })).sort((p, q) => q.a + q.b - (p.a + p.b));
}

function rows(report) {
  const { a, b } = report;
  return [
    ["harness", a.harness, b.harness],
    ["model", a.model || "-", b.model || "-"],
    ["turns", a.turns, b.turns],
    ["assistant messages", a.assistantMessages, b.assistantMessages],
    ["tool calls", a.toolCalls, b.toolCalls],
    ["tool errors", a.toolErrors, b.toolErrors],
    ["errors", a.errors, b.errors],
    ["files touched", a.filesTouched, b.filesTouched],
    ["duration", fmtDuration(a.durationMs), fmtDuration(b.durationMs)],
    ["input tokens", fmtNum(a.usage.input), fmtNum(b.usage.input)],
    ["output tokens", fmtNum(a.usage.output), fmtNum(b.usage.output)],
    ["cache read tokens", fmtNum(a.usage.cacheRead), fmtNum(b.usage.cacheRead)],
    ["cost (USD)", a.costUsd != null ? a.costUsd.toFixed(4) : "-", b.costUsd != null ? b.costUsd.toFixed(4) : "-"],
    ["final message chars", a.finalMessageChars, b.finalMessageChars],
  ];
}

export function renderCompareText(report, { labelA = "A", labelB = "B" } = {}) {
  const out = [];
  out.push(`${c.bold}${pad("metric", 22)}${pad(labelA, 28)}${labelB}${c.reset}`);
  for (const [k, va, vb] of rows(report)) out.push(`${pad(k, 22)}${pad(va, 28)}${vb}`);
  out.push("", `${c.bold}tool usage${c.reset}`);
  for (const t of report.tools) out.push(`${pad("  " + t.name, 22)}${pad(t.a, 28)}${t.b}`);
  out.push("", `${c.bold}files touched${c.reset}`);
  if (report.files.both.length) out.push(`  both:     ${report.files.both.join(", ")}`);
  if (report.files.onlyA.length) out.push(`  only ${labelA}:   ${report.files.onlyA.join(", ")}`);
  if (report.files.onlyB.length) out.push(`  only ${labelB}:   ${report.files.onlyB.join(", ")}`);
  if (!report.files.both.length && !report.files.onlyA.length && !report.files.onlyB.length) out.push("  (none)");
  if (report.diffB?.stat || report.diffA?.stat) {
    out.push("", `${c.bold}workspace diff stat${c.reset}`);
    if (report.diffA?.stat) out.push(`  ${labelA}:`, indent(report.diffA.stat, "    "));
    if (report.diffB?.stat) out.push(`  ${labelB}:`, indent(report.diffB.stat, "    "));
  }
  if (report.judge) {
    const j = report.judge;
    out.push("", `${c.bold}judge (${j.model})${c.reset}`);
    out.push(`  winner: ${j.winner}   scores: ${labelA}=${j.scoreA}/10  ${labelB}=${j.scoreB}/10`);
    out.push(indent(j.summary || "", "  "));
    for (const d of j.differences || []) out.push(`  - ${d}`);
  }
  return out.join("\n");
}

export function renderCompareMarkdown(report, { labelA = "original", labelB = "rerun" } = {}) {
  const md = [];
  md.push(`# Session comparison`, "");
  md.push(`| metric | ${labelA} | ${labelB} |`, `|---|---|---|`);
  for (const [k, va, vb] of rows(report)) md.push(`| ${k} | ${va} | ${vb} |`);
  md.push("", `## Tool usage`, "", `| tool | ${labelA} | ${labelB} |`, `|---|---|---|`);
  for (const t of report.tools) md.push(`| ${t.name} | ${t.a} | ${t.b} |`);
  md.push("", `## Files touched`, "");
  md.push(`- both: ${report.files.both.join(", ") || "(none)"}`);
  md.push(`- only ${labelA}: ${report.files.onlyA.join(", ") || "(none)"}`);
  md.push(`- only ${labelB}: ${report.files.onlyB.join(", ") || "(none)"}`);
  if (report.diffA?.stat || report.diffB?.stat) {
    md.push("", `## Workspace diff`, "");
    if (report.diffA?.stat) md.push(`### ${labelA}`, "", "```", report.diffA.stat, "```", "");
    if (report.diffB?.stat) md.push(`### ${labelB}`, "", "```", report.diffB.stat, "```", "");
  }
  if (report.judge) {
    const j = report.judge;
    md.push("", `## Judge (${j.model})`, "", `**Winner:** ${j.winner} — ${labelA} ${j.scoreA}/10, ${labelB} ${j.scoreB}/10`, "", j.summary || "", "");
    for (const d of j.differences || []) md.push(`- ${d}`);
  }
  md.push("", `## Final message — ${labelA}`, "", report.finalA || "_(none)_", "", `## Final message — ${labelB}`, "", report.finalB || "_(none)_", "");
  return md.join("\n");
}

function indent(text, prefix) {
  return String(text)
    .split("\n")
    .map((l) => prefix + l)
    .join("\n");
}

const JUDGE_SYSTEM = `You are an impartial reviewer comparing two runs of a coding agent on the same task.
You see the user's requests, each run's final message, the tools each run used, and the resulting workspace diff.
Judge which run better accomplished what the user asked, weighing correctness and completeness first, then
scope discipline (not doing unrequested work), then efficiency. Be concrete and cite evidence from the diffs.
Reply with a JSON object: {"winner": "A"|"B"|"tie", "scoreA": 0-10, "scoreB": 0-10, "summary": "...", "differences": ["...", ...]}`;

function clipText(s, n) {
  s = String(s ?? "");
  return s.length > n ? s.slice(0, n) + `\n… [truncated ${s.length - n} chars]` : s;
}

/** Ask an LLM to judge A vs B. Returns {winner, scoreA, scoreB, summary, differences, model}. */
export async function judgeSessions(a, b, { diffA, diffB, model, backend } = {}) {
  const turns = userTurns(a);
  const sa = stats(a);
  const sb = stats(b);
  const block = (label, s, st, diff, final) =>
    [
      `## Run ${label}: ${s.harness}${st.model ? ` / ${st.model}` : ""}`,
      `tool calls: ${st.toolCalls} (${st.toolErrors} errors); files touched: ${filesTouched(s).map((f) => f.path).join(", ") || "none"}`,
      `### Final message`,
      clipText(final, 6000),
      `### Workspace diff`,
      diff?.patch ? clipText(diff.patch, 30000) : "(no diff captured)",
    ].join("\n");
  const prompt = [
    `# User requests (in order)`,
    ...turns.map((t) => `${t.turn}. ${clipText(t.text, 4000)}`),
    "",
    block("A", a, sa, diffA, finalAssistantText(a)),
    "",
    block("B", b, sb, diffB, finalAssistantText(b)),
  ].join("\n");
  const mdl = model || undefined;
  const obj = await completeJson({ system: JUDGE_SYSTEM, prompt, model: mdl, backend });
  return {
    winner: obj.winner || "tie",
    scoreA: Number(obj.scoreA ?? 0),
    scoreB: Number(obj.scoreB ?? 0),
    summary: obj.summary || "",
    differences: Array.isArray(obj.differences) ? obj.differences : [],
    model: mdl || "default",
  };
}
