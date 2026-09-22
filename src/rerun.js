/**
 * Rerun orchestration: replay a recorded session's user turns against a harness/model,
 * capture the new session, diff the workspace, and compare against the original.
 */
import path from "node:path";
import fs from "node:fs";
import { randomUUID } from "node:crypto";
import { adapterFor } from "./adapters/index.js";
import { userTurns, normalizeHarnessName, renumberTurns } from "./model.js";
import { simulateUserTurn } from "./simulate.js";
import { compareSessions, judgeSessions, renderCompareMarkdown } from "./compare.js";
import { isGitRepo, repoRoot, baseCommit, createWorktree, captureDiff } from "./workspace.js";
import { casimirHome, ensureDir, writeJson, nowStamp, slug, exists, c } from "./util.js";
import { formatEvent } from "./render.js";

/** Decide where the rerun will execute. Returns {dir, mode, commit?, how?, repo?}. */
export function planWorkspace(original, opts) {
  const ws = opts.workspace || "auto";
  const cwd = original.cwd;
  if (ws !== "auto" && ws !== "worktree" && ws !== "same") {
    const dir = path.resolve(ws);
    if (!exists(dir)) throw new Error(`workspace directory does not exist: ${dir}`);
    return { dir, mode: "dir" };
  }
  if (!cwd || !exists(cwd)) {
    if (ws === "worktree" || ws === "same") throw new Error(`original cwd is not available here (${cwd}); pass --workspace <dir>`);
    return { dir: process.cwd(), mode: "cwd-fallback", note: `original cwd ${cwd} not found; using current directory` };
  }
  const git = isGitRepo(cwd);
  if (ws === "same" || (ws === "auto" && !git)) {
    return { dir: cwd, mode: "same", note: git ? null : "original cwd is not a git repo; running in place (no diff capture)" };
  }
  const repo = repoRoot(cwd);
  const { commit, how } = baseCommit(original, cwd);
  const runId = opts.runId;
  const dest = path.join(casimirHome(), "worktrees", runId);
  const rel = path.relative(repo, cwd);
  return { dir: path.join(dest, rel), root: dest, mode: "worktree", commit, how, repo };
}

export function makeRunId(original, harness, model) {
  return `${nowStamp()}-${slug(harness)}-${slug(model || "default")}-${String(original.id || "").slice(0, 8)}`;
}

/**
 * opts: {harness, model, userMode, workspace, turns, permissionMode, sandbox, judge,
 *        llmModel, llmBackend, outDir, quiet, extraArgs, dryRun}
 */
export async function rerun(original, opts = {}, io = {}) {
  const log = io.log || ((s) => process.stderr.write(s + "\n"));
  const out = io.out || ((s) => process.stdout.write(s + "\n"));
  const harness = normalizeHarnessName(opts.harness || original.harness);
  const adapter = adapterFor(harness);
  const sameHarness = harness === original.harness;
  const model = opts.model || (sameHarness ? original.model : undefined);
  const runId = opts.runId || makeRunId(original, harness, model);
  const ws = planWorkspace(original, { ...opts, runId });
  const turns = userTurns(original);
  const maxTurns = opts.turns ? Math.min(opts.turns, turns.length) : turns.length;
  const runDir = opts.outDir ? path.resolve(opts.outDir) : path.join(casimirHome(), "runs", runId);

  if (!turns.length) throw new Error("original session has no user turns to replay");

  log(`${c.bold}rerun${c.reset} ${original.harness}:${original.id} → ${harness}${model ? ` (${model})` : " (harness default model)"}`);
  log(`  turns: ${maxTurns}/${turns.length}  user mode: ${opts.userMode || "verbatim"}`);
  log(`  workspace: ${ws.dir} [${ws.mode}${ws.commit ? `, ${ws.commit.slice(0, 10)} — ${ws.how}` : ""}]`);
  if (ws.note) log(`  ${c.yellow}note: ${ws.note}${c.reset}`);
  log(`  output: ${runDir}`);
  if (opts.dryRun) {
    for (const t of turns.slice(0, maxTurns)) log(`  turn ${t.turn}: ${t.text.split("\n")[0].slice(0, 100)}`);
    return { runDir, dryRun: true, workspace: ws };
  }

  if (ws.mode === "worktree") {
    createWorktree(ws.repo, ws.commit, ws.root);
    log(`  created worktree ${ws.root}`);
  }
  await ensureDir(runDir);
  writeJson(path.join(runDir, "original.json"), original);

  const session = {
    id: null,
    harness,
    model,
    cwd: ws.dir,
    title: original.title,
    startedAt: new Date().toISOString(),
    rerunOf: { harness: original.harness, id: original.id, path: original.path },
    workspace: ws,
    events: [],
  };
  const meta = { runId, original: { harness: original.harness, id: original.id, path: original.path }, harness, model, opts: { ...opts }, workspace: ws, startedAt: session.startedAt };
  writeJson(path.join(runDir, "meta.json"), meta);
  const rawPath = path.join(runDir, "raw.jsonl");
  const rawFd = fs.openSync(rawPath, "a");
  const start = Date.now();
  let child = null;
  const onSig = () => {
    log(`\n${c.yellow}interrupted; stopping harness${c.reset}`);
    child?.kill("SIGINT");
  };
  process.on("SIGINT", onSig);

  let harnessSessionId = null;
  let costUsd = 0;
  let sawCost = false;
  const permissionMode = opts.permissionMode || (ws.mode === "worktree" || ws.mode === "dir" ? "auto" : "acceptEdits");
  const sandbox = opts.sandbox || (ws.mode === "worktree" || ws.mode === "dir" ? "auto" : "workspace-write");
  const persist = () => {
    session.endedAt = new Date().toISOString();
    writeJson(path.join(runDir, "session.json"), session);
  };

  try {
    for (let i = 0; i < maxTurns; i++) {
      const t = turns[i];
      let message = t.text;
      let simulated = null;
      if (i > 0 && (opts.userMode === "simulate" || opts.userMode === "auto")) {
        log(`${c.magenta}simulating user for turn ${t.turn}…${c.reset}`);
        const sim = await simulateUserTurn({ original, rerun: session, turnIndex: t.turn, model: opts.llmModel, backend: opts.llmBackend });
        simulated = sim;
        if (sim.message == null) {
          log(`${c.magenta}simulator stopped the session: ${sim.reason}${c.reset}`);
          session.events.push({ turn: t.turn, ts: new Date().toISOString(), kind: "system", subtype: "simulator-stop", text: sim.reason });
          break;
        }
        message = sim.message;
        if (!sim.verbatim) log(`${c.magenta}adapted message: ${message.split("\n")[0].slice(0, 120)}${c.reset}`);
      }
      const userEv = { turn: t.turn, ts: new Date().toISOString(), kind: "user", text: message };
      if (simulated) userEv.simulated = { verbatim: simulated.verbatim, reason: simulated.reason };
      session.events.push(userEv);
      if (!opts.quiet) out(formatEvent(userEv, {}, { start }));

      const res = await adapter.runTurn({
        prompt: message,
        cwd: ws.dir,
        model,
        turn: t.turn,
        sessionId: harnessSessionId ? undefined : randomUUID(),
        resume: harnessSessionId,
        permissionMode,
        sandbox,
        extraArgs: opts.extraArgs,
        onChild: (ch) => (child = ch),
        onEvent: (ev) => {
          session.events.push(ev);
          if (!opts.quiet) {
            const s = formatEvent(ev, { thinking: opts.thinking, maxLines: 6 }, { start });
            if (s != null) out(s);
          }
        },
      });
      for (const r of res.raw || []) fs.writeSync(rawFd, JSON.stringify(r) + "\n");
      harnessSessionId = res.sessionId || harnessSessionId;
      if (res.model) session.model = res.model; // what the harness actually reported beats what we asked for
      if (res.costUsd != null) {
        costUsd += res.costUsd;
        sawCost = true;
      }
      if (res.usage) session.usageTotal = res.usage;
      persist();
      if (res.isError && !opts.continueOnError) {
        log(`${c.red}harness reported an error in turn ${t.turn}; stopping (use --continue-on-error to keep going)${c.reset}`);
        break;
      }
    }
  } finally {
    process.off("SIGINT", onSig);
    fs.closeSync(rawFd);
  }

  session.id = harnessSessionId || runId;
  session.harnessSessionId = harnessSessionId;
  if (sawCost) session.costUsd = costUsd;

  // Prefer the harness's own on-disk log for the rerun when we can find it (full fidelity).
  if (harnessSessionId && adapter.findLogById) {
    try {
      const file = adapter.findLogById(harnessSessionId);
      if (file) {
        const full = adapter.parseFile(file);
        if (full.events.some((e) => e.kind === "user")) {
          // keep our user events (they carry simulation metadata) but take everything else from the log
          const ours = session.events.filter((e) => e.kind === "user");
          const theirs = full.events.filter((e) => e.kind !== "user");
          const merged = [];
          let ui = 0;
          for (const e of full.events) {
            if (e.kind === "user") {
              merged.push(ours[ui] || e);
              ui++;
            } else merged.push(e);
          }
          // keep live-captured errors the on-disk log doesn't carry (e.g. a result-level failure)
          const known = new Set(theirs.filter((e) => e.kind === "error").map((e) => e.text));
          for (const e of session.events) if (e.kind === "error" && !known.has(e.text)) merged.push(e);
          session.events = renumberTurns(merged.length ? merged : [...ours, ...theirs]);
          session.model = session.model || full.model;
          if (full.usageTotal) session.usageTotal = full.usageTotal;
          session.harnessLogPath = file;
        }
      }
    } catch (err) {
      if (process.env.CASIMIR_DEBUG) log(`could not load harness log: ${err.message}`);
    }
  }
  renumberTurns(session.events);
  persist();

  const diff = captureDiff(ws.dir);
  fs.writeFileSync(path.join(runDir, "diff.patch"), diff.patch);
  writeJson(path.join(runDir, "diff.json"), { files: diff.files, stat: diff.stat });

  let judge = null;
  if (opts.judge) {
    log(`${c.magenta}asking judge…${c.reset}`);
    try {
      judge = await judgeSessions(original, session, { diffA: opts.originalDiff || null, diffB: diff, model: opts.llmModel, backend: opts.llmBackend });
    } catch (err) {
      log(`${c.red}judge failed: ${err.message}${c.reset}`);
    }
  }
  const report = compareSessions(original, session, { diffB: diff, judge });
  fs.writeFileSync(path.join(runDir, "report.md"), renderCompareMarkdown(report));
  writeJson(path.join(runDir, "report.json"), report);
  meta.endedAt = session.endedAt;
  meta.harnessSessionId = harnessSessionId;
  writeJson(path.join(runDir, "meta.json"), meta);
  return { runDir, session, report, diff, workspace: ws };
}
