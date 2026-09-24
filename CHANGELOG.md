# Changelog

## 1.0.0-rc.1 (unreleased)

The first release candidate for 1.0. It isn't a final release: live testing on macOS and
Windows, human review of the judge, pilot users and release artifact checks are still to do.
See the [release ledger](docs/production-readiness.md) for status and the
[migration guide](docs/migration.md) if you're upgrading.

### Added

- `casimir doctor --json` to check setup without making model calls.
- `casimir resume` to continue interrupted runs from a recovery journal.
- `casimir cleanup` to preview and remove a run's artifacts and worktrees.
- `casimir export --share` for redacted exports.
- `--checks` for your own pass/fail commands, run against the result.
- Checkpoints, so forks restore the recorded workspace and conversation.
- CI on Linux, macOS and Windows.

### Changed

- Reports now tell apart "the agent finished" from "the task succeeded". Missing or
  contradictory evidence is reported as inconclusive, and a failed required check can never
  count as a pass.
- The Anthropic API backend makes its HTTPS requests in-process instead of passing
  credentials to `curl`.
- `--llm auto` now picks a signed-in Claude Code or Codex subscription and no longer falls back
  to an API key found in the environment.
- Unrestricted agent execution now needs an explicit `--allow-unrestricted`.
- Subprocesses have time limits and bounded output; run state is written atomically and runs
  are locked while active.
- Tests use compiled protocol fixtures instead of shell scripts.

### Compatibility

- Sessions imported from before this version can still be viewed and fully rerun, but can't be
  forked without a compatible checkpoint.
