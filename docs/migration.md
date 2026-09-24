# Upgrading to 1.0

This page lists what changed on the way to 1.0 and what you may need to do about it.

## Old runs and sessions

Existing session and run JSON without a version number can still be read. New documents
include `schemaVersion: 1`.

Runs created before this version don't have a recovery journal, so `resume` won't work on them.
Old run directories also have no ownership record, so `cleanup` refuses to delete them. That's
intentional.

## Judge and simulator backend

`--llm auto` now uses a signed-in Claude Code subscription, or a signed-in Codex subscription if
Claude Code isn't available. It no longer picks the Anthropic API just because an API key is set
in your shell. If you want the old behaviour, pass `--llm api`.

`--llm codex-cli` can now be used for judging and simulation. If you don't choose a model, it
uses the Codex CLI's default, so set `--llm-model` when you need comparisons to stay stable.

## "Finished" no longer means "passed"

A process that exits cleanly doesn't count as a successful task any more. Reports now show four
separate things: execution, your executable checks, the judge's assessment and the overall
outcome.

You'll get *inconclusive* when there's no evaluation evidence, when citations can't be verified,
when the two judge orderings contradict each other, or when there was an infrastructure
problem. A failed required check can never produce an overall pass.

## Permissions

Casimir now keeps your agent's existing permission settings. If you really do want unrestricted
execution, pass `--allow-unrestricted` along with the agent's bypass setting. This applies to
bypass flags you pass after `--` too. Worktrees are not sandboxes.

## Workspaces

`--workspace DIR` now treats DIR as the source Git repository and creates a separate worktree
from it. It no longer runs the agent directly inside DIR. If you do want that, use
`--workspace same`.

Checkpoints need a Git repository, and the run output directory must be empty.

## Forks

You can still inspect and fully rerun old imported sessions. Forking one now needs a verified
Casimir checkpoint and a compatible transcript version. Commit timestamps are no longer accepted
as a stand-in for what the workspace looked like. Forks made the old, heuristic way aren't
upgraded.

## Interrupted runs

Run `casimir doctor --json` before you start, and `casimir resume RUN` after an interruption.

If a prompt might already have run, look at the private raw logs before you use
`casimir resume RUN --retry-interrupted`. That starts a new attempt, and it can't undo anything
the first attempt did outside the repository.

## Cleanup

Cleanup now previews first. Run `casimir cleanup RUN` to see what it would remove, then
`casimir cleanup RUN --apply`.

## Comparisons and attribution

Standalone comparisons keep any required-check failures that were recorded.

Attribution now needs more evidence before it will draw a conclusion:

- the fingerprint of the original rubric (pass the saved `--brief`);
- matching observed models;
- conclusive evidence from the resampled continuations;
- matching fingerprints for the judge instructions, backend, token budget, repeat count and
  helper context version.

If any of these are missing or different, the conclusion is withheld. If you change the judge,
you have to re-evaluate the original under the same criteria. Older evaluations without this
evidence can still be inspected but can't be used for attribution.

## Judge output

The judge's `uncertainty` field records missing or contradictory evidence that matters to a
requirement the user asked for, and it makes the assessment inconclusive. The separate
`limitations` field holds informational caveats about things the user didn't ask for, and
doesn't add requirements. Neither can override a failed executable check.

If a run was interrupted and some turn costs weren't measured, the total cost is reported as
unknown.
