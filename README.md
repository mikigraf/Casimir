# casimir

Replay, rerun, and compare coding-agent sessions recorded by **Claude Code**, **OpenAI Codex**,
**GitHub Copilot CLI**, and **Gemini CLI**. Written in Rust; a single binary with no runtime
dependencies beyond `git` (and `curl` for the optional Anthropic API backend).

Both harnesses already write a complete transcript of every session to disk. `casimir` reads those
logs, normalizes them into one event model, and lets you:

- **list / show / play** any past session as a readable transcript, with the original pacing;
- **rerun** a session's user turns against a different model or a different harness, in a fresh git
  worktree checked out at the commit the original session started from;
- **simulate the user** for follow-up turns when the rerun diverges from the original, so the
  replay keeps pursuing the same goals instead of replying to things that never happened;
- **compare** two sessions (or a session and its rerun): tool usage, files touched, tokens, cost,
  duration, workspace diff, end-state similarity, final answer, and optionally an LLM judge that is
  run in both candidate orders so position bias is caught instead of reported as a verdict;
- **replicate** reruns (and vary the simulator model) to get pass@1, pass^k and score spreads instead
  of a single, unrepeatable result.

## Install

```
cargo install --path .        # puts `casimir` on your PATH
# or
cargo build --release         # binary at target/release/casimir
```

Requires a Rust toolchain (1.80+) and a C linker.

## Where the logs come from

| harness | location | format |
|---|---|---|
| Claude Code | `~/.claude/projects/<cwd-slug>/<session-id>.jsonl` (`$CLAUDE_CONFIG_DIR`) | one record per line; `user`/`assistant` records carry the API-shaped message (text, thinking, tool_use, tool_result) plus cwd, branch, version, permission mode |
| Codex | `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` (`$CODEX_HOME`) | `session_meta` (cwd, git commit, CLI version), `turn_context` (model, sandbox), `response_item` (messages, reasoning, function calls, apply_patch), `event_msg` (token counts, lifecycle) |
| Copilot CLI | `~/.copilot/session-state/<session-id>/` (`$COPILOT_HOME`) | `workspace.yaml` (id, cwd, branch, timestamps, summary) + `events.jsonl` (`user.message`, `assistant.message`, `tool.execution_start/complete`, `session.shutdown` with per-model token metrics). GitHub documents this schema as internal, so treat the adapter as experimental |
| Gemini CLI | `~/.gemini/tmp/<project>/chats/session-<ts>-<id>.jsonl` (`$GEMINI_CLI_HOME`) | metadata line, then message records (`user`/`gemini` with `toolCalls`, `thoughts`, `tokens`, `model`), `$set` upserts and `$rewindTo`; cwd from the project's `.project_root` (or `projects.json` for hashed directories); legacy single-object `.json` also read |

Harness-injected context (system reminders, `<environment_context>`, `<session_context>`, AGENTS.md
instructions, slash command echoes, subagent side-chains) is recognized and kept out of the user
turns, so a rerun sends only what the human actually typed and lets the target harness inject its own
context.

## Commands

```
casimir list [--harness claude-code|codex] [--cwd DIR] [--json]
casimir show <session> [--thinking] [--full] [--sidechains] [--turn N] [--format text|md|json]
casimir play <session> [--speed 5] [--max-delay 2000]
casimir stats <session>
casimir export <session> -o out.md|out.json

casimir rerun <session> [--harness H] [--model M] [--user verbatim|simulate]
                        [--workspace auto|worktree|same|DIR] [--turns N]
                        [--replicates N] [--sim-model M ...] [--sim-llm B]
                        [--judge] [--judge-model M] [--judge-llm B] [--pass-threshold 7]
                        [--llm auto|api|claude-cli|cmd] [--llm-model M]
                        [--original-diff RUN_DIR] [-o DIR] [--dry-run]
                        [-- extra args for the harness CLI]
casimir compare <a> <b> [--judge] [--judge-model M] [--format text|md|json]
casimir runs
```

`<session>` can be a log path, a rerun directory, a Copilot session directory, `last`,
`claude:last`, `codex:last`, `copilot:last`, `gemini:last`, a session id, or a unique id prefix.

## How a rerun works

1. The original session is parsed and its user turns extracted.
2. A workspace is chosen. By default, if the original cwd is a git repo, a detached worktree is
   created under `~/.casimir/worktrees/<run>` at the **base commit**: the commit recorded by Codex,
   or for Claude Code the last commit on the recorded branch before the session started. Your
   checkout is never touched. `--workspace same` runs in place; `--workspace DIR` uses any directory.
3. Turn 1 is sent verbatim to the target harness (`claude -p --output-format stream-json`,
   `codex exec --json`, `copilot -p --output-format json`, or `gemini -p --output-format stream-json`),
   then later turns are sent either verbatim or through the user simulator. Sessions are resumed
   between turns, so the target harness keeps its own context exactly as it would interactively.
4. Events stream to the terminal as they happen. When the harness finishes, casimir re-reads the
   harness's own on-disk log for the new session so the rerun has the same fidelity as the original.
5. The workspace diff is captured (including untracked files), and a comparison report is written:

```
~/.casimir/runs/<timestamp>-<harness>-<model>-<orig-id>/
  original.json   normalized original session
  session.json    normalized rerun session
  raw.jsonl       raw harness output
  diff.patch      workspace changes made by the rerun
  original.patch  the original session's changes, when known (see below)
  report.md       comparison table (+ judge verdict if requested)
```

With `--replicates N` (or several `--sim-model` values) the directory instead holds one
subdirectory per replicate plus `replicates.json` and a summary `report.md`.

Permissions: in an isolated worktree or explicit directory the harness runs with permission prompts
bypassed (`--dangerously-skip-permissions` / `--dangerously-bypass-approvals-and-sandbox`), because
a non-interactive rerun cannot answer prompts. In-place reruns default to `acceptEdits` /
`workspace-write`. Override with `--permission-mode` / `--sandbox`.

### Scoring outcomes, not trajectories

A rerun that reaches the same end state by a different route is a success, so the comparison
reports **end-state similarity** between the two workspace diffs: Jaccard over changed files times
the mean per-file Jaccard over added and removed lines. Tool-sequence similarity is shown too, but
only as a descriptive number. The original session's diff comes from a run directory
(`--original-diff`), or is reconstructed from git: the commits between the base commit and the last
commit before the session ended, or, failing that, the working tree against the base commit
(labelled as a heuristic in the report).

### Judging without position bias

LLM judges flip their verdict when the two candidates are swapped, most often when the candidates
are close in quality. `--judge` therefore always asks twice, with A and B in both orders, averages
the scores, and reports a **tie flagged as order-sensitive** when the two verdicts disagree. A
close-call warning appears when the averaged scores are within one point. Both passes are recorded
in `report.json`.

### Replicates

Agent runs are noisy, so one rerun is not a result. `--replicates N` runs the same replay N times
(each in its own worktree and run directory) and reports pass@1 (fraction that passed), pass^k
(every replicate passed), the judge-score range, end-state similarity, and token and tool-call
spreads. A replicate passes when it finishes every turn without a harness error and, if judged,
scores at least `--pass-threshold` (default 7 of 10). Pass one or more `--sim-model` values to run
the matrix once per simulator model; the summary groups results by simulator so simulator-induced
variance is visible instead of hidden.

### The user simulator (`--user simulate`)

Later user messages in a real session reacted to what the agent did: "no, use the other function",
"yes, go ahead", "the test you added fails". When a rerun diverges, sending those verbatim makes no
sense. With `--user simulate`, an LLM sees every message the real user sent, what the original agent
had done before each one, and what the new agent has done so far, and produces the message this user
would send now: verbatim when it still applies, adapted when it doesn't, or a stop with a typed
reason (`goals_met`, `cannot_adapt`, `out_of_scope`). The simulator carries notes across turns,
discloses information progressively, may not introduce anything absent from the recorded session,
and must cite the original turns an adapted message is grounded in; ungrounded replies are retried
up to three times, then the original message is sent verbatim. Simulated turns are marked in the
transcript and recorded with `simulated: {verbatim, reason, grounded_in}`; the simulator model and
backend are stored on the session and shown in every comparison, because the choice of simulator
alone measurably shifts agent outcomes.

The simulator (`--sim-model`, `--sim-llm`) and the judge (`--judge-model`, `--judge-llm`) are
configured independently and default to `--llm-model` / `--llm`. They use `claude-opus-5` through
the Anthropic Messages API (via `curl`) when credentials are available (`ANTHROPIC_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, or an `ant auth login` profile), with Anthropic's server-side refusal
fallback enabled. With `claude-cli` they run through `claude -p` with tools disabled, reusing your
Claude Code login. With `cmd` they run `$CASIMIR_LLM_CMD` (prompt on stdin, system prompt in
`$CASIMIR_LLM_SYSTEM`), which is how the tests drive them and how you can plug in any gateway.

## Examples

```
# what happened in my last Claude Code session?
casimir show claude:last --thinking

# watch it back at 10x
casimir play claude:last --speed 10

# same prompts, cheaper model, fresh worktree, then a side-by-side table
casimir rerun claude:last --model sonnet

# same task through Codex instead, with a simulated user and an order-swapped LLM judge
casimir rerun claude:last --harness codex --model gpt-5-codex --user simulate --judge

# three replicates, two simulator models: pass@1, pass^k, score spread per simulator
casimir rerun claude:last --model sonnet --user simulate --judge --replicates 3 \
    --sim-model claude-opus-5 --sim-model claude-sonnet-5

# replay a Gemini CLI session through Copilot CLI
casimir rerun gemini:last --harness copilot

# compare any two sessions or runs
casimir compare codex:last ~/.casimir/runs/2026-09-22_11-40-03-claude-code-sonnet-58fd0bfc --format md
```

## Development

```
cargo test          # parsers, renderers, comparison, and reruns driven by fake harness scripts
cargo build --release
```

Environment knobs: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `COPILOT_HOME`, `GEMINI_CLI_HOME`,
`CASIMIR_HOME` (runs and worktrees), `CASIMIR_CLAUDE_BIN`, `CASIMIR_CODEX_BIN`,
`CASIMIR_COPILOT_BIN`, `CASIMIR_GEMINI_BIN` (alternate harness executables), `CASIMIR_LLM_CMD`,
`CASIMIR_DEBUG`.

## Limitations

- A rerun replays the user's *inputs*, not the environment: network state, installed tools, and
  anything outside the git worktree may differ from the original run.
- Claude Code logs do not record a commit hash; the base commit is inferred from the branch and the
  session start time. Codex records it directly.
- Claude Code subagent transcripts (side-chains) are shown with `--sidechains` but not replayed
  separately; the target harness spawns its own.
- Live reruns need the target harness to be logged in (`claude /login`, `codex login`,
  `copilot login`, a Gemini API key or Google login). Gemini reruns set
  `GEMINI_CLI_TRUST_WORKSPACE=true` so headless mode runs in the worktree.
- Copilot CLI and Gemini CLI store formats are undocumented or internal and may change; their
  adapters were validated against real files written by the current CLI versions and against
  fixtures, not against long-running sessions.
- Replicates and order-swapped judging multiply API cost: N replicates × 2 judge calls each.
