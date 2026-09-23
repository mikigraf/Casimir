# What Casimir is trying to measure

Casimir turns an existing coding session into a repeatable experiment: would another model or
harness satisfy the same user intent, with what cost and process differences? The experimental
unit is a live continuation in a fresh workspace. A transcript is evidence of behavior; it is
not a complete environment checkpoint or a correctness oracle.

This audit (2026-09-22) checked the primary papers behind the feature brief. The table separates
their findings from Casimir's implementation choices. Paper results are specific to the studied
tasks, models, and serving configurations; their numerical results are not Casimir guarantees.

| Primary source | Finding relevant to this project | Implementation and limits |
|---|---|---|
| [The Replay Gap](https://arxiv.org/html/2608.08239), §3–4 | Live model switches alter downstream actions; same-model controls can also diverge. The study measures exact command edit distance on post-fork suffixes and warns about empty-patch comparisons. | Run live, provide a same-model control, and isolate replicates. Casimir's canonical-kind LCS distance is coarser than the paper's metric. Comparing a target mean to the control maximum is a descriptive rule, not a significance test. |
| [DoVer](https://arxiv.org/html/2512.06749), §4.2 | Three repetitions per intervention; validation requires at least two successes. Partial validation also requires intervention fulfillment and measured milestone progress. | Default to three when judging, controlling, or forking. Casimir's `partial/refuted/inconclusive` buckets use completion and success counts; they do **not** implement DoVer's milestone-based categories. Truncated native transcripts remain an experimental substitute for a full checkpoint. |
| [Causal Agent Replay](https://arxiv.org/html/2606.08275), §4, §7 | Resampling an early step also re-rolls downstream decisions. The latest significant rescue identifies a point of commitment under the paper's assumptions; it distinguishes proportion intervals from effect-difference intervals. | Attribution requires the original model, verbatim turns, and a judge. Withhold a candidate locus unless the judge consistently places the original below the success threshold. Report Wilson rescue-proportion intervals and describe the result as a conditional turn-level diagnostic, not a causal proof. |
| [What Resolve Rate Hides / TraceProbe](https://arxiv.org/html/2607.06184), Tables I–II | Nine canonical action types, deterministic target-aware diagnostics, and separate completion evidence. Its tail-validation detector looks at the final five actions after the last write. These signals are descriptive. | Map tools to the nine kinds plus an `other` fallback. Casimir implements a subset of the detectors; it does not reproduce target-and-effect-aware Converge alignment. The tail-validation flag alone is not evidence of failure. |
| [AgentLens](https://arxiv.org/html/2605.12925), §5 | Process-quality analysis can distinguish weak successes and efficient atypical solutions from conventional successful trajectories. | Casimir's `lucky` flag merely means a passing run triggered one of its simple detectors. `principledPassAt1` excludes those flags. Neither is the paper's reference-based process-quality score or a validated classifier of patch correctness. |
| [Dissecting model behavior through agent trajectories](https://arxiv.org/html/2606.17454), §4 | Recall-based patch-feature distance measures recovery of a verified solution; an independent oracle judges whether additional edits are harmless. | Report recall alongside symmetric similarity. Casimir uses one recorded reference, not a verified solution set, so neither measure establishes correctness. Capture against the initial base so agent commits are included. |
| [Who Judges Matters](https://arxiv.org/abs/2609.17857) | The studied open-weight families exhibit family-conditioned preferences and order sensitivity. | Judge both AB and BA and flag family overlap. This is a risk indicator, not a quantified correction for every commercial model family. |
| [Reliability without Validity](https://arxiv.org/abs/2606.19544) | Repeatability, position bias, and agreement with humans are distinct properties. | Report order and repeat diagnostics separately. A handful of calls on one task does not validate a judge population. Casimir reports modal agreement, not a full reproduction of the paper's validation protocol. |
| [Human-in-the-Loop Patch Evaluation](https://arxiv.org/html/2511.10865), §3–5 | Generate and human-refine a task rubric; agreement is higher on a unanimous-human subset than on the full dataset. | Freeze one brief across the entire experiment, record whether it was reviewed, and reject explicit invalidity findings even when the numerical score is high. The 0.57 and 0.75 kappa results use different evaluation subsets, not successive refinement stages. |
| [Mind the Sim2Real Gap](https://arxiv.org/abs/2603.11245), [Lost in Simulation](https://arxiv.org/abs/2601.17087) | Simulated users can change measured agent success and differ systematically from humans. | Keep simulator models as separate groups, match controls by simulator, and report lexical drift and blinded human spot checks. Lexical similarity is not behavioral calibration. |
| [SWE-Together](https://arxiv.org/abs/2606.29957) | Interactive coding evaluation motivates explicit user-intent tracking. | Preserve source-turn grounding and record simulation choices, skipped turns, and stopping reasons. Intent coverage is a secondary judge-based metric. |
| [Chronicle](https://arxiv.org/html/2609.20625), §3, §6 | Boundary/occurrence envelopes record inputs, outputs, and drift metadata; replay guarantees depend on boundary coverage and hidden state. | Write deterministic, uniquely addressed records in call order, including unanswered and orphaned tools. Mark inherited prefix records and unavailable model inputs explicitly. Casimir does not implement Chronicle's replay or make its reproducibility claims. |

## Operational contract

1. Freeze the reference diff and rubric before any candidate executes. Each replicate gets a fresh
   worktree at its planned commit; `--workspace DIR` supplies the repository for a matrix.
2. Preserve the raw stream and execution status. A submitted prompt is not a completed turn.
   Report harness crashes and incomplete output as failures, including when continuing after an error.
3. Preserve committed, staged, unstaged, and untracked edits in the final diff against the base.
4. Without `--judge`, a pass means clean execution only. With a judge, also require a valid score
   above threshold and no explicit invalidity findings. A simulator's early `goals_met` stop needs
   a passing judge before it can count as success. No-op turns are separately recorded.
5. Report heuristic workspace reconstruction, unknown model identity, judge noise, and limited
   sample sizes. None can be repaired by increasing decimal precision.
6. Dry runs perform no model calls and write no experiment outputs or worktrees. Invalid arguments
   fail before starting the experiment.

## Validation

`cargo test` exercises the parsers, live subprocess protocol through controlled harness fixtures,
worktree isolation, rubric reuse, commit capture, failed execution, judging, simulation, forks,
attribution, and record serialization. Fixtures verify orchestration and error handling; they do
not validate task-success judgments or research-level effect estimates. The
[validation report](validation.md) records live provider coverage and its limits;
`scripts/smoke-claude.py` reproduces the Claude workflow and retains local artifacts.

Unresolved research extensions include task-specific executable outcome oracles, full environment
snapshots, paired effect estimation over many tasks, and calibration against human-rated outcomes.
These are outside the implemented session-comparison contract, rather than implied capabilities.
