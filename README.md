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
                        [--replicates N] [--control] [--sim-model M ...] [--sim-llm B]
                        [--judge] [--judge-model M] [--judge-llm B] [--judge-repeats N]
                        [--brief FILE] [--pass-threshold 7]
                        [--llm auto|api|claude-cli|cmd] [--llm-model M]
                        [--original-diff RUN_DIR] [-o DIR] [--dry-run]
                        [-- extra args for the harness CLI]
casimir fork <session> --at-turn N [--message "..."] [rerun options]
casimir attribute <session> [--turns-at 2,3,4] [rerun options]
casimir compare <a> <b> [--judge] [--judge-model M] [--judge-repeats N] [--brief FILE] [--format text|md|json]
casimir brief <session> [-o brief.json]          draft a per-session rubric / analysis / intents for review
casimir pairs <run-dir>... -o DIR                blinded original-vs-simulated pairs for human spot checks
casimir pairs-score <pairs.key.json> <answers.json>
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
  record.jsonl    every model reply and tool call as an addressable envelope
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
the mean per-file Jaccard over added and removed lines, plus a **recall** of the reference diff's
lines reproduced by the other side. Recall does not penalize harmless extra edits, which is why a
solution-distance study preferred it over a symmetric measure (arXiv 2606.17454). Tool-sequence
and canonical action-sequence similarity are shown too, but only as descriptive numbers. The original session's diff comes from a run directory
(`--original-diff`), or is reconstructed from git: the commits between the base commit and the last
commit before the session ended, or, failing that, the working tree against the base commit
(labelled as a heuristic in the report).

### Judging: position, repeatability, family, and rubric

LLM judges flip their verdict when the two candidates are swapped, and the flip rate is large:
42 to 79 percent of pairs for open-weight judges in one study, with production judges showing a
first-slot preference of 0.125 to 0.192 (arXiv 2609.17857, 2606.19544). `--judge` therefore always
asks in both orders, averages the scores, and reports a **tie flagged as order-sensitive** when
the two verdicts disagree. The tie is the headline; the averaged scores remain the primary
continuous outcome, and a high tie rate across replicates is reported as a judge-quality problem,
not as "no difference".

Every judgement reports the **first-slot win rate** and the resulting **position bias** (its
distance from 0.5; the reliability gate in arXiv 2606.19544 is below 0.10). Position bias is
independent of repeatability, so `--judge-repeats N` runs each ordering N times at the same
settings and reports **test-retest** agreement separately from agent-run variance; a judge with
test-retest above 0.95 and position bias above 0.10 is flagged **reliable-but-biased**.

Judges also favour their own model family by roughly 3 to 8 points of win share with quality held
fixed (arXiv 2609.17857). Casimir infers the family of the judge and of both candidates and warns
when the judge shares a family with exactly one of them, which is the common case of a Claude judge
comparing Claude Code against another harness. Pick `--judge-model` from another family for the
headline verdict when that warning appears.

A **per-session brief** grounds the judge in the task. `casimir brief <session>` drafts an
objective, constraints, intervention conditions, a rubric of 5 to 10 checkable criteria with
must-have flags, and atomic intents; edit the JSON, set `humanReviewed` to true, and pass
`--brief`. A human-refined rubric was what lifted judge-human agreement on patch validity from a
kappa of 0.38 to 0.57 to 0.75 in arXiv 2511.10865; the same paper found 43.5 percent of
test-passing agent patches judged invalid, so the judge is also asked to check root cause and to
list **invalid reasons** from a fixed taxonomy: requirement violation, root cause not addressed,
incomplete implementation, new issues introduced. Reruns that judge or simulate draft a brief
automatically into the run directory when none is passed.

End-state similarity measures agreement with one human trajectory, not correctness; the report
labels it as such and never uses it as a validity signal.

### Live re-execution, not log stitching

A rerun always re-executes the target harness live. Substituting one model's recorded outputs into
another model's trajectory is not a valid comparison: when live SWE-bench trajectories are forked
and continued by a different model, most of what follows is rewritten and only a few percent of
the recorded states remain valid (arXiv 2608.08239). The recorded raw output is kept as an
addressable record for auditing (see below), never used as a replay source.

### Control groups: the same-model noise floor

The same study found that same-model forks at temperature zero also diverge, by an amount that
depends on the serving stack. So a swap that "looks different" may be noise. `--control` adds a
group that reruns the **original** harness and model alongside the target. Every replicate reports
its divergence from the original: the first user turn whose canonical action sequence differs, and
a tool-sequence distance (one minus the longest-common-subsequence similarity of canonical
actions). A target group is only called different when its mean distance exceeds the control
group's largest distance; otherwise the report says so explicitly.

### Fork at a turn

`casimir fork <session> --at-turn N [--message "..."]` preserves turns 1 to N-1 verbatim, writes a
truncated transcript under a new session id where the harness will find it, checks out a worktree
at the last commit before turn N, and resumes the session natively with either the original
turn-N message (a resample) or an edited one (an intervention). In-situ intervention at the
suspected failure step flipped 17.6 percent of failed trials in one study, while end-of-trace
self-refinement flipped none (arXiv 2512.06749). Message-only logs cannot restore the agent's
state, which is why the fork leans on the harness's own resume: Claude Code and Codex are
supported; Copilot CLI and Gemini CLI cannot resume a truncated transcript. The plan notes
whether the workspace before turn N could be restored exactly, from commits, or only heuristically
(no commit between the session base and turn N although files were edited).

### Attribution: the point of commitment

`casimir attribute <session> --turns-at 2,3,4` resamples the session at each listed turn with
replicates and reports the pass rate with a Wilson interval per turn. Resampling turn k re-rolls
everything after it, so early turns show spurious effects; the causal locus is the **latest** turn
whose interval still excludes zero, the last point at which re-deciding still rescues the run
(arXiv 2606.08275).

### Process metrics and anti-patterns

Every tool call is mapped onto a canonical action vocabulary (file read, file write, search,
command, plan, navigate, fetch, agent spawn, reason) so trajectories from different harnesses are
comparable, and three anti-patterns are labelled with deterministic rules (arXiv 2607.06184): a
**search loop** is ten or more consecutive search or read actions with no write and no validation
command; **re-read churn** is the same file read three or more times in a ten-action window with no
intervening write; a **verification skip** is no recognized test, build, or lint command after the
last source write. These were more common in failed than in resolved SWE-bench runs (search loops
56 vs 41 percent, churn 45 vs 34 percent). Stats and comparisons show them together with the
failed-action share and exploration share. A replicate that passes but shows an anti-pattern is
flagged as a **lucky pass**, and each group reports a principled pass rate that excludes them
(after the "lucky pass" analysis in arXiv 2605.12925, where ranking by process quality reordered
models materially).

### Record envelopes

Each run directory also gets `record.jsonl`: every model reply and every tool call of the rerun as
an envelope addressed by boundary and occurrence (`model[3]`, `tool:Bash[7]`) with its input,
output, and drift metadata (harness, version, model, permission mode, sandbox, simulator). This
follows Chronicle's record design (arXiv 2609.20625) and is bookkeeping for auditing and diffing
runs; it is not a replay mode.

### Replicates

Agent runs are noisy, so one rerun is not a result. `--replicates N` runs the same replay N times
(each in its own worktree and run directory) and reports pass@1 (fraction that passed), pass^k
(every replicate passed), the judge-score range, end-state similarity, and token and tool-call
spreads. A replicate passes when it finishes every turn without a harness error and, if judged,
scores at least `--pass-threshold` (default 7 of 10). Each group also gets a majority verdict
(arXiv 2512.06749): **validated** when at least two thirds pass, **partial** when fewer pass but
at least two thirds completed cleanly, **inconclusive** when most did not complete, **refuted**
otherwise. Replicates default to three whenever a judge, a control group, a fork, or attribution is
involved. Pass one or more `--sim-model` values to run the matrix once per simulator model; the
summary groups results by simulator so simulator-induced variance is visible instead of hidden.

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

**Simulator calibration.** LLM user simulators make tasks easier: against 451 real users, agent
success was 63.6 percent, while most simulators put it 14 to 20 points higher, and swapping only
the simulator model moved agent success by about 9 points (arXiv 2603.11245, 2601.17087). Casimir
therefore treats the simulator as a blocking factor: groups are keyed by simulator model, the
control-group comparison is only made between groups sharing a simulator, the matrix reports the
**between-simulator spread** next to the between-target spread, and simulated pass rates are
labelled as relative comparisons rather than absolute task success. Persona prompting does not
close the gap and can widen it, so casimir measures the residual **simulator drift** instead:
lexicon counters over the adapted turns versus the recorded human's own turns in the same session
(short turns, politeness, hedging, pivots, questions, em dashes, identifier tokens, words per turn).
When a brief is available and a judge runs, **intent coverage** is computed as in SWE-Together
(arXiv 2606.29957): recall of the original intents re-expressed by the simulated user and
precision of simulated messages that stay in scope, combined as 0.7 recall plus 0.3 precision.
The simulator is conditioned on the brief's session analysis, may answer **no-op** at a turn whose
intent is already satisfied, and labels every message it sends as verbatim, answer, question,
redirect, or new requirement. For occasional human checks, `casimir pairs` exports blinded
original-versus-simulated message pairs with a separate key, and `casimir pairs-score` reports the
Turing pass rate with a Wilson interval (0.5 means indistinguishable).

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

# is sonnet really different, or is it noise? add a same-model control group
casimir rerun claude:last --model sonnet --control --replicates 3

# what would have happened if I had said something else at turn 3?
casimir fork claude:last --at-turn 3 --message "Use the existing helper instead of adding a new one"

# which turn committed the failed session to its outcome?
casimir attribute claude:last --turns-at 2,3,4 --judge

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
- Replicates and order-swapped judging multiply API cost: N replicates × 2 × `--judge-repeats`
  judge calls, plus one brief draft and one intent-coverage call per run.
- Replicate statistics are thin: the per-group pass@1 interval is a Wilson interval treating
  replicates as independent, and three replicates cannot resolve differences below roughly ten
  percentage points; the report says so. No verified guidance on temperature or seed effects,
  serving-stack variance, or adaptive stopping was found.
- No trace export format is offered. Two research passes produced no verified evidence on which
  schema (OpenTelemetry GenAI conventions, OpenInference, SWE-agent or OpenHands trajectories) a
  tool like this should target, so the normalized `session.json` and `record.jsonl` stay the
  interchange formats rather than an unverified mapping.
