import fs from "node:fs";
import path from "node:path";
import * as claudeCode from "./claude-code.js";
import * as codex from "./codex.js";
import { readJsonlHead, readJson, exists } from "../util.js";
import { normalizeHarnessName } from "../model.js";

export const adapters = { "claude-code": claudeCode, codex };

export function adapterFor(harness) {
  const a = adapters[normalizeHarnessName(harness)];
  if (!a) throw new Error(`no adapter for harness ${harness}`);
  return a;
}

/** All known sessions across harnesses, newest first. */
export function listAllSessions({ harness } = {}) {
  const wanted = harness ? normalizeHarnessName(harness) : null;
  let out = [];
  for (const [nm, a] of Object.entries(adapters)) {
    if (wanted && nm !== wanted) continue;
    try {
      out = out.concat(a.listSessions());
    } catch (err) {
      if (process.env.CASIMIR_DEBUG) console.error(`list ${nm}: ${err.message}`);
    }
  }
  out.sort((x, y) => String(y.updatedAt || "").localeCompare(String(x.updatedAt || "")));
  return out;
}

/** Load a session from a path: raw harness log, casimir run dir, or normalized session.json. */
export function loadSessionFile(file) {
  const st = fs.statSync(file);
  if (st.isDirectory()) {
    const sj = path.join(file, "session.json");
    if (!exists(sj)) throw new Error(`${file} is not a casimir run directory (no session.json)`);
    return loadSessionFile(sj);
  }
  if (file.endsWith(".json")) {
    const data = readJson(file);
    if (data && Array.isArray(data.events) && data.harness) return data;
    throw new Error(`${file} is not a casimir session export`);
  }
  // Codex session_meta lines can exceed 100KB (they embed the base instructions), so read generously.
  const head = readJsonlHead(file, 1024 * 1024);
  if (head.some((r) => codex.detect(r))) return codex.parseFile(file);
  if (head.some((r) => claudeCode.detect(r))) return claudeCode.parseFile(file);
  // last resort: sniff the raw prefix
  const fd = fs.openSync(file, "r");
  const buf = Buffer.alloc(4096);
  const n = fs.readSync(fd, buf, 0, 4096, 0);
  fs.closeSync(fd);
  const prefix = buf.subarray(0, n).toString("utf8");
  if (/"type":\s*"session_meta"/.test(prefix) || /"payload":/.test(prefix)) return codex.parseFile(file);
  if (/"sessionId":/.test(prefix) || /"parentUuid":/.test(prefix)) return claudeCode.parseFile(file);
  throw new Error(`cannot determine session format of ${file}`);
}

/**
 * Resolve a user-supplied session reference:
 *   - a file or run directory path
 *   - "last", "claude:last", "codex:last"
 *   - a session id or unique id prefix (optionally "codex:<prefix>")
 */
export function resolveSession(ref) {
  if (!ref) throw new Error("session reference required (path, id, id prefix, or last)");
  if (exists(ref)) return loadSessionFile(path.resolve(ref));
  let harness = null;
  let key = ref;
  const m = /^(claude|claude-code|codex):(.+)$/.exec(ref);
  if (m) {
    harness = normalizeHarnessName(m[1]);
    key = m[2];
  }
  const all = listAllSessions({ harness });
  if (key === "last" || key === "latest") {
    if (!all.length) throw new Error(`no ${harness || ""} sessions found`);
    return loadSessionFile(all[0].path);
  }
  const hits = all.filter((s) => s.id === key || s.id?.startsWith(key) || path.basename(s.path).includes(key));
  if (hits.length === 1) return loadSessionFile(hits[0].path);
  if (hits.length > 1) {
    const exact = hits.filter((s) => s.id === key);
    if (exact.length === 1) return loadSessionFile(exact[0].path);
    throw new Error(`ambiguous session "${ref}": ${hits.map((h) => `${h.harness}:${h.id}`).join(", ")}`);
  }
  throw new Error(`session not found: ${ref}`);
}
