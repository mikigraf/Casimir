# Authenticated acceptance suite

The ten maintained tasks in `tasks.json` cover multi-file bug fixes, refactoring, tests,
a local dependency migration and follow-up corrections. `scripts/acceptance.py` creates each
source repository, freezes an external checker, runs both supported harnesses twice, verifies
source checkout immutability and outcome reporting, and exercises cross-harness replay in both
directions. Model task failures are recorded and allowed; orchestration failures fail the gate.

This suite costs provider usage. Invocation requires `--allow-paid`. Configure the pinned
provider versions and their own login state on a protected machine first. Permission defaults
are preserved; `--allow-unrestricted` is a separate explicit opt-in for test repositories.

Linux requires all 40 task attempts. Native macOS and Windows additionally require recorded
authenticated checkpoint, fork and interrupted-resume workflows. A missing compatible
checkpoint blocks those workflows; fixtures cannot certify the native transcript format.
The live driver reports these workflow requirements separately and does not turn a basic
replay pass into a complete platform acceptance pass.

Keep output private. Upload only redacted evidence summaries to public CI. A protected
self-hosted runner must not be shared with untrusted pull-request jobs.
