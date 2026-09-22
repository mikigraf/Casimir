/**
 * Normalized session model shared by all harness adapters.
 *
 * Session {
 *   id, harness: 'claude-code'|'codex', path?, title?, cwd?, gitBranch?, gitCommit?,
 *   model?, version?, startedAt?, endedAt?, permissionMode?, sandbox?,
 *   usageTotal?: Usage (cumulative, when the harness reports it that way),
 *   costUsd?: number,
 *   events: Event[]
 * }
 * Event {
 *   turn: number (1-based user turn), ts: ISO string,
 *   kind: 'user'|'assistant'|'thinking'|'tool_call'|'tool_result'|'system'|'error',
 *   text?, tool?: {id,name,input}, result?: {id,name?,output,isError},
 *   model?, usage?: Usage, sidechain?: boolean, subtype?: string, msgId?: string
 * }
 * Usage { input, output, cacheRead, cacheWrite, reasoning? }
 */

export const HARNESSES = ["claude-code", "codex"];

export function normalizeHarnessName(name) {
  if (!name) return null;
  const n = String(name).toLowerCase();
  if (["claude", "claude-code", "claudecode", "cc"].includes(n)) return "claude-code";
  if (["codex", "openai", "codex-cli"].includes(n)) return "codex";
  throw new Error(`unknown harness "${name}" (expected claude-code or codex)`);
}

export function emptyUsage() {
  return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0 };
}

export function addUsage(a, b) {
  if (!b) return a;
  a.input += b.input || 0;
  a.output += b.output || 0;
  a.cacheRead += b.cacheRead || 0;
  a.cacheWrite += b.cacheWrite || 0;
  a.reasoning += b.reasoning || 0;
  return a;
}

/** Real user prompts in order (excludes tool results, harness-injected messages, sidechains). */
export function userTurns(session) {
  return session.events
    .filter((e) => e.kind === "user" && !e.sidechain)
    .map((e) => ({ turn: e.turn, ts: e.ts, text: e.text }));
}

export function eventsForTurn(session, turn) {
  return session.events.filter((e) => e.turn === turn);
}

/** Last assistant text of a turn (or of the whole session). */
export function finalAssistantText(session, turn = null) {
  const evs = session.events.filter(
    (e) => e.kind === "assistant" && !e.sidechain && e.text?.trim() && (turn == null || e.turn === turn),
  );
  return evs.length ? evs[evs.length - 1].text : "";
}

/** Files written by the agent, inferred from tool calls. */
export function filesTouched(session) {
  const files = new Map(); // path -> Set(ops)
  const add = (p, op) => {
    if (!p) return;
    if (!files.has(p)) files.set(p, new Set());
    files.get(p).add(op);
  };
  for (const e of session.events) {
    if (e.kind !== "tool_call" || !e.tool) continue;
    const { name, input } = e.tool;
    const inp = input && typeof input === "object" ? input : {};
    switch (name) {
      case "Write":
        add(inp.file_path, "write");
        break;
      case "Edit":
      case "MultiEdit":
      case "NotebookEdit":
        add(inp.file_path || inp.notebook_path, "edit");
        break;
      case "apply_patch": {
        const patch = typeof inp.patch === "string" ? inp.patch : typeof input === "string" ? input : "";
        for (const m of patch.matchAll(/^\*\*\* (Add|Update|Delete) File: (.+)$/gm)) {
          add(m[2].trim(), m[1].toLowerCase());
        }
        for (const ch of inp.changes || []) add(ch.path, ch.kind || "edit");
        break;
      }
      case "file_change":
        for (const ch of inp.changes || []) add(ch.path, ch.kind || "edit");
        break;
      default: {
        // shell heredoc writes: cat > file <<EOF
        const cmd = shellCommand(e);
        if (cmd) {
          for (const m of cmd.matchAll(/(?:^|[;&|(]\s*)(?:cat|echo|printf)\b[^;&|\n]*?>>?\s*([^\s;&|<>]+)/g)) add(m[1], "shell-write");
          for (const m of cmd.matchAll(/\btee\s+(?:-a\s+)?([^\s;&|<>]+)/g)) add(m[1], "shell-write");
        }
      }
    }
  }
  return [...files.entries()].map(([path, ops]) => ({ path, ops: [...ops] }));
}

/** Command string for shell-like tool calls, else null. */
export function shellCommand(e) {
  if (e.kind !== "tool_call" || !e.tool) return null;
  const { name, input } = e.tool;
  const inp = input && typeof input === "object" ? input : {};
  if (name === "Bash") return inp.command ?? null;
  if (name === "shell" || name === "shell_command" || name === "local_shell" || name === "exec_command" || name === "container.exec" || name === "command_execution") {
    if (Array.isArray(inp.command)) return inp.command.join(" ");
    if (typeof inp.command === "string") return inp.command;
    if (typeof inp.cmd === "string") return inp.cmd;
  }
  return null;
}

/** One-line human summary of a tool call. */
export function toolOneLiner(e, max = 120) {
  const { name, input } = e.tool || {};
  const inp = input && typeof input === "object" ? input : {};
  const cmd = shellCommand(e);
  const one = (s) => String(s ?? "").replace(/\s+/g, " ").trim();
  let detail;
  if (cmd != null) detail = one(cmd);
  else if (inp.file_path) detail = inp.file_path;
  else if (inp.notebook_path) detail = inp.notebook_path;
  else if (inp.pattern) detail = `${inp.pattern}${inp.path ? " in " + inp.path : ""}`;
  else if (name === "apply_patch") {
    const patch = typeof inp.patch === "string" ? inp.patch : "";
    const files = [...patch.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)].map((m) => m[1].trim());
    detail = files.join(", ") || (inp.changes || []).map((c) => c.path).join(", ");
  } else if (inp.query) detail = one(inp.query);
  else if (inp.url) detail = inp.url;
  else if (inp.description) detail = one(inp.description);
  else if (inp.prompt) detail = one(inp.prompt);
  else if (typeof input === "string") detail = one(input);
  else detail = one(JSON.stringify(inp));
  const s = `${name}: ${detail}`;
  return s.length > max ? s.slice(0, max - 1) + "…" : s;
}

export function durationMs(session) {
  const ts = session.events.map((e) => Date.parse(e.ts)).filter((n) => !Number.isNaN(n));
  if (ts.length < 2) return 0;
  return Math.max(...ts) - Math.min(...ts);
}

/** Per-session aggregate statistics. */
export function stats(session) {
  const s = {
    harness: session.harness,
    model: session.model || null,
    turns: 0,
    assistantMessages: 0,
    thinkingBlocks: 0,
    toolCalls: 0,
    toolErrors: 0,
    toolsByName: {},
    filesTouched: filesTouched(session).length,
    durationMs: durationMs(session),
    usage: emptyUsage(),
    costUsd: session.costUsd ?? null,
    errors: 0,
    sidechainEvents: 0,
    finalMessageChars: finalAssistantText(session).length,
  };
  const seenMsg = new Set();
  const turns = new Set();
  for (const e of session.events) {
    if (e.sidechain) {
      s.sidechainEvents++;
      continue; // subagent traffic is reported separately
    }
    switch (e.kind) {
      case "user":
        turns.add(e.turn);
        break;
      case "assistant":
        s.assistantMessages++;
        break;
      case "thinking":
        s.thinkingBlocks++;
        break;
      case "tool_call": {
        s.toolCalls++;
        const n = e.tool?.name || "?";
        s.toolsByName[n] = (s.toolsByName[n] || 0) + 1;
        break;
      }
      case "tool_result":
        if (e.result?.isError) s.toolErrors++;
        break;
      case "error":
        s.errors++;
        break;
    }
    if (e.usage && !session.usageTotal) {
      const key = e.msgId || `${e.ts}-${e.kind}`;
      if (!seenMsg.has(key)) {
        seenMsg.add(key);
        addUsage(s.usage, e.usage);
      }
    }
  }
  if (session.usageTotal) s.usage = { ...emptyUsage(), ...session.usageTotal };
  s.turns = turns.size;
  return s;
}

/** Recompute turn numbers from user events (used after merging). */
export function renumberTurns(events) {
  let turn = 0;
  for (const e of events) {
    if (e.kind === "user" && !e.sidechain) turn++;
    e.turn = Math.max(turn, 1);
  }
  return events;
}
