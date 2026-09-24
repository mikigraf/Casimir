# Methodology

This page explains how Casimir runs and scores an experiment, and why it makes the choices it
does. For the papers behind these choices and where Casimir departs from them, see the
[research notes](research.md).

## Reading the logs

Each agent has its own log format. Casimir turns all of them into one event model.

| Agent | What's in the log |
|---|---|
| Claude Code | One JSON record per line. `user` and `assistant` records carry the API-shaped message (text, thinking, tool_use, tool_result) plus cwd, branch, version and permission mode. |
| Codex | `session_meta` (cwd, git commit, CLI version), `turn_context` (model, sandbox), `response_item` (messages, reasoning, function calls, apply_patch) and `event_msg` (token counts, lifecycle). |
| Copilot CLI | `workspace.yaml` (id, cwd, branch, timestamps, summary) and `events.jsonl` (`user.message`, `assistant.message`, `tool.execution_start/complete`, and `session.shutdown` with per-model token metrics). GitHub describes this schema as internal. |
| Gemini CLI | A metadata line, then message records (`user`/`gemini` with `toolCalls`, `thoughts`, `tokens`, `model`), `$set` upserts and `$rewindTo`. The cwd comes from the project's `.project_root`, or `projects.json` for hashed directories. Older single-object `.json` files are also read. |

Agents inject their own context into the conversation: system reminders,
`<environment_context>`, `<session_context>`, AGENTS.md instructions, slash command echoes and
subagent side-chains. Casimir recognizes these and keeps them out of the user turns. A rerun
sends only what the person actually typed, and the target agent adds its own context.

## Running a rerun

**Workspace.** If the original session ran in a Git repository, Casimir creates a detached
worktree under `~/.casimir/worktrees/<run>` at the base commit. For Codex that's the commit it
recorded. For Claude Code it's the last commit on the recorded branch before the session
started. `--workspace same` runs in place instead, and `--workspace DIR` uses DIR as the source
repository for a new worktree.

**Turns.** The first prompt goes to the target agent verbatim, using its headless mode:

- `claude -p --output-format stream-json`
- `codex exec --json`
- `copilot -p --output-format json`
- `gemini -p --output-format stream-json`

Later prompts are sent verbatim or through the [user simulator](#the-user-simulator). Casimir
resumes the agent's session between turns so it keeps its own context, as it would if you
were typing.

**Capture.** Events stream to the terminal as they arrive. When the agent is done, Casimir
reads the agent's own log of the new session so the rerun is recorded in the same detail as the
original. The workspace diff is taken against the starting commit and includes committed,
staged, unstaged and untracked changes.

**Reference diff.** To compare end states, Casimir needs to know what the original session
changed. In order of preference it uses:

1. the run directory or patch you pass with `--original-diff`;
2. the saved patch, when the input is itself a Casimir run (even if that run's worktree has
   changed since);
3. the commits between the base commit and the last commit before the session ended;
4. the working tree compared to the base commit. The report labels this one as a guess.

**Outcomes.** Reports keep four things apart: whether the agent process finished, whether your
`--checks` passed, what the judge concluded, and the overall task outcome. A clean exit with no
checks or judge is *inconclusive*. A failed required check is always a failure, whatever the
judge says.

## Why reruns are always live

Casimir always runs the target agent for real. It never splices one model's recorded outputs
into another model's trajectory. When live SWE-bench trajectories are forked and handed to a
different model, most of what follows gets rewritten, and only a few percent of the recorded
states are still valid ([arXiv 2608.08239](https://arxiv.org/html/2608.08239)). The raw
recording is kept for auditing (see [record envelopes](#record-envelopes)), but it's never
replayed.

## Scoring the end state

Two agents can reach the same result by different routes, and that should count as a success.
So the main comparison looks at where each run ended up, not how it got there.

- **End-state similarity** is the Jaccard index over the sets of changed files, multiplied by
  the mean per-file Jaccard index over added and removed lines.
- **Recall** is the share of the reference diff's lines that the other side reproduced. It
  doesn't penalize harmless extra edits, which is why a solution-distance study preferred it to
  a symmetric measure ([arXiv 2606.17454](https://arxiv.org/html/2606.17454)).

Tool-sequence and action-sequence similarity are shown too, but only as descriptive numbers.

Keep in mind that both numbers measure agreement with one recorded human session, not
correctness. The report says so and never treats similarity as proof that a run was valid.
Empty or binary-only reference patches don't carry any line information, so they're marked as
uninformative and left out of aggregate line-overlap scores.

## The judge

`--judge` asks an LLM to compare the two runs. LLM judges have a few well-known problems, and
Casimir tries to measure each one rather than hide it.

**Position bias.** Judges often change their verdict when you swap the order of the two
candidates. One study saw this in 42 to 79 percent of pairs for open-weight judges, and
production judges preferred the first slot by 0.125 to 0.192
([arXiv 2609.17857](https://arxiv.org/abs/2609.17857),
[2606.19544](https://arxiv.org/abs/2606.19544)). So Casimir always asks in both orders and
averages the scores. If the two verdicts disagree, the result is a tie marked
*order-sensitive*. The averaged scores are still the main number. A high rate of
order-sensitive ties across replicates points to an unreliable judge, not to two equally good
runs.

Each judgement reports the first-slot win rate and the resulting position bias (its distance
from 0.5). The reliability study above uses 0.10 as its threshold.

**Repeatability.** Position bias and repeatability are separate things. `--judge-repeats N`
asks the same question N times in each order and reports test-retest agreement, kept apart from
the variance between agent runs. A judge with test-retest above 0.95 but position bias above
0.10 is flagged as *reliable but biased*.

**Self-preference.** Judges tend to favour their own model family by roughly 3 to 8 points of
win share at equal quality ([arXiv 2609.17857](https://arxiv.org/abs/2609.17857)). Casimir
works out the family of the judge and of both candidates, and warns when the judge shares a
family with exactly one of them. The common case is a Claude judge comparing Claude Code with
another agent. If you see that warning, use a `--judge-model` from a different family for the
headline result.

**Rubrics.** A per-session brief gives the judge something concrete to score against.
`casimir brief <session>` drafts one with:

- the objective and constraints;
- when the user would step in;
- a rubric of 5 to 10 checkable criteria, some marked as must-haves;
- a list of atomic user intents.

Edit the JSON, set `humanReviewed` to `true`, and pass it with `--brief`. A study of
human-refined rubrics reports a kappa of 0.57 on its full dataset and 0.75 on the subset where
the human raters were unanimous. Those are two different subsets, not two stages of
refinement ([arXiv 2511.10865](https://arxiv.org/html/2511.10865)). The same paper found that
43.5 percent of agent patches that passed the tests were still invalid. So the judge also
checks whether the root cause was fixed, and lists any *invalid reasons* from a fixed set:

- requirement violation
- root cause not addressed
- incomplete implementation
- new issues introduced

If you don't pass a brief, reruns that judge or simulate draft one into the run directory. A
matrix or attribution run drafts it once and reuses it for every replicate, together with a
frozen reference diff. The judge also sees a bounded sample of recorded tool inputs and
outputs, so it can check requirements like "commit the change" or "run the tests". What the
user actually asked for outranks anything an unreviewed draft rubric made up. Review the draft
before you rely on it for research.

## Replicates

Agent runs are noisy, so a single rerun isn't a result. `--replicates N` runs the same replay N
times, each in its own worktree and run directory, and reports:

- **pass@1**: the fraction of replicates that passed;
- **pass^k**: whether every replicate passed;
- the range of judge scores, end-state similarity, and the spread of tokens and tool calls.

A replicate passes when it finishes every turn without an agent error and, if judged, scores
at least `--pass-threshold` (default 7 out of 10) with no invalid reasons. Without a judge,
pass@1 only tells you the run completed, not that the task was done. A prompt that was sent but
never finished doesn't count as completed. Simulator no-ops are recorded separately, and an
early `goals_met` stop only counts as a success if the judge passes it.

Each group also gets a majority verdict, loosely based on the two-out-of-three rule in
[DoVer](https://arxiv.org/html/2512.06749):

| Verdict | When |
|---|---|
| validated | at least two thirds pass |
| partial | at least one passes but fewer than two thirds, and at least two thirds complete cleanly |
| inconclusive | fewer than two thirds complete cleanly |
| refuted | none pass, even though enough completed |

These buckets don't reproduce DoVer's intervention-fulfillment and milestone categories.

Replicates default to three whenever a judge, a control group, a fork or attribution is
involved. Pass `--sim-model` more than once to run the whole matrix once per simulator model.
Results are grouped by simulator so you can see how much the simulator itself moves things.

Some rules for matrices:

- Every replicate gets a fresh worktree. `--workspace DIR` names the source repository;
  `--workspace same` and non-Git directories are rejected. Single runs can still run in place.
- `--dry-run` creates no files and no worktrees.
- Existing run output is never overwritten.
- Agent failures and missing judge results give a nonzero exit status, after diagnostics are
  saved.

## Control groups

Even the same model at temperature zero can diverge from itself, by an amount that depends on
the serving stack ([arXiv 2608.08239](https://arxiv.org/html/2608.08239)). So a model swap that
*looks* different might just be noise.

`--control` adds a group that reruns the original agent and model next to the target. For every
replicate, Casimir reports how far it drifted from the original:

- the first user turn where the sequence of action kinds differs;
- a tool-sequence distance: one minus the longest-common-subsequence similarity of those
  action kinds.

If the target group's mean distance is larger than the largest distance in the control group,
it's flagged as beyond the control spread. That's a descriptive rule of thumb, not a
significance test.

A few details:

- For forks, only the part after the fork point is compared, so the shared history doesn't
  dilute the difference.
- The distance only looks at kinds of action. Two runs that call the same tool with different
  arguments look identical to it.
- A control needs a recorded model for the original session. If the control reports a
  different model, or either group fails to complete cleanly, the control comparison is
  withheld.

## Forking at a turn

`casimir fork <session> --at-turn N [--message "..."]` restores the recorded workspace, Git
index and native conversation into a fresh worktree, then continues from turn N. You can
replace the message at that turn or let it be resampled.

This needs a verified Casimir checkpoint and a transcript version that has been validated for
your installed agent. Older imported logs can be inspected and fully rerun, but commit
timestamps and inferred state aren't enough to fork from. External services, files outside the
repository and process memory are not captured. See [checkpoints](checkpoints.md) and the
[compatibility manifest](../compatibility/harnesses.json).

## Attribution

When a session failed, it's useful to know which turn sealed its fate.
`casimir attribute <session> --turns-at 2,3,4 --judge` resamples the original agent and model
starting at each listed turn and checks whether the task can still be rescued from there.

It needs verbatim user turns and a judge that evaluates the whole task, because a clean exit on
its own can't show that a failed task was rescued. If the judge doesn't consistently score the
original below the pass threshold, no turn is reported.

Resampling from turn k also re-rolls every decision after it. Following the idea in
[Causal Agent Replay](https://arxiv.org/html/2606.08275), Casimir picks the latest tested turn
that had rescues and a positive Wilson lower bound. Those are intervals for rescue rates, not
paired causal effects. Treat the answer as a turn-level diagnostic that depends on the judge
and the checkpoint, not as proof of cause.

## Process metrics and anti-patterns

Every tool call is mapped to a shared set of action kinds (file read, file write, search,
command, plan, navigate, fetch, agent spawn, reason) so runs from different agents can be
compared. Three anti-patterns are then detected with fixed rules, following
[arXiv 2607.06184](https://arxiv.org/html/2607.06184):

- **Search loop:** ten or more searches or reads in a row with no write and no validation
  command.
- **Re-read churn:** the same file read three or more times within ten actions with no write in
  between.
- **Verification skip:** no recognizable test, build or lint command in the overlap of the last
  five actions and everything after the last write (the paper's tail-validation check).

In that study, search loops and churn were more common in failed SWE-bench runs than in
resolved ones (search loops 56 vs 41 percent, churn 45 vs 34 percent). `stats` and comparisons
show these flags along with the share of failed actions and the share spent exploring.

A replicate that passes but shows an anti-pattern is flagged as a **lucky pass**. Each group
also reports a "principled" pass rate that leaves those out, after the analysis in
[AgentLens](https://arxiv.org/html/2605.12925), where ranking by process quality reordered
models noticeably. These are Casimir's own heuristics, not AgentLens's quality classifier, and a
verification-skip flag on its own doesn't make a working patch wrong.

## Record envelopes

Every run directory has a `record.jsonl`. It holds every model reply and tool call from the
rerun, each addressed by boundary and occurrence (`model[3]`, `tool:Bash[7]`), with its input,
output and drift metadata (agent, version, model, permission mode, sandbox, simulator). The
design follows [Chronicle](https://arxiv.org/html/2609.20625). It's there for auditing and
diffing runs; Casimir doesn't replay from it.

- Records are in call order and include tool calls that never got an answer or lost their
  parent.
- Records inherited from before a fork point are marked.
- The normalized logs don't contain the full model request, so those inputs are marked
  `inputAvailable: false`.
- `sourceTurn` points at the matching turn in the original session. That way, turns the
  simulator skipped don't throw off comparisons, intent coverage or blinded pairs.

## The user simulator

In a real session, later messages react to what the agent just did: "no, use the other
function", "yes, go ahead", "the test you added fails". Once a rerun goes a different way,
sending those messages verbatim stops making sense.

With `--user simulate`, an LLM sees every message the real user sent, what the original agent
had done before each one, and what the new agent has done so far. It then writes the message
this user would send now. It can:

- send the original verbatim, if it still applies;
- adapt it;
- skip the turn as a no-op, if that intent is already satisfied;
- stop, with a reason: `goals_met`, `cannot_adapt` or `out_of_scope`.

The simulator keeps notes across turns and reveals information gradually. It may not introduce
anything that isn't in the recorded session, and it has to cite the original turns an adapted
message is based on. If it fails to ground a reply three times, the original message is sent
verbatim instead. Each message it sends is labelled as verbatim, answer, question, redirect or
new requirement. When a brief is available, the simulator also sees the brief's session
analysis.

Simulated turns are marked in the transcript and stored with
`simulated: {verbatim, reason, grounded_in}`. The simulator's model and backend are saved with
the session and shown in every comparison, because the choice of simulator on its own changes
results.

### Calibrating the simulator

LLM user simulators make tasks look easier than they are. Against 451 real users, agents
succeeded 63.6 percent of the time, while most simulators put success 14 to 20 points higher.
Swapping only the simulator model moved agent success by about 9 points
([arXiv 2603.11245](https://arxiv.org/abs/2603.11245),
[2601.17087](https://arxiv.org/abs/2601.17087)).

So Casimir treats the simulator as its own experimental factor:

- groups are keyed by simulator model;
- control comparisons only happen between groups that share a simulator;
- the matrix reports the spread between simulators next to the spread between targets;
- simulated pass rates are labelled as relative comparisons, not absolute success rates.

Persona prompting doesn't close the gap and can make it worse. Instead, Casimir measures
**simulator drift**: simple word-level counters comparing the adapted turns to the real user's
own turns in the same session. It counts short turns, politeness, hedging, pivots, questions, em
dashes, identifier tokens and words per turn.

When there's a brief and a judge, Casimir also computes **intent coverage** as in
[SWE-Together](https://arxiv.org/abs/2606.29957): recall of the original intents that the
simulated user expressed, and precision of simulated messages that stayed in scope, combined as
0.7 × recall + 0.3 × precision.

For human spot checks, `casimir pairs` exports blinded pairs of real and simulated messages with
a separate answer key. `casimir pairs-score` reports how often people could tell them apart,
with a Wilson interval. A score of 0.5 means they couldn't.

## Judge and simulator backends

The judge (`--judge-model`, `--judge-llm`) and simulator (`--sim-model`, `--sim-llm`) are set up
separately and fall back to `--llm-model` and `--llm`.

- **`auto`** (default) uses a signed-in Claude Code subscription, then a signed-in Codex
  subscription. It never switches to an API key just because one is in your shell.
- **`claude-cli`** runs `claude -p` with tools turned off.
- **`codex-cli`** runs `codex exec` in a private temporary directory with read-only
  permissions. Unless you set `--llm-model` or `--judge-model`, it uses the Codex CLI's default
  model, so pin a model if you need comparisons to stay stable.
- **`api`** calls the Anthropic API directly with your API key.
- **`cmd`** runs `$CASIMIR_LLM_CMD` with the prompt on stdin and the system prompt in
  `$CASIMIR_LLM_SYSTEM`.

The CLI backends keep your subscription login and report token usage. Agent CLIs are launched
with API-key overrides removed, so a key in your environment can't quietly run up API charges.
The backend and model are recorded with every call. The `costUsd` that Claude reports is its own
estimate of the equivalent API price, not what your subscription costs. Codex doesn't report
cost, so it shows up as unknown.

Provider docs: [Codex authentication](https://learn.chatgpt.com/docs/auth),
[Codex non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode),
[Claude Code headless mode](https://code.claude.com/docs/en/headless).
