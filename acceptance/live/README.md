# Live acceptance suite

This suite runs Casimir against real, signed-in agents to check it works end to end. **It uses
up your provider subscription quota.**

## The tasks

`tasks.json` has ten maintained tasks: multi-file bug fixes, refactors, writing tests, a local
dependency migration and follow-up corrections.

`scripts/acceptance.py`, for each task:

1. creates the source repository;
2. freezes an external checker for it;
3. runs both supported agents twice;
4. checks that the source checkout wasn't touched and that the outcome was reported correctly;
5. replays across agents in both directions.

A model failing a task is recorded and allowed. A failure in Casimir's own orchestration fails
the gate.

## Running it

Set up the pinned agent versions and sign in to each of them on a protected machine first. You
have to pass `--allow-subscription-usage` to start the suite.

The driver gives Claude `acceptEdits` and Codex `workspace-write` inside its throwaway task
repositories. Normal Casimir runs keep your agent's permission settings, and
`--allow-unrestricted` is a separate opt-in for the test driver.

If you only have one subscription, use `--harness claude-code` or `--harness codex` to collect
partial evidence. A subset run is labelled `partial` and can't satisfy the full release gate.

Keep the output private and only upload redacted summaries to public CI. Don't share a protected
self-hosted runner with untrusted pull-request jobs.

## Native workflows

```sh
python scripts/native-workflows.py --casimir PATH --output PRIVATE_DIRECTORY \
    --claude-permission-mode acceptEdits --allow-subscription-usage
```

This collects evidence for native replay, fork and recovery. It:

1. interrupts the second turn after the first one has been saved;
2. checks that an implicit retry is refused;
3. retries explicitly in a fresh attempt;
4. checks the first turn wasn't sent again;
5. checks that resuming a finished run does nothing.

`--harness claude-code` or `--harness codex` is handy when validating a single version, but a
single-agent result can't satisfy a gate that needs both. Native continuation is only enabled
once the checked-in compatibility manifest lists the exact agent version **and platform**.

`--claude-permission-mode acceptEdits` lets Claude edit files in the throwaway repository while
keeping its other permission checks. By default your own configuration is kept, and unrestricted
mode is a separate opt-in.

## What each platform needs

Linux needs all 40 task attempts. macOS and Windows also need recorded, authenticated
checkpoint, fork and interrupted-resume workflows. Without a compatible checkpoint those
workflows are blocked, and fixtures can't certify a native transcript format. The driver
reports these workflow requirements separately, so a basic replay pass never counts as a
complete platform pass.

## Results so far

The 2026-09-23 Linux run passed all 40 attempts and both replay directions. Its
[hash-only receipt](evidence/linux-subscription-2026-09-23.json) is integration evidence, not
certification of a release commit: it notes that the binary was built before the repository
`HEAD` was committed. The full run needs repeating from a clean release commit before 1.0.
