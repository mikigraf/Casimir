import { parseArgs } from "node:util";
import fs from "node:fs";
import path from "node:path";
import { listAllSessions, resolveSession } from "./adapters/index.js";
import { renderTranscript, renderMarkdown, renderStats, renderSessionList } from "./render.js";
import { play } from "./play.js";
import { rerun } from "./rerun.js";
import { compareSessions, judgeSessions, renderCompareText, renderCompareMarkdown } from "./compare.js";
import { stats } from "./model.js";
import { readJson, exists, c } from "./util.js";

const HELP = `casimir — replay, rerun and compare coding-agent sessions (Claude Code, Codex)

usage: casimir <command> [options]

  list                        list recorded sessions from all harnesses
      --harness <h>           claude-code | codex
      --cwd <dir>             only sessions recorded in this directory
      --limit <n>             default 30
      --json

  show <session>              print a transcript
      --thinking              include reasoning blocks
      --full                  do not truncate tool output
      --sidechains            include subagent traffic
      --turn <n>              only one user turn
      --format text|md|json

  play <session>              replay with original pacing
      --speed <x>             default 5
      --max-delay <ms>        default 2000
      (+ show options)

  stats <session>             aggregate numbers (--json)
  export <session>            write normalized JSON or markdown
      -o <file>  --format json|md

  rerun <session>             replay the user's turns against a harness/model
      --harness <h>           target harness (default: same as original)
      --model <m>             target model (default: original model when same harness)
      --user verbatim|simulate  how later user turns are produced (default verbatim)
      --workspace auto|worktree|same|<dir>
                              auto = fresh git worktree at the session's base commit
      --turns <n>             replay only the first n user turns
      --permission-mode <m>   Claude Code permission mode (default: bypass in worktrees)
      --sandbox <m>           Codex sandbox (default: bypass in worktrees, else workspace-write)
      --judge                 ask an LLM to score original vs rerun
      --llm auto|api|claude-cli   backend for simulator/judge
      --llm-model <m>         model for simulator/judge (default claude-opus-5)
      --thinking              show reasoning while running
      -o <dir>                run directory (default ~/.casimir/runs/<id>)
      --continue-on-error     keep replaying turns after a harness error
      --dry-run               print the plan only
      -- <args>               extra args passed to the harness CLI

  compare <a> <b>             compare two sessions / run directories
      --judge  --llm  --llm-model  --format text|md|json

  runs                        list rerun directories under ~/.casimir/runs

<session> is a log path, a casimir run dir, "last", "claude:last", "codex:last",
a session id, or a unique id prefix (optionally "codex:<prefix>").
`;

const OPTIONS = {
  harness: { type: "string" },
  cwd: { type: "string" },
  limit: { type: "string" },
  json: { type: "boolean" },
  thinking: { type: "boolean" },
  full: { type: "boolean" },
  sidechains: { type: "boolean" },
  turn: { type: "string" },
  format: { type: "string" },
  speed: { type: "string" },
  "max-delay": { type: "string" },
  "max-lines": { type: "string" },
  output: { type: "string", short: "o" },
  model: { type: "string" },
  user: { type: "string" },
  workspace: { type: "string" },
  turns: { type: "string" },
  "permission-mode": { type: "string" },
  sandbox: { type: "string" },
  judge: { type: "boolean" },
  llm: { type: "string" },
  "llm-model": { type: "string" },
  "dry-run": { type: "boolean" },
  quiet: { type: "boolean", short: "q" },
  "continue-on-error": { type: "boolean" },
  help: { type: "boolean", short: "h" },
  version: { type: "boolean", short: "v" },
};

export async function main(argv) {
  let extra = [];
  const dd = argv.indexOf("--");
  if (dd >= 0) {
    extra = argv.slice(dd + 1);
    argv = argv.slice(0, dd);
  }
  let parsed;
  try {
    parsed = parseArgs({ args: argv, options: OPTIONS, allowPositionals: true, strict: true });
  } catch (err) {
    console.error(`casimir: ${err.message}\n`);
    console.error(HELP);
    return 2;
  }
  const { values: v, positionals } = parsed;
  const cmd = positionals[0];
  if (v.version) {
    const pkg = readJson(new URL("../package.json", import.meta.url));
    console.log(pkg.version);
    return 0;
  }
  if (!cmd || v.help || cmd === "help") {
    console.log(HELP);
    return 0;
  }
  const num = (s, d) => (s == null ? d : Number(s));
  const showOpts = { thinking: v.thinking, full: v.full, sidechains: v.sidechains, turn: num(v.turn, null), maxLines: num(v["max-lines"], undefined) };

  switch (cmd) {
    case "list": {
      let items = listAllSessions({ harness: v.harness });
      if (v.cwd) {
        const want = path.resolve(v.cwd);
        items = items.filter((s) => s.cwd && path.resolve(s.cwd) === want);
      }
      items = items.slice(0, num(v.limit, 30));
      console.log(v.json ? JSON.stringify(items, null, 2) : renderSessionList(items));
      return 0;
    }
    case "show": {
      const session = resolveSession(positionals[1]);
      const fmt = v.format || "text";
      if (fmt === "json") console.log(JSON.stringify(session, null, 2));
      else if (fmt === "md") console.log(renderMarkdown(session, showOpts));
      else console.log(renderTranscript(session, showOpts));
      return 0;
    }
    case "play": {
      const session = resolveSession(positionals[1]);
      await play(session, { ...showOpts, speed: num(v.speed, 5), maxDelayMs: num(v["max-delay"], 2000) });
      return 0;
    }
    case "stats": {
      const session = resolveSession(positionals[1]);
      console.log(v.json ? JSON.stringify(stats(session), null, 2) : renderStats(session));
      return 0;
    }
    case "export": {
      const session = resolveSession(positionals[1]);
      const fmt = v.format || (v.output?.endsWith(".md") ? "md" : "json");
      const body = fmt === "md" ? renderMarkdown(session, { ...showOpts, thinking: v.thinking ?? true }) : JSON.stringify(session, null, 2);
      if (v.output) {
        fs.writeFileSync(v.output, body);
        console.error(`wrote ${v.output}`);
      } else console.log(body);
      return 0;
    }
    case "rerun": {
      const original = resolveSession(positionals[1]);
      const res = await rerun(original, {
        harness: v.harness,
        model: v.model,
        userMode: v.user || "verbatim",
        workspace: v.workspace || "auto",
        turns: num(v.turns, null),
        permissionMode: v["permission-mode"],
        sandbox: v.sandbox,
        judge: v.judge,
        llmModel: v["llm-model"],
        llmBackend: v.llm || "auto",
        outDir: v.output,
        quiet: v.quiet,
        thinking: v.thinking,
        dryRun: v["dry-run"],
        continueOnError: v["continue-on-error"],
        extraArgs: extra,
      });
      if (res.dryRun) return 0;
      console.log("");
      console.log(renderCompareText(res.report, { labelA: "original", labelB: "rerun" }));
      console.log("");
      console.log(`${c.bold}run saved:${c.reset} ${res.runDir}`);
      if (res.workspace.mode === "worktree") console.log(`${c.dim}worktree kept at ${res.workspace.root} (remove with: git worktree remove --force ${res.workspace.root})${c.reset}`);
      console.log(`${c.dim}casimir show ${res.runDir}   |   casimir compare ${original.path || original.id} ${res.runDir}${c.reset}`);
      return 0;
    }
    case "compare": {
      if (!positionals[2]) throw new Error("compare needs two sessions");
      const a = resolveSession(positionals[1]);
      const b = resolveSession(positionals[2]);
      const diffA = loadDiff(positionals[1]);
      const diffB = loadDiff(positionals[2]);
      let judge = null;
      if (v.judge) judge = await judgeSessions(a, b, { diffA, diffB, model: v["llm-model"], backend: v.llm || "auto" });
      const report = compareSessions(a, b, { diffA, diffB, judge });
      const fmt = v.format || "text";
      if (fmt === "json") console.log(JSON.stringify(report, null, 2));
      else if (fmt === "md") console.log(renderCompareMarkdown(report, { labelA: "A", labelB: "B" }));
      else console.log(renderCompareText(report, { labelA: `A: ${a.harness}`, labelB: `B: ${b.harness}` }));
      return 0;
    }
    case "runs": {
      const root = path.join(process.env.CASIMIR_HOME || path.join(process.env.HOME || "", ".casimir"), "runs");
      if (!exists(root)) {
        console.log("(no runs yet)");
        return 0;
      }
      for (const d of fs.readdirSync(root).sort().reverse()) {
        const m = path.join(root, d, "meta.json");
        if (!exists(m)) continue;
        const meta = readJson(m);
        console.log(`${d}  ${meta.harness}${meta.model ? "/" + meta.model : ""}  ← ${meta.original.harness}:${meta.original.id}`);
      }
      return 0;
    }
    default:
      console.error(`unknown command: ${cmd}\n`);
      console.error(HELP);
      return 2;
  }
}

function loadDiff(ref) {
  if (!ref || !exists(ref)) return null;
  const dir = fs.statSync(ref).isDirectory() ? ref : path.dirname(ref);
  const dj = path.join(dir, "diff.json");
  const dp = path.join(dir, "diff.patch");
  if (!exists(dj)) return null;
  const d = readJson(dj);
  if (exists(dp)) d.patch = fs.readFileSync(dp, "utf8");
  return d;
}
