# casimir

Replay, rerun, and compare coding-agent sessions recorded by **Claude Code** and **OpenAI Codex**.
Written in Rust; a single static-ish binary with no runtime dependencies beyond `git` (and `curl`
for the optional Anthropic API backend).

Both harnesses already write a complete transcript of every session to disk. `casimir` reads those
logs, normalizes them into one event model, and lets you:

- **list / show / play** any past session as a readable transcript, with the original pacing;
- **rerun** a session's user turns against a different model or a different harness, in a fresh git
  worktree checked out at the commit the original session started from;
- **simulate the user** for follow-up turns when the rerun diverges from the original, so the
  replay keeps pursuing the same goals instead of replying to things that never happened;
- **compare** two sessions (or a session and its rerun): tool usage, files touched, tokens, cost,
  duration, workspace diff, final answer, and optionally an LLM judge.

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

Harness-injected context (system reminders, `<environment_context>`, AGENTS.md instructions, slash
command echoes, subagent side-chains) is recognized and kept out of the user turns, so a rerun sends
only what the human actually typed and lets the target harness inject its own context.

## Commands

```
casimir list [--harness claude-code|codex] [--cwd DIR] [--json]
casimir show <session> [--thinking] [--full] [--sidechains] [--turn N] [--format text|md|json]
casimir play <session> [--speed 5] [--max-delay 2000]
casimir stats <session>
casimir export <session> -o out.md|out.json

casimir rerun <session> [--harness H] [--model M] [--user verbatim|simulate]
                        [--workspace auto|worktree|same|DIR] [--turns N] [--judge]
                        [--llm auto|api|claude-cli] [--llm-model M] [-o DIR] [--dry-run]
                        [-- extra args for the harness CLI]
casimir compare <a> <b> [--judge] [--format text|md|json]
casimir runs
```

`<session>` can be a log path, a rerun directory, `last`, `claude:last`, `codex:last`, a session id,
or a unique id prefix.

## How a rerun works

1. The original session is parsed and its user turns extracted.
2. A workspace is chosen. By default, if the original cwd is a git repo, a detached worktree is
   created under `~/.casimir/worktrees/<run>` at the **base commit**: the commit recorded by Codex,
   or for Claude Code the last commit on the recorded branch before the session started. Your
   checkout is never touched. `--workspace same` runs in place; `--workspace DIR` uses any directory.
3. Turn 1 is sent verbatim to the target harness (`claude -p --output-format stream-json` or
   `codex exec --json`), then later turns are sent either verbatim or through the user simulator.
   Sessions are resumed between turns (`--resume` / `codex exec resume`), so the target harness keeps
   its own context exactly as it would in an interactive session.
4. Events stream to the terminal as they happen. When the harness finishes, casimir re-reads the
   harness's own on-disk log for the new session so the rerun has the same fidelity as the original.
5. The workspace diff is captured (including untracked files), and a comparison report is written:

```
~/.casimir/runs/<timestamp>-<harness>-<model>-<orig-id>/
  original.json   normalized original session
  session.json    normalized rerun session
  raw.jsonl       raw harness output
  diff.patch      workspace changes made by the rerun
  report.md       comparison table (+ judge verdict if requested)
```

Permissions: in an isolated worktree or explicit directory the harness runs with permission prompts
bypassed (`--dangerously-skip-permissions` / `--dangerously-bypass-approvals-and-sandbox`), because
a non-interactive rerun cannot answer prompts. In-place reruns default to `acceptEdits` /
`workspace-write`. Override with `--permission-mode` / `--sandbox`.

### The user simulator (`--user simulate`)

Later user messages in a real session reacted to what the agent did: "no, use the other function",
"yes, go ahead", "the test you added fails". When a rerun diverges, sending those verbatim makes no
sense. With `--user simulate`, an LLM sees every message the real user sent, what the original agent
had done before each one, and what the new agent has done so far, and produces the message this user
would send now: verbatim when it still applies, adapted when it doesn't, or a stop when the goals are
already met. Simulated turns are marked in the recorded session (`simulated: {verbatim, reason}`).

The simulator and judge use `claude-opus-5` through the Anthropic Messages API (via `curl`) when
credentials are available (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, or an `ant auth login`
profile), with Anthropic's server-side refusal fallback enabled. With `--llm claude-cli` they run
through `claude -p` with tools disabled, reusing your Claude Code login instead.

## Examples

```
# what happened in my last Claude Code session?
casimir show claude:last --thinking

# watch it back at 10x
casimir play claude:last --speed 10

# same prompts, cheaper model, fresh worktree, then a side-by-side table
casimir rerun claude:last --model sonnet

# same task through Codex instead, with a simulated user and an LLM judge
casimir rerun claude:last --harness codex --model gpt-5-codex --user simulate --judge

# compare any two sessions or runs
casimir compare codex:last ~/.casimir/runs/2026-09-22_11-40-03-claude-code-sonnet-58fd0bfc --format md
```

## Development

```
cargo test          # parsers, renderers, comparison, and reruns driven by fake harness scripts
cargo build --release
```

Environment knobs: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `CASIMIR_HOME` (runs and worktrees),
`CASIMIR_CLAUDE_BIN`, `CASIMIR_CODEX_BIN` (alternate harness executables), `CASIMIR_DEBUG`.

## Limitations

- A rerun replays the user's *inputs*, not the environment: network state, installed tools, and
  anything outside the git worktree may differ from the original run.
- Claude Code logs do not record a commit hash; the base commit is inferred from the branch and the
  session start time. Codex records it directly.
- Claude Code subagent transcripts (side-chains) are shown with `--sidechains` but not replayed
  separately; the target harness spawns its own.
- Live reruns need the target harness to be logged in (`claude /login`, `codex login`).
