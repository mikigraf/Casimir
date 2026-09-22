import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import * as claude from "../src/adapters/claude-code.js";
import * as codex from "../src/adapters/codex.js";
import { loadSessionFile, resolveSession } from "../src/adapters/index.js";
import { stats, userTurns, filesTouched, finalAssistantText, toolOneLiner } from "../src/model.js";
import { renderMarkdown, renderTranscript } from "../src/render.js";
import { compareSessions, renderCompareMarkdown, relativize } from "../src/compare.js";
import { extractJson } from "../src/util.js";
import { rerun } from "../src/rerun.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const FX = path.join(here, "fixtures");
const CLAUDE = path.join(FX, "claude-code.jsonl");
const CODEX = path.join(FX, "codex.jsonl");

test("claude-code: parses log into normalized session", () => {
  const s = claude.parseFile(CLAUDE);
  assert.equal(s.harness, "claude-code");
  assert.equal(s.id, "11111111-2222-4333-8444-555555555555");
  assert.equal(s.title, "Add greet function");
  assert.equal(s.model, "claude-opus-5");
  assert.equal(s.cwd, "/work/demo");
  assert.equal(s.gitBranch, "main");
  assert.equal(s.permissionMode, "bypassPermissions");
  const turns = userTurns(s);
  assert.equal(turns.length, 2);
  assert.equal(turns[0].text, "Add a greet(name) function to lib.py and a test for it", "system-reminder stripped");
  assert.equal(turns[1].turn, 2);
  const kinds = s.events.filter((e) => !e.sidechain).map((e) => e.kind);
  assert.deepEqual(kinds.slice(0, 5), ["user", "thinking", "assistant", "tool_call", "tool_result"]);
  const sys = s.events.filter((e) => e.kind === "system").map((e) => e.subtype);
  assert.deepEqual(sys, ["command", "command-output"]);
  assert.equal(s.events.filter((e) => e.sidechain).length, 2);
  const res = s.events.find((e) => e.kind === "tool_result" && e.result.isError);
  assert.equal(res.result.name, "Bash");
});

test("claude-code: stats dedupe usage per message and exclude subagents", () => {
  const st = stats(claude.parseFile(CLAUDE));
  assert.equal(st.turns, 2);
  assert.equal(st.assistantMessages, 3);
  assert.equal(st.thinkingBlocks, 1);
  assert.equal(st.toolCalls, 5);
  assert.equal(st.toolErrors, 1);
  assert.equal(st.filesTouched, 2);
  assert.equal(st.sidechainEvents, 2);
  assert.deepEqual(st.toolsByName, { Bash: 2, Edit: 2, Write: 1 });
  assert.equal(st.usage.output, 120 + 6 * 300);
  assert.equal(st.usage.input, 5 + 6 * 3);
  assert.equal(st.usage.cacheRead, 2000 + 6 * 3000);
  assert.equal(st.usage.cacheWrite, 1000 + 6 * 200);
  assert.equal(st.usage.reasoning, 40);
  assert.equal(st.durationMs, Date.parse("2026-09-20T10:02:08.000Z") - Date.parse("2026-09-20T10:00:01.000Z"));
});

test("codex: parses rollout, skips injected context, maps tools and usage", () => {
  const s = codex.parseFile(CODEX);
  assert.equal(s.harness, "codex");
  assert.equal(s.id, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
  assert.equal(s.model, "gpt-5-codex");
  assert.equal(s.gitCommit, "0123456789abcdef0123456789abcdef01234567");
  assert.equal(s.gitBranch, "main");
  assert.equal(s.sandbox, "workspace-write");
  assert.equal(s.effort, "medium");
  const turns = userTurns(s);
  assert.equal(turns.length, 2);
  assert.equal(turns[0].text, "Add a greet(name) function to lib.py and a test for it");
  const calls = s.events.filter((e) => e.kind === "tool_call");
  assert.deepEqual(calls.map((e) => e.tool.name), ["shell", "apply_patch", "shell", "apply_patch"]);
  assert.deepEqual(calls[0].tool.input.command, ["bash", "-lc", "cat lib.py"]);
  const results = s.events.filter((e) => e.kind === "tool_result");
  assert.equal(results[0].result.output, "def add(a, b):\n    return a + b\n", "JSON-wrapped output unwrapped");
  assert.equal(results[0].result.isError, false);
  assert.equal(results[2].result.isError, true, "Exit code: 127 detected");
  assert.equal(results[2].result.name, "shell");
  const st = stats(s);
  assert.equal(st.assistantMessages, 2, "event_msg duplicates ignored");
  assert.equal(st.thinkingBlocks, 1);
  assert.deepEqual(st.usage, { input: 15000, output: 800, cacheRead: 9000, cacheWrite: 0, reasoning: 250 });
  assert.deepEqual(filesTouched(s).map((f) => [f.path, f.ops]), [["lib.py", ["update"]], ["test_lib.py", ["add"]]]);
  assert.equal(finalAssistantText(s), 'Done: greet now defaults to "World".');
  assert.equal(finalAssistantText(s, 1), "Added `greet` to lib.py and a test in test_lib.py. pytest isn't installed so the test was not run.");
});

test("format detection and session resolution", () => {
  assert.equal(loadSessionFile(CLAUDE).harness, "claude-code");
  assert.equal(loadSessionFile(CODEX).harness, "codex");
  assert.equal(resolveSession(CODEX).id, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-"));
  const exported = path.join(tmp, "s.json");
  fs.writeFileSync(exported, JSON.stringify(codex.parseFile(CODEX)));
  assert.equal(loadSessionFile(exported).events.length, codex.parseFile(CODEX).events.length);
  assert.throws(() => resolveSession("definitely-not-a-session-xyz"), /not found/);
});

test("tool one-liners and file inference from shell heredocs", () => {
  const ev = { kind: "tool_call", tool: { id: "x", name: "Bash", input: { command: "cat > a/b.txt <<'EOF'\nhi\nEOF\n; tee out.log" } } };
  assert.equal(toolOneLiner(ev, 40), "Bash: cat > a/b.txt <<'EOF' hi EOF ; te…");
  const files = filesTouched({ events: [ev] }).map((f) => f.path);
  assert.deepEqual(files, ["a/b.txt", "out.log"]);
  const patch = { kind: "tool_call", tool: { id: "y", name: "apply_patch", input: { patch: "*** Begin Patch\n*** Delete File: gone.py\n*** End Patch" } } };
  assert.equal(toolOneLiner(patch), "apply_patch: gone.py");
});

test("renderers produce transcript and markdown", () => {
  const s = claude.parseFile(CLAUDE);
  const text = renderTranscript(s, { thinking: true });
  assert.match(text, /user \(turn 2\)/);
  assert.match(text, /I should look at lib.py first/);
  assert.doesNotMatch(text, /Explore the repo/, "sidechains hidden by default");
  assert.match(renderTranscript(s, { sidechains: true }), /\[subagent\]/);
  const md = renderMarkdown(codex.parseFile(CODEX));
  assert.match(md, /## Turn 2 — user/);
  assert.match(md, /apply_patch: lib.py, test_lib.py/);
});

test("compare relativizes paths across harnesses", () => {
  const a = claude.parseFile(CLAUDE);
  const b = codex.parseFile(CODEX);
  const r = compareSessions(a, b);
  assert.deepEqual(r.files.both, ["lib.py", "test_lib.py"]);
  assert.deepEqual(r.files.onlyA, []);
  assert.equal(relativize("/work/demo/x/y.py", "/work/demo"), "x/y.py");
  assert.equal(relativize("/elsewhere/y.py", "/work/demo"), "/elsewhere/y.py");
  const md = renderCompareMarkdown(r);
  assert.match(md, /\| tool calls \| 5 \| 4 \|/);
});

test("extractJson tolerates prose and fences", () => {
  assert.deepEqual(extractJson('Sure:\n```json\n{"a": 1, "b": "x}y"}\n```\nthanks'), { a: 1, b: "x}y" });
  assert.deepEqual(extractJson('prefix {"action":"stop","message":""} suffix'), { action: "stop", message: "" });
  assert.equal(extractJson("no json here"), null);
});

test("claude-code stream-json mapping (live run shape)", () => {
  const rec = { type: "assistant", timestamp: "2026-01-01T00:00:00Z", message: { id: "m", model: "claude-sonnet-5", content: [{ type: "text", text: "hi" }, { type: "tool_use", id: "t", name: "Bash", input: { command: "ls" } }], usage: { input_tokens: 1, output_tokens: 2 } } };
  const evs = claude.recordToEvents(rec, 3);
  assert.equal(evs.length, 2);
  assert.equal(evs[0].turn, 3);
  assert.equal(evs[0].usage.output, 2);
  assert.equal(evs[1].usage, undefined, "usage attached once per message");
  assert.equal(evs[1].tool.name, "Bash");
});

test("codex exec --json event mapping", () => {
  const state = { n: 0 };
  assert.deepEqual(codex.execEventToEvents({ type: "thread.started", thread_id: "T" }, 1, state), []);
  assert.equal(state.threadId, "T");
  const evs = codex.execEventToEvents({ type: "item.completed", item: { id: "i", type: "command_execution", command: "ls", aggregated_output: "a\n", exit_code: 2 } }, 1, state);
  assert.equal(evs[0].kind, "tool_call");
  assert.equal(evs[1].result.isError, true);
  codex.execEventToEvents({ type: "turn.completed", usage: { input_tokens: 3, cached_input_tokens: 1, output_tokens: 2 } }, 1, state);
  assert.deepEqual(state.usage, { input: 3, output: 2, cacheRead: 1, cacheWrite: 0, reasoning: 0 });
});

function tmpRepo() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-repo-"));
  execFileSync("git", ["init", "-q", dir]);
  fs.writeFileSync(path.join(dir, "README.md"), "seed\n");
  execFileSync("git", ["-C", dir, "-c", "user.email=t@t", "-c", "user.name=t", "add", "."]);
  execFileSync("git", ["-C", dir, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "seed"]);
  return dir;
}

test("rerun drives a fake claude harness end to end", async () => {
  const original = claude.parseFile(CLAUDE);
  const repo = tmpRepo();
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-run-"));
  process.env.CASIMIR_CLAUDE_BIN = path.join(FX, "fake-claude.sh");
  process.env.CASIMIR_HOME = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-home-"));
  const logs = [];
  const res = await rerun(original, { workspace: repo, outDir, quiet: true }, { log: (s) => logs.push(s), out: () => {} });
  assert.equal(res.session.harness, "claude-code");
  assert.equal(res.session.model, "fake-model");
  assert.equal(userTurns(res.session).length, 2);
  assert.equal(res.session.events.filter((e) => e.kind === "tool_call").length, 2);
  assert.equal(res.session.costUsd, 0.02);
  assert.ok(res.diff.files.some((f) => f.path === "out.txt"), "workspace diff captured");
  assert.match(res.diff.patch, /\+hi/);
  for (const f of ["session.json", "original.json", "meta.json", "raw.jsonl", "diff.patch", "report.md", "report.json"]) {
    assert.ok(fs.existsSync(path.join(outDir, f)), `${f} written`);
  }
  const reloaded = loadSessionFile(outDir);
  assert.equal(stats(reloaded).toolCalls, 2);
  assert.equal(res.report.b.turns, 2);
  assert.match(res.session.events.find((e) => e.kind === "assistant").text, /Working on: Add a greet/);
});

test("rerun drives a fake codex harness (cross-harness) end to end", async () => {
  const original = claude.parseFile(CLAUDE);
  const repo = tmpRepo();
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-run-"));
  process.env.CASIMIR_CODEX_BIN = path.join(FX, "fake-codex.sh");
  process.env.CODEX_HOME = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-codexhome-"));
  const res = await rerun(original, { harness: "codex", model: "gpt-5-codex", workspace: repo, outDir, turns: 1, quiet: true }, { log: () => {}, out: () => {} });
  assert.equal(res.session.harness, "codex");
  assert.equal(userTurns(res.session).length, 1);
  const names = res.session.events.filter((e) => e.kind === "tool_call").map((e) => e.tool.name);
  assert.deepEqual(names, ["shell", "apply_patch"]);
  assert.deepEqual(res.session.usageTotal, { input: 100, output: 30, cacheRead: 40, cacheWrite: 0, reasoning: 0 });
  assert.deepEqual(res.report.files.both, []);
  assert.ok(res.report.files.onlyB.includes("out.txt"));
});

test("rerun creates a worktree at the base commit when cwd is a git repo", async () => {
  const repo = tmpRepo();
  const original = claude.parseFile(CLAUDE);
  original.cwd = repo;
  original.gitBranch = null;
  original.startedAt = new Date(Date.now() + 60_000).toISOString(); // after the seed commit
  process.env.CASIMIR_CLAUDE_BIN = path.join(FX, "fake-claude.sh");
  process.env.CASIMIR_HOME = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-home-"));
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), "casimir-run-"));
  const res = await rerun(original, { outDir, turns: 1, quiet: true }, { log: () => {}, out: () => {} });
  assert.equal(res.workspace.mode, "worktree");
  assert.ok(res.workspace.root.startsWith(process.env.CASIMIR_HOME));
  assert.ok(fs.existsSync(path.join(res.workspace.dir, "out.txt")), "fake harness wrote into the worktree");
  assert.ok(!fs.existsSync(path.join(repo, "out.txt")), "original checkout untouched");
});
