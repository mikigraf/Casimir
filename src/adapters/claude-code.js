/**
 * Claude Code adapter.
 *
 * Session logs: $CLAUDE_CONFIG_DIR (default ~/.claude)/projects/<cwd-slug>/<session-id>.jsonl
 * Each line is a record; `user` / `assistant` records carry an API-shaped `message`.
 * Rerun: `claude -p --output-format stream-json --verbose` emits records with the same
 * `message` shape, so one mapper serves both the on-disk log and the live stream.
 */
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { readJsonl, readJsonlHead, walk, homeDir, cleanEnv, truncate, firstLine } from "../util.js";
import { renumberTurns } from "../model.js";

export const name = "claude-code";

export function configDir() {
  return process.env.CLAUDE_CONFIG_DIR || path.join(homeDir(), ".claude");
}

const SKIP_TYPES = new Set([
  "queue-operation",
  "atis-latch",
  "last-prompt",
  "attachment",
  "file-history-snapshot",
  "progress",
]);

function stripReminders(text) {
  return String(text ?? "")
    .replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, "")
    .trim();
}

function textOfContent(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((b) => b && b.type === "text" && typeof b.text === "string")
    .map((b) => b.text)
    .join("\n");
}

function mapUsage(u) {
  if (!u) return undefined;
  return {
    input: u.input_tokens || 0,
    output: u.output_tokens || 0,
    cacheRead: u.cache_read_input_tokens || 0,
    cacheWrite: u.cache_creation_input_tokens || 0,
    reasoning: u.output_tokens_details?.thinking_tokens || 0,
  };
}

/** Classify a user-role record. Returns {kind, subtype?, text} or null for "nothing to emit". */
function classifyUserText(text, rec) {
  const raw = String(text ?? "");
  if (rec?.isCompactSummary) return { kind: "system", subtype: "compact", text: raw };
  const cmd = raw.match(/<command-name>([\s\S]*?)<\/command-name>/);
  if (cmd) {
    const args = raw.match(/<command-args>([\s\S]*?)<\/command-args>/)?.[1] ?? "";
    return { kind: "system", subtype: "command", text: `${cmd[1].trim()} ${args.trim()}`.trim() };
  }
  const out = raw.match(/<local-command-stdout>([\s\S]*?)<\/local-command-stdout>/);
  if (out) return { kind: "system", subtype: "command-output", text: out[1].trim() };
  if (/^\[Request interrupted by user/.test(raw.trim())) return { kind: "system", subtype: "interrupt", text: raw.trim() };
  const clean = stripReminders(raw);
  if (!clean) return null;
  if (rec?.isMeta) return { kind: "system", subtype: "meta", text: clean };
  return { kind: "user", text: clean };
}

/**
 * Convert one user/assistant record (log line or stream-json line) into events.
 * `turn` is the current turn number; the caller increments it when a `user` event is returned.
 */
export function recordToEvents(rec, turn) {
  const events = [];
  const ts = rec.timestamp || new Date().toISOString();
  const base = { turn, ts };
  if (rec.isSidechain) base.sidechain = true;
  if (rec.agentId) base.agentId = rec.agentId;
  const msg = rec.message || {};

  if (rec.type === "user") {
    const content = msg.content;
    if (Array.isArray(content)) {
      for (const b of content) {
        if (b?.type === "tool_result") {
          let output = b.content;
          if (Array.isArray(output)) output = textOfContent(output);
          if (output != null && typeof output !== "string") output = JSON.stringify(output);
          events.push({
            ...base,
            kind: "tool_result",
            result: { id: b.tool_use_id, output: output ?? "", isError: !!b.is_error },
          });
        }
      }
    }
    const text = textOfContent(content);
    const cls = classifyUserText(text, rec);
    if (cls) events.push({ ...base, ...cls });
    return events;
  }

  if (rec.type === "assistant") {
    const usage = mapUsage(msg.usage);
    const synthetic = typeof msg.model === "string" && msg.model.startsWith("<"); // e.g. "<synthetic>": harness-generated API error text
    const model = synthetic ? undefined : msg.model;
    let first = true;
    for (const b of msg.content || []) {
      if (synthetic && b.type === "text" && b.text?.trim()) {
        events.push({ ...base, kind: "error", text: b.text });
        continue;
      }
      const ev = { ...base, msgId: msg.id, model };
      if (first && usage) {
        ev.usage = usage;
        first = false;
      }
      if (b.type === "text") {
        if (!b.text?.trim()) continue;
        events.push({ ...ev, kind: "assistant", text: b.text });
      } else if (b.type === "thinking" || b.type === "redacted_thinking") {
        events.push({ ...ev, kind: "thinking", text: b.thinking || "" });
      } else if (b.type === "tool_use") {
        events.push({ ...ev, kind: "tool_call", tool: { id: b.id, name: b.name, input: b.input } });
      }
    }
    if (msg.stop_reason === "refusal") events.push({ ...base, kind: "error", text: "model refused (stop_reason=refusal)" });
    return events;
  }
  return events;
}

/** Build a normalized session from parsed log records. */
export function parseRecords(records, { path: file } = {}) {
  const session = { id: null, harness: name, path: file, events: [] };
  let turn = 0;
  const toolNames = new Map();
  for (const rec of records) {
    if (!rec || typeof rec !== "object") continue;
    const t = rec.type;
    if (SKIP_TYPES.has(t)) continue;
    if (!session.id && rec.sessionId) session.id = rec.sessionId;
    if (t === "ai-title" && rec.aiTitle) session.title = rec.aiTitle;
    else if (t === "custom-title" && rec.customTitle) session.title = rec.customTitle;
    else if (t === "summary" && rec.summary && !session.title) session.title = rec.summary;
    if (t === "system") {
      const text = typeof rec.content === "string" ? rec.content : rec.subtype || "";
      if (rec.subtype === "compact_boundary" || /compact/i.test(rec.subtype || "")) {
        session.events.push({ turn: Math.max(turn, 1), ts: rec.timestamp, kind: "system", subtype: "compact", text: "context compacted" });
      } else if (rec.level === "error") {
        session.events.push({ turn: Math.max(turn, 1), ts: rec.timestamp, kind: "error", text });
      }
      continue;
    }
    if (t !== "user" && t !== "assistant") continue;

    if (!session.cwd && rec.cwd) session.cwd = rec.cwd;
    if (!session.gitBranch && rec.gitBranch) session.gitBranch = rec.gitBranch;
    if (!session.version && rec.version) session.version = rec.version;
    if (!session.permissionMode && rec.permissionMode) session.permissionMode = rec.permissionMode;
    if (!session.startedAt && rec.timestamp) session.startedAt = rec.timestamp;
    if (rec.timestamp) session.endedAt = rec.timestamp;

    const evs = recordToEvents(rec, Math.max(turn, 1));
    for (const ev of evs) {
      if (ev.kind === "user" && !ev.sidechain) {
        turn++;
        ev.turn = turn;
      }
      if (ev.kind === "tool_call") toolNames.set(ev.tool.id, ev.tool.name);
      if (ev.kind === "tool_result" && !ev.result.name) ev.result.name = toolNames.get(ev.result.id);
      if (ev.model && !session.model) session.model = ev.model;
      session.events.push(ev);
    }
  }
  if (!session.title) {
    const firstUser = session.events.find((e) => e.kind === "user");
    if (firstUser) session.title = truncate(firstLine(firstUser.text), 80);
  }
  if (!session.id && file) session.id = path.basename(file, ".jsonl");
  return session;
}

export function parseFile(file) {
  return parseRecords(readJsonl(file), { path: file });
}

/** Cheap metadata for listing (reads the head of the file only). */
export function listSessions() {
  const root = path.join(configDir(), "projects");
  const files = walk(root, (p) => p.endsWith(".jsonl") && path.dirname(path.dirname(p)) === root);
  const out = [];
  for (const file of files) {
    let st;
    try {
      st = fs.statSync(file);
    } catch {
      continue;
    }
    if (st.size === 0) continue;
    const head = readJsonlHead(file);
    const first = head.find((r) => r.type === "user" && typeof r.message?.content === "string");
    const anyRec = head.find((r) => r.sessionId) || {};
    const title = head.find((r) => r.type === "ai-title")?.aiTitle;
    const prompt = first ? stripReminders(first.message.content) : "";
    if (!first && !title) continue; // no real user turn recorded yet
    out.push({
      harness: name,
      id: anyRec.sessionId || path.basename(file, ".jsonl"),
      path: file,
      cwd: first?.cwd || anyRec.cwd,
      gitBranch: first?.gitBranch,
      startedAt: first?.timestamp || anyRec.timestamp,
      updatedAt: st.mtime.toISOString(),
      title: title || truncate(firstLine(prompt), 80),
      sizeBytes: st.size,
    });
  }
  return out;
}

/** Locate the on-disk log for a session id written by this harness. */
export function findLogById(id) {
  const root = path.join(configDir(), "projects");
  const hits = walk(root, (p) => path.basename(p) === `${id}.jsonl`);
  return hits[0] || null;
}

function permissionArgs(mode) {
  if (!mode || mode === "auto" || mode === "bypassPermissions" || mode === "bypass") return ["--dangerously-skip-permissions"];
  return ["--permission-mode", mode];
}

/**
 * Run one user turn through `claude -p` and return normalized events.
 * opts: {prompt, cwd, model, sessionId (new session), resume (existing id), permissionMode,
 *        allowedTools, extraArgs, onEvent, turn, bin}
 */
export function runTurn(opts) {
  const bin = opts.bin || process.env.CASIMIR_CLAUDE_BIN || "claude";
  const args = ["-p", "--output-format", "stream-json", "--verbose"];
  if (opts.model) args.push("--model", opts.model);
  if (opts.resume) args.push("--resume", opts.resume);
  else args.push("--session-id", opts.sessionId || randomUUID());
  args.push(...permissionArgs(opts.permissionMode));
  if (opts.allowedTools?.length) args.push("--allowedTools", ...opts.allowedTools);
  if (opts.extraArgs?.length) args.push(...opts.extraArgs);

  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { cwd: opts.cwd, env: cleanEnv(), stdio: ["pipe", "pipe", "pipe"] });
    const events = [];
    const raw = [];
    let sessionId = opts.resume || opts.sessionId || null;
    let result = null;
    let stderr = "";
    let buf = "";
    const turn = opts.turn || 1;
    const toolNames = new Map();
    const handle = (line) => {
      const t = line.trim();
      if (!t) return;
      let rec;
      try {
        rec = JSON.parse(t);
      } catch {
        return;
      }
      raw.push(rec);
      if (rec.session_id) sessionId = rec.session_id;
      if (rec.type === "result") {
        result = rec;
        if (rec.is_error && rec.result) {
          const ev = { turn, ts: new Date().toISOString(), kind: "error", text: String(rec.result) };
          events.push(ev);
          opts.onEvent?.(ev);
        }
        return;
      }
      if (rec.type !== "user" && rec.type !== "assistant") return;
      rec.timestamp = rec.timestamp || new Date().toISOString();
      for (const ev of recordToEvents(rec, turn)) {
        if (ev.kind === "user") continue; // our own prompt echo (or reminder text); we record it ourselves
        if (ev.kind === "tool_call") toolNames.set(ev.tool.id, ev.tool.name);
        if (ev.kind === "tool_result") ev.result.name = toolNames.get(ev.result.id);
        events.push(ev);
        opts.onEvent?.(ev);
      }
    };
    child.stdout.on("data", (d) => {
      buf += d.toString();
      const lines = buf.split("\n");
      buf = lines.pop();
      lines.forEach(handle);
    });
    child.stderr.on("data", (d) => {
      stderr += d.toString();
    });
    child.on("error", reject);
    child.on("close", (code) => {
      if (buf) handle(buf);
      if (code !== 0 && !result) {
        return reject(new Error(`claude exited with code ${code}: ${truncate(stderr.trim(), 2000)}`));
      }
      resolve({
        sessionId,
        events,
        raw,
        result,
        model: result?.modelUsage ? Object.keys(result.modelUsage)[0] : events.find((e) => e.model)?.model,
        costUsd: result?.total_cost_usd ?? null,
        isError: !!result?.is_error,
        stderr,
      });
    });
    child.stdin.end(opts.prompt);
    opts.onChild?.(child);
  });
}

export function detect(firstRecord) {
  return !!firstRecord && (("sessionId" in firstRecord && "type" in firstRecord) || "parentUuid" in firstRecord);
}
