# Release notes

## Unreleased: 1.0 candidate implementation

Adds bounded subprocess supervision, atomic recovery journals and run locks, checkpoint-backed
fork requirements, explicit unrestricted-execution consent, in-process Anthropic HTTPS,
`doctor --json`, `resume`, frozen executable checks, redacted sharing exports and owned cleanup.
Native protocol fixtures replace shell-dependent test runners. CI targets Linux, macOS and Windows.

Task outcomes now distinguish clean execution from success. Missing/contradictory evidence is
inconclusive; failed required checks cannot become passes. Imported historical sessions remain
readable and support full reruns, but cannot be forked without compatible checkpoints.

This is not a 1.0 certification. Native authenticated acceptance, reviewed evaluation calibration,
pilot-user evidence and release artifact verification remain mandatory. See
[the release ledger](docs/production-readiness.md) and [migration guidance](docs/migration.md).
