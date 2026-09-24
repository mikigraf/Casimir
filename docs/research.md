# Research notes

## What Casimir is trying to measure

Casimir turns a coding session you already had into a repeatable experiment. The question is:
would a different model or agent satisfy the same user intent, and how would the cost and the
process differ?

Each experiment is a live continuation in a fresh workspace. The original transcript tells you
how the agent behaved. It isn't a complete snapshot of the environment, and it isn't an answer
key.

## The papers behind it

These notes come from a review of the primary sources on 2026-09-22. For each paper they
separate what the paper found from what Casimir actually does with it. The papers' numbers
apply to the tasks, models and serving setups they studied; they aren't guarantees about
Casimir. The [methodology](methodology.md) page describes the resulting features in more
detail.

### [The Replay Gap](https://arxiv.org/html/2608.08239) (§3–4)

**Found:** Switching models mid-run changes what happens downstream, and even the same model
can diverge from itself. The study measures exact command edit distance on the part after the
fork and warns about comparing empty patches.

**Casimir:** Runs are always live, there's an optional same-model control group, and replicates
are isolated from each other. Casimir's distance (LCS over action kinds) is coarser than the
paper's. Flagging a target whose mean exceeds the control's maximum is a descriptive rule, not
a significance test.

### [DoVer](https://arxiv.org/html/2512.06749) (§4.2)

**Found:** Each intervention was repeated three times, and validation required at least two
successes. Partial validation also required the intervention to be fulfilled and measurable
progress on milestones.

**Casimir:** Defaults to three replicates when judging, running a control or forking. Casimir's
`partial`, `refuted` and `inconclusive` buckets only count completions and successes; they
**don't** implement DoVer's milestone-based categories. Truncated native transcripts are still
an experimental substitute for a full checkpoint.

### [Causal Agent Replay](https://arxiv.org/html/2606.08275) (§4, §7)

**Found:** Resampling an early step also re-rolls every decision after it. Under the paper's
assumptions, the latest significant rescue marks the point of commitment. The paper
distinguishes intervals on proportions from intervals on differences in effect.

**Casimir:** Attribution needs the original model, verbatim turns and a judge. No turn is named
unless the judge consistently puts the original below the pass threshold. Casimir reports
Wilson intervals on rescue proportions and calls the result a conditional, turn-level
diagnostic, not a proof of cause.

### [What Resolve Rate Hides / TraceProbe](https://arxiv.org/html/2607.06184) (Tables I–II)

**Found:** Nine canonical action types, deterministic diagnostics that take the target into
account, and completion evidence kept separate. Its tail-validation detector looks at the final
five actions after the last write. All of these signals are descriptive.

**Casimir:** Maps tools to the nine kinds, with an `other` fallback. It implements some of the
detectors but not the paper's target- and effect-aware Converge alignment. A tail-validation
flag alone isn't evidence of failure.

### [AgentLens](https://arxiv.org/html/2605.12925) (§5)

**Found:** Looking at process quality can separate weak successes, and efficient but unusual
solutions, from typical successful runs.

**Casimir:** The `lucky` flag only means that a passing run tripped one of Casimir's simple
detectors, and `principledPassAt1` leaves those runs out. Neither is the paper's
reference-based process quality score, and neither is a validated way to tell whether a patch is
correct.

### [Dissecting model behavior through agent trajectories](https://arxiv.org/html/2606.17454) (§4)

**Found:** A recall-based distance between patch features measures how much of a verified
solution was recovered. A separate oracle decides whether any extra edits are harmless.

**Casimir:** Reports recall next to symmetric similarity. Casimir only has one recorded
reference, not a set of verified solutions, so neither number establishes correctness. Diffs
are captured against the starting commit so that commits made by the agent are included.

### [Who Judges Matters](https://arxiv.org/abs/2609.17857)

**Found:** The open-weight model families studied prefer their own family's outputs and are
sensitive to the order candidates are shown in.

**Casimir:** Judges in both orders (AB and BA) and warns about family overlap. The warning is a
risk indicator, not a measured correction for every commercial model family.

### [Reliability without Validity](https://arxiv.org/abs/2606.19544)

**Found:** Repeatability, position bias and agreement with humans are three different
properties.

**Casimir:** Reports order effects and repeatability separately. A handful of calls on one task
can't validate a judge in general. Casimir reports modal agreement; it doesn't reproduce the
paper's full validation protocol.

### [Human-in-the-Loop Patch Evaluation](https://arxiv.org/html/2511.10865) (§3–5)

**Found:** Generate a task rubric and have a human refine it. Agreement is higher on the subset
where human raters were unanimous than on the full dataset.

**Casimir:** Freezes one brief for a whole experiment, records whether a human reviewed it, and
rejects runs with explicit invalidity findings even if their score is high. The 0.57 and 0.75
kappa figures come from different subsets of the evaluation, not from successive rounds of
refinement.

### [Mind the Sim2Real Gap](https://arxiv.org/abs/2603.11245) and [Lost in Simulation](https://arxiv.org/abs/2601.17087)

**Found:** Simulated users can change measured agent success and behave systematically
differently from real people.

**Casimir:** Keeps each simulator model in its own group, only compares controls that share a
simulator, and reports lexical drift and blinded human spot checks. Lexical similarity is not
the same as behaving like the real user.

### [SWE-Together](https://arxiv.org/abs/2606.29957)

**Found:** Evaluating interactive coding makes the case for tracking user intents explicitly.

**Casimir:** Keeps track of which original turn each simulated message is based on, and records
simulation choices, skipped turns and stop reasons. Intent coverage is a secondary,
judge-based metric.

### [Chronicle](https://arxiv.org/html/2609.20625) (§3, §6)

**Found:** Envelopes keyed by boundary and occurrence record inputs, outputs and drift metadata.
Whether replay is reliable depends on how much of the boundary is covered and how much state is
hidden.

**Casimir:** Writes deterministic, uniquely addressed records in call order, including tool
calls that were never answered or lost their parent. Records inherited from before a fork, and
model inputs that aren't available, are marked explicitly. Casimir doesn't implement Chronicle's
replay and doesn't make its reproducibility claims.

## Ground rules for experiments

1. Freeze the reference diff and the rubric before any candidate runs. Every replicate gets a
   fresh worktree at its planned commit. For a matrix, `--workspace DIR` names the repository.
2. Keep the raw stream and the execution status. Sending a prompt isn't the same as completing
   a turn. Agent crashes and incomplete output count as failures, including when
   `--continue-on-error` carries on past them.
3. Keep committed, staged, unstaged and untracked edits in the final diff against the base.
4. Without `--judge`, a pass only means the run completed cleanly. With a judge, it also needs
   a valid score at or above the threshold and no invalidity findings. A simulator stopping
   early with `goals_met` only counts as a success if the judge passes it. No-op turns are
   recorded separately.
5. Report it when the workspace was reconstructed heuristically, when the model is unknown,
   when the judge is noisy and when the sample is small. More decimal places won't fix any of
   these.
6. A dry run makes no model calls and writes no experiment output or worktrees. Invalid
   arguments fail before the experiment starts.

## How this is tested

`cargo test` covers the parsers, the live subprocess protocol (using a controlled fake agent),
worktree isolation, rubric reuse, commit capture, failed runs, judging, simulation, forks,
attribution and record serialization. These tests check orchestration and error handling. They
don't validate whether a judge's verdict on a task is right, or any research-level effect size.

The [validation report](validation.md) covers testing against live providers and its limits.
`scripts/smoke-claude.py` reruns the Claude workflow end to end and keeps the artifacts locally.

## Not implemented yet

Some research directions are out of scope for now: task-specific executable oracles, full
environment snapshots, paired effect estimates across many tasks, and calibration against
human-rated outcomes. Casimir doesn't claim to do any of these.
