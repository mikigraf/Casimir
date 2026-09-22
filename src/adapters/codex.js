/**
 * OpenAI Codex CLI adapter.
 *
 * Session rollouts: $CODEX_HOME (default ~/.codex)/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl
 * Lines: {timestamp, type: session_meta|turn_context|response_item|event_msg|..., payload}
 * `response_item` payloads are the model-facing items (messages, reasoning, function calls);
 * `event_msg` payloads are UI events (token counts, task lifecycle) and mostly duplicate them.
 * Rerun: `codex exec --json` prints thread/turn/item events as JSONL; after the run we re-read
 * the rollout the CLI wrote so the rerun has the same fidelity as the original.
 */
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { readJsonl, readJsonlHead, walk, homeDir, cleanEnv, truncate, firstLine } from "../util.js";

export const name = "codex";

export function codexHome() {
  return process.env.CODEX_HOME || path.join(homeDir(), ".codex");
}

// Messages Codex injects with role=user that are not typed by the human.
const INJECTED_PREFIXES = [
  "<environment_context>",
  "<user_instructions>",
  "<permissions_instructions>",
  "<skills_instructions>",
  "<apps_instructions>",
  "<collaboration_mode",
  "<multi_agent",
  "<turn_aborted>",
  "<context_window_guidance>",
  "<mcp_instructions>",
  "# AGENTS.md instructions",
];

function isInjected(text) {
  const t = text.trimStart();
  return INJECTED_PREFIXES.some((p) => t.startsWith(p));
}

function textOf(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((b) => b && typeof b.text === "string" && (b.type === "input_text" || b.type === "output_text" || b.type === "text"))
    .map((b) => b.text)
    .join("\n");
}

function parseArgs(s) {
  if (s == null) return {};
  if (typeof s !== "string") return s;
  try {
    return JSON.parse(s);
  } catch {
    return { raw: s };
  }
}

/** Codex tool outputs are often strings that embed exit codes, or JSON with {output, metadata}. */
function parseOutput(out) {
  let text = out;
  let isError = false;
  if (out && typeof out === "object") {
    if (typeof out.output === "string") text = out.output;
    else if (Array.isArray(out.content)) text = out.content.map((c) => c.text ?? "").join("\n");
    else text = JSON.stringify(out);
    const ec = out.metadata?.exit_code;
    if (typeof ec === "number" && ec !== 0) isError = true;
  } else if (typeof out === "string") {
    const t = out.trimStart();
    if (t.startsWith("{")) {
      try {
        const j = JSON.parse(t);
        if (typeof j.output === "string") {
          text = j.output;
          const ec = j.metadata?.exit_code;
          if (typeof ec === "number" && ec !== 0) isError = true;
        }
      } catch {
        /* keep raw */
      }
    }
    const m = /^Exit code: (\d+)/m.exec(text);
    if (m && Number(m[1]) !== 0) isError = true;
  }
  return { output: text == null ? "" : String(text), isError };
}

function mapUsage(u) {
  if (!u) return undefined;
  return {
    input: u.input_tokens || 0,
    output: u.output_tokens || 0,
    cacheRead: u.cached_input_tokens || 0,
    cacheWrite: 0,
    reasoning: u.reasoning_output_tokens || 0,
  };
}

/** Build a normalized session from parsed rollout records. */
export function parseRecords(records, { path: file } = {}) {
  const session = { id: null, harness: name, path: file, events: [] };
  let turn = 0;
  const toolNames = new Map();
  const push = (ev) => {
    ev.turn = Math.max(turn, 1);
    session.events.push(ev);
  };
  for (const rec of records) {
    if (!rec || typeof rec !== "object") continue;
    const ts = rec.timestamp;
    const p = rec.payload || {};
    if (ts) {
      if (!session.startedAt) session.startedAt = ts;
      session.endedAt = ts;
    }
    switch (rec.type) {
      case "session_meta": {
        session.id = p.id || p.session_id || session.id;
        session.cwd = p.cwd;
        session.version = p.cli_version;
        session.startedAt = p.timestamp || session.startedAt;
        if (p.git) {
          session.gitCommit = p.git.commit_hash;
          session.gitBranch = p.git.branch;
        }
        session.source = p.source;
        break;
      }
      case "turn_context": {
        if (p.model) session.model = session.model || p.model;
        if (p.cwd && !session.cwd) session.cwd = p.cwd;
        if (p.sandbox_policy && !session.sandbox) session.sandbox = p.sandbox_policy.type || p.sandbox_policy.mode || JSON.stringify(p.sandbox_policy);
        if (p.approval_policy && !session.permissionMode) session.permissionMode = typeof p.approval_policy === "string" ? p.approval_policy : JSON.stringify(p.approval_policy);
        if (p.collaboration_mode?.settings?.reasoning_effort) session.effort = p.collaboration_mode.settings.reasoning_effort;
        break;
      }
      case "response_item": {
        switch (p.type) {
          case "message": {
            const text = textOf(p.content);
            if (p.role === "user") {
              if (!text.trim()) break;
              if (text.trimStart().startsWith("<turn_aborted>")) {
                push({ ts, kind: "system", subtype: "interrupt", text: "turn aborted by user" });
              } else if (text.trimStart().startsWith("<user_shell_command>")) {
                push({ ts, kind: "system", subtype: "command", text: text.replace(/<\/?user_shell_command>/g, "").trim() });
              } else if (isInjected(text)) {
                // harness-injected context: not a human turn
              } else {
                turn++;
                push({ ts, kind: "user", text });
              }
            } else if (p.role === "assistant") {
              if (text.trim()) push({ ts, kind: "assistant", text, model: session.model });
            }
            // developer/system roles are harness prompts; skip
            break;
          }
          case "reasoning": {
            const parts = [];
            for (const s of p.summary || []) if (s?.text) parts.push(s.text);
            for (const c of p.content || []) if (c?.text) parts.push(c.text);
            push({ ts, kind: "thinking", text: parts.join("\n\n") });
            break;
          }
          case "function_call": {
            const id = p.call_id || p.id;
            toolNames.set(id, p.name);
            push({ ts, kind: "tool_call", tool: { id, name: p.name, input: parseArgs(p.arguments) } });
            break;
          }
          case "custom_tool_call": {
            const id = p.call_id || p.id;
            toolNames.set(id, p.name);
            push({ ts, kind: "tool_call", tool: { id, name: p.name, input: { patch: p.input } } });
            break;
          }
          case "local_shell_call": {
            const id = p.call_id || p.id;
            toolNames.set(id, "shell");
            push({ ts, kind: "tool_call", tool: { id, name: "shell", input: { command: p.action?.command, workdir: p.action?.working_directory } } });
            break;
          }
          case "web_search_call": {
            const id = p.id || p.call_id;
            toolNames.set(id, "web_search");
            push({ ts, kind: "tool_call", tool: { id, name: "web_search", input: p.action || {} } });
            break;
          }
          case "function_call_output":
          case "custom_tool_call_output": {
            const { output, isError } = parseOutput(p.output);
            push({ ts, kind: "tool_result", result: { id: p.call_id, name: toolNames.get(p.call_id), output, isError } });
            break;
          }
          case "compacted":
          case "compaction":
            push({ ts, kind: "system", subtype: "compact", text: "context compacted" });
            break;
          default:
            break;
        }
        break;
      }
      case "compacted":
        push({ ts, kind: "system", subtype: "compact", text: "context compacted" });
        break;
      case "event_msg": {
        switch (p.type) {
          case "token_count": {
            const total = p.info?.total_token_usage;
            if (total) session.usageTotal = mapUsage(total);
            if (p.info?.model_context_window) session.contextWindow = p.info.model_context_window;
            break;
          }
          case "task_complete": {
            // the same failure is usually also logged as an `error` event just before
            const last = session.events[session.events.length - 1];
            if (p.error?.message && !(last?.kind === "error" && last.text === p.error.message)) push({ ts, kind: "error", text: p.error.message });
            break;
          }
          case "error":
          case "stream_error":
            if (p.message) push({ ts, kind: "error", text: p.message });
            break;
          case "turn_aborted":
            push({ ts, kind: "system", subtype: "interrupt", text: `turn aborted (${p.reason || "unknown"})` });
            break;
          default:
            // user_message / agent_message / agent_reasoning duplicate response_items
            break;
        }
        break;
      }
      default:
        break;
    }
  }
  if (!session.id && file) {
    const m = /rollout-.*-([0-9a-f-]{36})\.jsonl$/.exec(file);
    session.id = m ? m[1] : path.basename(file, ".jsonl");
  }
  const firstUser = session.events.find((e) => e.kind === "user");
  if (firstUser) session.title = truncate(firstLine(firstUser.text), 80);
  return session;
}

export function parseFile(file) {
  return parseRecords(readJsonl(file), { path: file });
}

function sessionFiles() {
  const roots = [path.join(codexHome(), "sessions"), path.join(codexHome(), "archived_sessions")];
  const files = [];
  for (const r of roots) walk(r, (p) => /rollout-.*\.jsonl$/.test(p), files);
  return files;
}

export function listSessions() {
  const out = [];
  for (const file of sessionFiles()) {
    let st;
    try {
      st = fs.statSync(file);
    } catch {
      continue;
    }
    const head = readJsonlHead(file, 1024 * 1024);
    const meta = head.find((r) => r.type === "session_meta")?.payload;
    const firstUser = head.find(
      (r) => r.type === "response_item" && r.payload?.type === "message" && r.payload.role === "user" && !isInjected(textOf(r.payload.content)),
    );
    if (!meta && !firstUser) continue;
    const prompt = firstUser ? textOf(firstUser.payload.content) : "";
    if (!prompt) continue;
    const idFromName = /rollout-.*-([0-9a-f-]{36})\.jsonl$/.exec(file)?.[1];
    out.push({
      harness: name,
      id: meta?.id || meta?.session_id || idFromName,
      path: file,
      cwd: meta?.cwd,
      gitBranch: meta?.git?.branch,
      startedAt: meta?.timestamp || head[0]?.timestamp,
      updatedAt: st.mtime.toISOString(),
      title: truncate(firstLine(prompt), 80),
      sizeBytes: st.size,
    });
  }
  return out;
}

export function findLogById(id) {
  return sessionFiles().find((p) => p.includes(id)) || null;
}

/** Map one `codex exec --json` event into normalized events. */
export function execEventToEvents(rec, turn, state) {
  const ts = new Date().toISOString();
  const evs = [];
  if (rec.type === "thread.started") {
    state.threadId = rec.thread_id;
    return evs;
  }
  if (rec.type === "turn.completed") {
    if (rec.usage) state.usage = mapUsage(rec.usage);
    return evs;
  }
  if (rec.type === "turn.failed") {
    evs.push({ turn, ts, kind: "error", text: rec.error?.message || "turn failed" });
    return evs;
  }
  if (rec.type === "error") {
    evs.push({ turn, ts, kind: "error", text: rec.message || "error" });
    return evs;
  }
  if (rec.type !== "item.completed") return evs;
  const it = rec.item || {};
  const id = it.id || `item_${state.n++}`;
  switch (it.type) {
    case "agent_message":
      if (it.text?.trim()) evs.push({ turn, ts, kind: "assistant", text: it.text });
      break;
    case "reasoning":
      evs.push({ turn, ts, kind: "thinking", text: it.text || "" });
      break;
    case "command_execution": {
      evs.push({ turn, ts, kind: "tool_call", tool: { id, name: "shell", input: { command: it.command } } });
      const ec = it.exit_code;
      evs.push({ turn, ts, kind: "tool_result", result: { id, name: "shell", output: it.aggregated_output || "", isError: (typeof ec === "number" && ec !== 0) || it.status === "failed" } });
      break;
    }
    case "file_change": {
      evs.push({ turn, ts, kind: "tool_call", tool: { id, name: "apply_patch", input: { changes: it.changes || [] } } });
      evs.push({ turn, ts, kind: "tool_result", result: { id, name: "apply_patch", output: (it.changes || []).map((c) => `${c.kind} ${c.path}`).join("\n"), isError: it.status === "failed" } });
      break;
    }
    case "mcp_tool_call": {
      const nm = `${it.server}.${it.tool}`;
      evs.push({ turn, ts, kind: "tool_call", tool: { id, name: nm, input: it.arguments || {} } });
      const out = it.error ? JSON.stringify(it.error) : it.result ? JSON.stringify(it.result) : "";
      evs.push({ turn, ts, kind: "tool_result", result: { id, name: nm, output: out, isError: !!it.error || it.status === "failed" } });
      break;
    }
    case "web_search":
      evs.push({ turn, ts, kind: "tool_call", tool: { id, name: "web_search", input: { query: it.query } } });
      break;
    case "error":
      evs.push({ turn, ts, kind: "error", text: it.message || "error" });
      break;
    default:
      break;
  }
  return evs;
}

function sandboxArgs(mode) {
  if (!mode || mode === "auto" || mode === "bypass" || mode === "danger-full-access") return ["--dangerously-bypass-approvals-and-sandbox"];
  return ["-s", mode];
}

/**
 * Run one user turn through `codex exec --json`.
 * opts: {prompt, cwd, model, resume (thread id), sandbox, extraArgs, onEvent, turn, bin}
 */
export function runTurn(opts) {
  const bin = opts.bin || process.env.CASIMIR_CODEX_BIN || "codex";
  const args = ["exec", "--json", "--skip-git-repo-check", "--color", "never"];
  if (opts.cwd) args.push("-C", opts.cwd);
  if (opts.model) args.push("-m", opts.model);
  args.push(...sandboxArgs(opts.sandbox));
  if (opts.extraArgs?.length) args.push(...opts.extraArgs);
  if (opts.resume) args.push("resume", opts.resume, "-");
  else args.push("-");

  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { cwd: opts.cwd, env: cleanEnv(), stdio: ["pipe", "pipe", "pipe"] });
    const events = [];
    const raw = [];
    const state = { threadId: opts.resume || null, usage: null, n: 0 };
    let stderr = "";
    let buf = "";
    const turn = opts.turn || 1;
    const handle = (line) => {
      const t = line.trim();
      if (!t.startsWith("{")) return;
      let rec;
      try {
        rec = JSON.parse(t);
      } catch {
        return;
      }
      raw.push(rec);
      for (const ev of execEventToEvents(rec, turn, state)) {
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
      if (code !== 0 && !state.threadId) {
        return reject(new Error(`codex exited with code ${code}: ${truncate(stderr.trim(), 2000)}`));
      }
      const hadError = events.some((e) => e.kind === "error") || code !== 0;
      resolve({ sessionId: state.threadId, events, raw, usage: state.usage, isError: hadError, stderr, model: opts.model });
    });
    child.stdin.end(opts.prompt);
    opts.onChild?.(child);
  });
}

/** After a run, re-read the rollout the CLI wrote for full fidelity. */
export function loadRolloutForThread(threadId) {
  const file = findLogById(threadId);
  return file ? parseFile(file) : null;
}

export function detect(firstRecord) {
  return !!firstRecord && "payload" in firstRecord && ("type" in firstRecord);
}
