# Authenticated acceptance suite

The ten maintained tasks in `tasks.json` cover multi-file bug fixes, refactoring, tests,
a local dependency migration and follow-up corrections. `scripts/acceptance.py` creates each
source repository, freezes an external checker, runs both supported harnesses twice, verifies
source checkout immutability and outcome reporting, and exercises cross-harness replay in both
directions. Model task failures are recorded and allowed; orchestration failures fail the gate.

This suite consumes provider subscription usage. Invocation requires
`--allow-subscription-usage`. Configure the pinned provider versions and their own subscription
login state on a protected machine first. The driver explicitly grants Claude `acceptEdits` and
Codex `workspace-write` in its disposable task repositories. Ordinary Casimir runs preserve
provider permission settings; `--allow-unrestricted` is a separate opt-in for the test driver.

Linux requires all 40 task attempts. Native macOS and Windows additionally require recorded
authenticated checkpoint, fork and interrupted-resume workflows. A missing compatible
checkpoint blocks those workflows; fixtures cannot certify the native transcript format.
The live driver reports these workflow requirements separately and does not turn a basic
replay pass into a complete platform acceptance pass.
The 2026-09-23 Linux run passed all 40 attempts and both replay directions; its
[hash-only receipt](evidence/linux-subscription-2026-09-23.json) is integration evidence,
not a release-commit certification. It records that the local binary was built before its
repository HEAD was committed. Repeat the full run from a clean release commit before 1.0.
Use `--harness claude-code` or `--harness codex` to collect real partial provider evidence when
only one subscription login is available. A subset is labelled `partial` and cannot satisfy
the full release gate.

Keep output private. Upload only redacted evidence summaries to public CI. A protected
self-hosted runner must not be shared with untrusted pull-request jobs.

Run `python scripts/native-workflows.py --casimir PATH --output PRIVATE_DIRECTORY --claude-permission-mode acceptEdits --allow-subscription-usage`
for native replay/fork/recovery evidence. It interrupts the second turn after a durable first
turn, verifies that implicit retry is refused, explicitly retries in a fresh attempt, checks
that the first turn is not repeated, and verifies completed resume is a no-op. The optional
`--harness claude-code` or `--harness codex` is useful for version validation; a subset result
cannot satisfy a release gate requiring both. The checked-in compatibility manifest must
validate the exact harness version **and platform** before native continuation is enabled.

For a disposable Claude test repository, `--claude-permission-mode acceptEdits` explicitly
permits edits while retaining the provider's other permission checks. The default still
preserves the user's configuration; unrestricted mode remains a separate opt-in.
