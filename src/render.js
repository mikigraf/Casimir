import { c, truncate, fmtDuration, fmtNum, pad } from "./util.js";
import { toolOneLiner, stats, filesTouched, userTurns } from "./model.js";

const DEFAULTS = { thinking: false, full: false, sidechains: false, maxLines: 12, maxChars: 4000, width: 100 };

function indent(text, prefix) {
  return String(text)
    .split("\n")
    .map((l) => prefix + l)
    .join("\n");
}

function clip(text, o) {
  if (o.full) return text;
  let lines = String(text ?? "").split("\n");
  let cut = false;
  if (lines.length > o.maxLines) {
    lines = lines.slice(0, o.maxLines);
    cut = true;
  }
  let out = lines.join("\n");
  if (out.length > o.maxChars) {
    out = out.slice(0, o.maxChars);
    cut = true;
  }
  return cut ? out + `\n${c.dim}… (truncated; use --full)${c.reset}` : out;
}

function clock(ev, start) {
  const t = Date.parse(ev.ts) - start;
  return Number.isNaN(t) ? "        " : pad("+" + fmtDuration(t), 8);
}

/** Render one event to a terminal string (may be multi-line). Returns null when hidden. */
export function formatEvent(ev, opts = {}, ctx = {}) {
  const o = { ...DEFAULTS, ...opts };
  if (ev.sidechain && !o.sidechains) return null;
  const start = ctx.start ?? Date.parse(ev.ts);
  const tag = `${c.gray}${clock(ev, start)}${c.reset} `;
  const side = ev.sidechain ? `${c.magenta}[subagent] ${c.reset}` : "";
  switch (ev.kind) {
    case "user":
      return `\n${tag}${c.bold}${c.cyan}▶ user (turn ${ev.turn})${c.reset}\n${indent(ev.text, "  ")}\n`;
    case "assistant":
      return `${tag}${side}${c.green}●${c.reset}${ev.model ? ` ${c.dim}${ev.model}${c.reset}` : ""}\n${indent(clip(ev.text, o), "  ")}`;
    case "thinking":
      if (!o.thinking) return null;
      if (!ev.text?.trim()) return `${tag}${side}${c.gray}∴ thinking (hidden by provider)${c.reset}`;
      return `${tag}${side}${c.gray}∴ thinking${c.reset}\n${c.gray}${indent(clip(ev.text, o), "  ")}${c.reset}`;
    case "tool_call":
      return `${tag}${side}${c.yellow}⚙ ${toolOneLiner(ev, o.full ? 100000 : 160)}${c.reset}`;
    case "tool_result": {
      const err = ev.result?.isError;
      const body = clip(ev.result?.output ?? "", { ...o, maxLines: o.full ? 1e9 : Math.min(o.maxLines, 6) });
      if (!body.trim()) return `${tag}${side}${c.dim}  ↳ (empty result)${c.reset}`;
      return `${(err ? c.red : c.dim)}${indent(body, "    │ ")}${c.reset}`;
    }
    case "system":
      return `${tag}${c.blue}◇ ${ev.subtype || "system"}${c.reset} ${c.dim}${truncate(String(ev.text ?? "").replace(/\s+/g, " "), 160)}${c.reset}`;
    case "error":
      return `${tag}${c.red}✖ ${truncate(ev.text, 500)}${c.reset}`;
    default:
      return null;
  }
}

export function renderHeader(session) {
  const s = stats(session);
  const lines = [];
  lines.push(`${c.bold}${session.harness}${c.reset} session ${c.dim}${session.id}${c.reset}`);
  if (session.title) lines.push(`  title:    ${session.title}`);
  if (session.model) lines.push(`  model:    ${session.model}`);
  if (session.cwd) lines.push(`  cwd:      ${session.cwd}${session.gitBranch ? ` (${session.gitBranch}${session.gitCommit ? "@" + session.gitCommit.slice(0, 8) : ""})` : ""}`);
  if (session.startedAt) lines.push(`  started:  ${session.startedAt}  duration: ${fmtDuration(s.durationMs)}`);
  lines.push(`  turns: ${s.turns}  assistant msgs: ${s.assistantMessages}  tool calls: ${s.toolCalls} (${s.toolErrors} errors)  files touched: ${s.filesTouched}`);
  lines.push(`  tokens: in ${fmtNum(s.usage.input)}  out ${fmtNum(s.usage.output)}  cache read ${fmtNum(s.usage.cacheRead)}${s.costUsd != null ? `  cost $${s.costUsd.toFixed(4)}` : ""}`);
  if (session.path) lines.push(`  ${c.dim}${session.path}${c.reset}`);
  return lines.join("\n");
}

export function renderTranscript(session, opts = {}) {
  const start = Date.parse(session.startedAt || session.events[0]?.ts);
  const out = [renderHeader(session), ""];
  for (const ev of session.events) {
    if (opts.turn && ev.turn !== opts.turn) continue;
    const s = formatEvent(ev, opts, { start });
    if (s != null) out.push(s);
  }
  return out.join("\n");
}

export function renderStats(session) {
  const s = stats(session);
  const rows = [
    ["harness", s.harness],
    ["model", s.model || "-"],
    ["turns", s.turns],
    ["assistant messages", s.assistantMessages],
    ["thinking blocks", s.thinkingBlocks],
    ["tool calls", s.toolCalls],
    ["tool errors", s.toolErrors],
    ["files touched", s.filesTouched],
    ["duration", fmtDuration(s.durationMs)],
    ["input tokens", fmtNum(s.usage.input)],
    ["output tokens", fmtNum(s.usage.output)],
    ["cache read tokens", fmtNum(s.usage.cacheRead)],
    ["cache write tokens", fmtNum(s.usage.cacheWrite)],
    ["cost (USD)", s.costUsd != null ? s.costUsd.toFixed(4) : "-"],
  ];
  const w = Math.max(...rows.map((r) => r[0].length));
  const lines = rows.map(([k, v]) => `${pad(k, w)}  ${v}`);
  lines.push("", "tools by name:");
  for (const [n, k] of Object.entries(s.toolsByName).sort((a, b) => b[1] - a[1])) lines.push(`  ${pad(n, 24)} ${k}`);
  const files = filesTouched(session);
  if (files.length) {
    lines.push("", "files touched:");
    for (const f of files) lines.push(`  ${f.path}  (${f.ops.join(",")})`);
  }
  return lines.join("\n");
}

/** Markdown export of a session. */
export function renderMarkdown(session, opts = {}) {
  const o = { ...DEFAULTS, maxLines: 40, ...opts };
  const s = stats(session);
  const md = [];
  md.push(`# ${session.title || session.id}`, "");
  md.push(`- harness: ${session.harness}`);
  md.push(`- session: ${session.id}`);
  if (session.model) md.push(`- model: ${session.model}`);
  if (session.cwd) md.push(`- cwd: ${session.cwd}${session.gitBranch ? ` (${session.gitBranch})` : ""}`);
  if (session.startedAt) md.push(`- started: ${session.startedAt}, duration ${fmtDuration(s.durationMs)}`);
  md.push(`- turns: ${s.turns}, tool calls: ${s.toolCalls}, tokens in/out: ${fmtNum(s.usage.input)}/${fmtNum(s.usage.output)}`);
  md.push("");
  for (const ev of session.events) {
    if (ev.sidechain && !o.sidechains) continue;
    switch (ev.kind) {
      case "user":
        md.push(`## Turn ${ev.turn} — user`, "", quote(ev.text), "");
        break;
      case "assistant":
        md.push(`**assistant**${ev.model ? ` (${ev.model})` : ""}:`, "", ev.text, "");
        break;
      case "thinking":
        if (o.thinking && ev.text?.trim()) md.push(`<details><summary>thinking</summary>`, "", ev.text, "", `</details>`, "");
        break;
      case "tool_call":
        md.push(`- 🔧 \`${toolOneLiner(ev, 200).replace(/`/g, "'")}\``);
        break;
      case "tool_result": {
        const body = o.full ? ev.result.output : clip(ev.result.output, { ...o, maxLines: 20 }).replace(/\x1b\[[0-9;]*m/g, "");
        if (body.trim()) md.push("", "  ```", indent(body, "  "), "  ```", "");
        break;
      }
      case "system":
        md.push(`> _${ev.subtype}_: ${truncate(String(ev.text ?? "").replace(/\s+/g, " "), 200)}`, "");
        break;
      case "error":
        md.push(`> ❌ ${ev.text}`, "");
        break;
    }
  }
  return md.join("\n");
}

function quote(text) {
  return String(text)
    .split("\n")
    .map((l) => "> " + l)
    .join("\n");
}

export function renderSessionList(items) {
  if (!items.length) return "(no sessions found)";
  const lines = [];
  lines.push(`${c.bold}${pad("harness", 12)}${pad("id", 38)}${pad("updated", 21)}${pad("cwd", 34)}title${c.reset}`);
  for (const it of items) {
    const cwd = it.cwd ? truncate(it.cwd.replace(process.env.HOME || "", "~"), 32) : "";
    lines.push(`${pad(it.harness, 12)}${pad(it.id, 38)}${pad((it.updatedAt || "").slice(0, 19).replace("T", " "), 21)}${pad(cwd, 34)}${truncate(it.title || "", 60)}`);
  }
  return lines.join("\n");
}

export { userTurns };
