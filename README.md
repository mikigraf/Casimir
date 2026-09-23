# casimir

> 1.0 candidate work is in progress. Do not treat fixture tests as provider or evaluation
> certification. See [release gates](docs/production-readiness.md), [migration](docs/migration.md),
> [privacy](docs/privacy.md), and [troubleshooting](docs/troubleshooting.md).

Replay, rerun, and compare coding-agent sessions recorded by **Claude Code**, **OpenAI Codex**,
**GitHub Copilot CLI**, and **Gemini CLI**. Claude Code and Codex are the supported targets;
Copilot and Gemini are experimental. Written in Rust; the single binary needs Git and the chosen
provider CLI for live experiments. The optional Anthropic API backend uses in-process HTTPS.

These harnesses record local session transcripts. `casimir` reads those logs,
normalizes them into one event model, and lets you:

- **list / show / play** any past session as a readable transcript, with the original pacing;
- **rerun** a session's user turns against a different model or harness in a fresh Git worktree;
  verified Casimir checkpoints restore the recorded workspace and conversation for forks;
- **simulate the user** for follow-up turns when the rerun diverges from the original, so the
  replay keeps pursuing the same goals instead of replying to things that never happened;
- **compare** two sessions (or a session and its rerun): tool usage, files touched, tokens, cost,
  duration, workspace diff, end-state similarity, final answer, and optionally an LLM judge that is
  run in both candidate orders so position bias is caught instead of reported as a verdict;
- **replicate** reruns (and vary the simulator model) to get pass@1, pass^k and score spreads instead
  of a single, unrepeatable result.

The [research audit](docs/research.md) explains the experimental objective, primary sources, and
where Casimir uses approximations rather than reproducing a paper’s method.

## Install

```
cargo install --path . --locked  # puts `casimir` on your PATH
# or
cargo build --release         # binary at target/release/casimir
```

Requires a Rust toolchain (1.85+) and a C linker.

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
                        [--llm auto|claude-cli|codex-cli|api|cmd] [--llm-model M]
                        [--original-diff RUN_DIR] [-o DIR] [--dry-run]
                        [-- extra args for the harness CLI]
casimir fork <session> --at-turn N [--message "..."] [rerun options]
casimir attribute <session> [--turns-at 2,3,4] [rerun options]
casimir compare <a> <b> [--judge] [--judge-model M] [--judge-repeats N] [--brief FILE] [--format text|md|json]
casimir brief <session> [-o brief.json]          draft a per-session rubric / analysis / intents for review
casimir pairs <run-dir>... -o DIR                blinded original-vs-simulated pairs for human spot checks
casimir pairs-score <pairs.key.json> <answers.json>
casimir runs

casimir doctor [--json]                          harness versions, login status, storage; no model calls
casimir resume <run-dir> [--retry-interrupted]   continue a durable run from its recovery journal
casimir cleanup <run-dir> [--apply] [--checkpoints]
                                                 preview (or apply) removal of a run's artifacts and worktrees
casimir predict-evaluation --corpus F -o F       judge predictions for the frozen evaluation corpus
casimir calibrate --corpus F --predictions F --reviewer-a F --reviewer-b F --adjudication F
                                                 score predictions against independent human review
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
5. The workspace diff is captured against the initial base (including committed and untracked edits), and a comparison report is written:

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
When the input is a saved run, its captured patch is reused as the reference even if that
run's workspace has changed since. `--original-diff` overrides it explicitly.

Permissions default to preserving the harness configuration. Unrestricted execution requires
`--allow-unrestricted` plus the requested bypass setting, including when supplied after `--`.
Worktrees separate repository edits; they are not OS sandboxes. Codex has a native Windows
sandbox; Claude Code currently has no native Windows OS sandbox.

Use `casimir doctor --json` to diagnose setup without paid calls. Harness turns default to a
15-minute timeout; judge/simulator calls default to five minutes (`--turn-timeout` and
`--llm-timeout`). Interrupted runs use `casimir resume RUN`; ambiguous turns require
`--retry-interrupted` and create a new attempt from a verified checkpoint.

Supply `--checks checks.json` for frozen executable validation. Reports distinguish process
execution, executable checks, judge assessment and overall task outcome. Clean execution
without evaluation evidence is inconclusive; a failed required check cannot become a pass.

Preview artifact removal with `casimir cleanup RUN`; add `--apply` to remove owned artifacts.
Use `casimir export RUN --share` for redacted sharing exports.

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
must-have flags, and atomic intents; edit the JSON, set `humanReviewed` to true, and pass `--brief`.
The human-refined rubric study reports kappa 0.57 on its full dataset and 0.75 on the
unanimous-human subset (different subsets, not successive refinement stages; arXiv 2511.10865). The
same paper found 43.5 percent of test-passing agent patches judged invalid, so the judge is also
asked to check root cause and to list **invalid reasons** from a fixed taxonomy: requirement
violation, root cause not addressed, incomplete implementation, new issues introduced. Reruns that
judge or simulate draft a brief automatically into the run directory when none is passed. Matrices
and attribution draft it once and reuse it across every replicate, alongside a frozen reference
diff. Judges receive bounded recorded tool inputs/results as evidence for requirements such as
committing or running verification. User requests take precedence over requirements invented by an
unreviewed draft rubric; a draft still needs review before a research experiment.

End-state similarity measures agreement with one human trajectory, not correctness; the report
labels it as such and never uses it as a validity signal. Empty or binary-only reference patches are
marked uninformative and excluded from aggregate line-overlap scores.

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
its divergence from the original: the first user turn whose canonical action sequence differs, and a
tool-sequence distance (one minus the longest-common-subsequence similarity of canonical actions). A
target group is flagged as beyond the observed control spread when its mean distance exceeds the
control group's largest distance. This is a descriptive threshold, not a statistical significance
test. Fork distances use only the requested post-fork suffix; preserved history does not dilute
divergence. Canonical-kind distance does not detect changed arguments within the same action kind. A
control requires a recorded original model. If the reported control model differs from that
identifier, or either group fails to complete cleanly, the control comparison is withheld.

### Fork at a turn

`casimir fork <session> --at-turn N [--message "..."]` restores the recorded workspace,
Git index and native conversation into a fresh worktree before continuing at turn N.
It requires a verified Casimir checkpoint and a validated installed transcript version.
Historical imported logs support inspection and full reruns, but commit timestamps and
inferred state are insufficient for forks. External services, files and process memory
are outside checkpoint coverage. See [checkpoint limitations](docs/checkpoints.md) and
[the compatibility manifest](compatibility/harnesses.json).

### Attribution: the point of commitment

`casimir attribute <session> --turns-at 2,3,4 --judge` resamples the original harness and model
at each listed turn. It requires verbatim user turns and full-task evaluation. A clean exit alone
cannot show that a failed task was rescued. The report withholds a candidate point of commitment
unless the judge consistently scores the original below the pass threshold.

Resampling turn k also re-rolls downstream decisions. Inspired by arXiv 2606.08275, Casimir selects
the latest tested turn with observed rescues and a positive Wilson lower bound. These are intervals
for rescue proportions, not paired causal effects. The result is a turn-level diagnostic conditional
on the judge and verified checkpoint, not proof of a causal step.

### Process metrics and anti-patterns

Every tool call is mapped onto a canonical action vocabulary (file read, file write, search,
command, plan, navigate, fetch, agent spawn, reason) so trajectories from different harnesses are
comparable, and three anti-patterns are labelled with deterministic rules (arXiv 2607.06184): a
**search loop** is ten or more consecutive search or read actions with no write and no validation
command; **re-read churn** is the same file read three or more times in a ten-action window with no
intervening write; a **verification skip** is no recognized test, build, or lint command in the
overlap of the final five actions and the region after the last write (the paper’s tail-validation
diagnostic). Search loops and churn were more common in failed than in resolved SWE-bench runs
(search loops 56 vs 41 percent, churn 45 vs 34 percent). Stats and comparisons show them together
with the failed-action share and exploration share. A replicate that passes but shows an
anti-pattern is flagged as a **lucky pass**, and each group reports a principled pass rate that
excludes them (after the "lucky pass" analysis in arXiv 2605.12925, where ranking by process quality
reordered models materially). These are Casimir heuristics, not AgentLens quality classifications; a
tail-validation flag is descriptive and does not by itself invalidate a successful patch.

### Record envelopes

Each run directory also gets `record.jsonl`: every model reply and every tool call of the rerun as
an envelope addressed by boundary and occurrence (`model[3]`, `tool:Bash[7]`) with its input,
output, and drift metadata (harness, version, model, permission mode, sandbox, simulator). This
follows Chronicle's record design (arXiv 2609.20625) and is bookkeeping for auditing and diffing
runs; it is not a replay mode. Envelopes follow call order and retain unanswered or orphaned tool
events. Inherited fork records are marked. Full model request inputs are unavailable in the
normalized logs and are explicitly marked `inputAvailable: false`.
`sourceTurn` records the corresponding turn in the immediate original session, so skipped
simulator turns do not shift comparison, intent-coverage, or blinded-pair alignment.

### Replicates

Agent runs are noisy, so one rerun is not a result. `--replicates N` runs the same replay N times
(each in its own worktree and run directory) and reports pass@1 (fraction that passed), pass^k
(every replicate passed), the judge-score range, end-state similarity, and token and tool-call
spreads. A replicate passes when it finishes every turn without a harness error and, if judged,
scores at least `--pass-threshold` (default 7 of 10) with no explicit invalidity findings. Without a
judge, pass@1 measures execution completion, not task correctness. Submitted prompts and failed
turns are not counted as completed. Simulator no-ops are recorded separately; an early `goals_met`
stop counts as success only with a passing judge. Each group also gets a Casimir majority verdict
(inspired by the two-of-three success threshold in arXiv 2512.06749): **validated** when at least
two thirds pass, **partial** when at least one but fewer than two thirds pass and at least two
thirds complete cleanly, **inconclusive** when fewer than two thirds complete cleanly, **refuted**
when none pass despite sufficient completion. These buckets do not reproduce DoVer’s
intervention-fulfillment and milestone-progress categories. Replicates default to three whenever a
judge, a control group, a fork, or attribution is involved. Pass one or more `--sim-model` values to
run the matrix once per simulator model; the summary groups results by simulator so
simulator-induced variance is visible instead of hidden. Every matrix replicate uses a fresh
worktree. For a matrix, `--workspace DIR` names its source git repository; `--workspace same` and
non-git directories are rejected. Single runs still support explicit in-place execution. Dry runs
create no experiment files or worktrees. Existing run outputs are never overwritten. Harness
failures and missing requested judge results return a nonzero CLI exit status after saving
diagnostics.

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

The simulator (`--sim-model`, `--sim-llm`) and judge (`--judge-model`, `--judge-llm`) are
configured independently and default to `--llm-model` / `--llm`. `auto` uses a signed-in
Claude Code subscription first, then a signed-in Codex subscription; it never switches to an
API key because one happens to be in your shell. Sign in with `claude auth login` or `codex login`,
then check `casimir doctor --json` for `subscriptionReady: true`. For headless Codex machines,
`codex login --device-auth` is supported when your account allows it. The Claude helper runs
`claude -p` with tools disabled; the Codex helper runs `codex exec` in a private temporary
directory with read-only permissions. Both preserve the CLI's subscription login and report
token usage; a monetary price is unknown when the provider does not report one. The Codex helper
uses the CLI's default model unless `--llm-model` or `--judge-model` selects one explicitly.

An explicit `--llm api` remains available for existing API-key workflows. `--llm cmd` runs
`$CASIMIR_LLM_CMD` (prompt on stdin, system prompt in `$CASIMIR_LLM_SYSTEM`). Provider CLI
subprocesses ignore API-key environment overrides so a configured key cannot silently charge
API usage. The selected backend and model are recorded with each call. Claude's reported
`costUsd` is its client-side API-equivalent estimate, not a subscription bill; Codex CLI cost
is reported as unknown. Token usage and any known estimate remain visible.

Provider references: [Codex authentication](https://learn.chatgpt.com/docs/auth),
[Codex noninteractive mode](https://learn.chatgpt.com/docs/non-interactive-mode), and
[Claude Code noninteractive mode](https://code.claude.com/docs/en/headless).

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
cargo test          # parsers, renderers, comparison, reruns and reliability, driven by compiled fixtures
python3 -m unittest discover -s tests -p 'test_*.py'
cargo fmt --check && cargo fmt --check --manifest-path tests/fixture/Cargo.toml
cargo clippy --all-targets -- -D warnings
cargo build --release
```

CI runs the Rust and Python tests on Linux, macOS and Windows with stable Rust and the minimum
supported Rust 1.85, plus rustfmt, strict Clippy and a release build. The opt-in live check requires Python 3 and an authenticated Claude Code CLI;
it consumes provider subscription usage and keeps its worktrees and reports for inspection:

```
python3 scripts/smoke-claude.py --output .context/live-check
```

Use a fresh output directory for each run, or `--resume` to reuse completed stages and continue
missing ones. A valid experiment can report a model failing a requirement: the check verifies
that completion and judge evidence agree with the reported outcome, rather than demanding a
perfect model pass rate. It checks actual file contents, two-turn resume,
committed workspace restoration, native transcript forking, reference-patch reuse, matched
controls, judging, user simulation, intent coverage, and attribution withholding for a successful
original. See [validation results](docs/validation.md) for the tested scope and provider limits.

Environment knobs: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `COPILOT_HOME`, `GEMINI_CLI_HOME`,
`CASIMIR_HOME` (runs and worktrees), `CASIMIR_CLAUDE_BIN`, `CASIMIR_CODEX_BIN`,
`CASIMIR_COPILOT_BIN`, `CASIMIR_GEMINI_BIN` (alternate harness executables), `CASIMIR_LLM_CMD`,
`CASIMIR_DEBUG`.

## Limitations

- A rerun replays the user's *inputs*. Verified Casimir checkpoints restore the recorded
  repository workspace and conversation; network state, external files/services, installed
  tools and process memory are outside checkpoint coverage.
- Imported historical sessions can be inspected and fully rerun. They cannot be forked until
  a compatible checkpoint exists; an inferred commit or timestamp is never sufficient.
- Claude Code subagent transcripts (side-chains) are shown with `--sidechains` but not replayed
  separately; the target harness spawns its own.
- Live supported reruns need a subscription login (`claude auth login` or `codex login`).
  Experimental Copilot/Gemini reruns use their own login flows. Gemini reruns set
  `GEMINI_CLI_TRUST_WORKSPACE=true` so headless mode runs in the worktree.
- Copilot CLI and Gemini CLI store formats are undocumented or internal and may change; their
  adapters remain experimental and lack authenticated live acceptance on this machine.
- Replicates and order-swapped judging consume subscription usage: N replicates × 2 × `--judge-repeats`
  judge calls, plus one shared brief draft per experiment and an intent-coverage call per judged
  simulated run.
- Replicate statistics are thin: the per-group pass@1 interval is a Wilson interval treating
  replicates as independent, and three replicates provide only a coarse estimate; inspect the interval rather than assuming
  a fixed percentage-point resolution. Serving-stack effects in the cited studies do not guarantee reproducibility for the installed
  harnesses; seed controls and adaptive stopping are not implemented.
- No standard trace export mapping is implemented. The normalized `session.json` and
  `record.jsonl` are the interchange formats.
