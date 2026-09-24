# Pilot users

Before 1.0, three people who didn't work on Casimir need to try it out on their own. This page
is the protocol for running those pilots.

## Setup

Each pilot uses their own repository, their own machine, the same release candidate, and the
provider permissions they normally use. Record their platform, the candidate commit, the agent
and its version, and a pseudonymous user ID. An agent or a fixture can't stand in for a pilot.

Give them the installation instructions and the CLI docs, but don't walk them through the
commands.

## The journey

Ask each pilot to go through these steps and explain the report in their own words.

1. **Install.** Verify the native archive's checksum and provenance, install it and run
   `casimir --version`. Installing from a pinned commit with
   `cargo install --git https://github.com/mikigraf/Casimir --rev COMMIT --locked` is fine too;
   just record which route they used.
2. **Doctor.** Run `casimir doctor --json`, find the installed agent and its login state, and
   explain what permissions it has and why a worktree doesn't fully isolate it.
3. **Experiment.** Pick an existing session with `casimir list`. Write a JSON executable check
   for a real requirement in the repository. Preview the experiment with `rerun --dry-run`,
   then run it with `--checks FILE` and a fresh output directory. Confirm the original checkout
   is unchanged. Record the required-check results even if the model fails the task.
4. **Interpret.** Explain the execution status, the check outcome, the judge's assessment (if
   they asked for one), any unavailable costs and any inconclusive evidence. Ask them: does a
   process that finished successfully prove the task succeeded?
5. **Recover.** Interrupt a multi-turn run after its first turn and use `resume`. Explain why an
   ambiguous prompt is refused. Retry it explicitly, look at the new attempt, and confirm the
   first turn wasn't sent again. The platform and agent version must already have validated
   checkpoint compatibility. If it's refused as an unvalidated format, that's a release
   blocker, not a pilot pass.
6. **Clean up.** Preview `cleanup RUN --checkpoints`, apply it, and check that the source
   repository is intact. Explain why shared checkpoints and the Git restore cache are kept.

## Recording results

Mark each step passed, failed or blocked, with a short note. Keep screenshots and recordings
private, and only publish reviewed, redacted summaries. Each summary needs:

- `schemaVersion: 1`
- `redacted: true`
- `userId`, `commit` and `platform`
- a `steps` object with the six keys `install`, `doctor`, `experiment`, `interpret`,
  `recover` and `cleanup`
- any unresolved issues

Every data loss, wrong result or onboarding blocker has to be fixed, and that part of the
journey repeated. A model failing the task is fine as long as Casimir orchestrated and reported
it correctly.

Add each finished summary and its hash to the release evidence bundle. Writing this protocol
down doesn't mean any pilot has happened yet.
