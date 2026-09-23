# Migration toward 1.0

Existing unversioned session/run JSON remains readable. New session, report, checkpoint,
recovery, check, doctor, sharing, brief, pair, matrix, attribution, and ownership documents carry `schemaVersion: 1`.
Legacy runs have no durable recovery commit record and cannot safely use `resume`.

A successful subprocess no longer means task success. Reports distinguish execution,
executable checks, judge assessment, and overall task outcome. Missing evaluation evidence,
unverified citations, contradictory orderings, and infrastructure problems yield an
inconclusive outcome. A failed required executable check can never produce an overall pass.

Permission defaults now preserve the installed harness configuration. If unrestricted
execution is intentional, pass `--allow-unrestricted` as well as the requested harness bypass
setting. This consent is also required for bypass flags after `--`. Worktrees are not sandboxes.

`--workspace DIR` now treats DIR as a source Git repository and creates a separate worktree.
It does not run the agent directly in DIR. `--workspace same` remains an explicit in-place choice.
A Git repository is required to capture checkpoints. Run output must be an empty directory.

Historical imported conversations may be inspected and fully rerun. A fork requires a verified
Casimir checkpoint and a compatible native transcript version. Commit timestamps are no longer
accepted as proof of historical workspace contents. Existing heuristic forks are not upgraded.

Run `casimir doctor --json` before execution. Use `casimir resume RUN` after an interruption.
If a prompt might have executed, inspect the private raw logs before choosing
`casimir resume RUN --retry-interrupted`; this creates a new attempt and cannot undo external effects.

Cleanup is preview-first: `casimir cleanup RUN`, then `casimir cleanup RUN --apply`.
Legacy directories without ownership records are intentionally refused.

Standalone comparisons carry forward recorded required-check failures. Attribution additionally
requires the original rubric fingerprint (supply the saved `--brief`), matching observed models,
and conclusive continuation evidence. Legacy evaluations without this evidence remain usable
for inspection but cannot establish attribution.
