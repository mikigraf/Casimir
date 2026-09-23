# Independent pilot acceptance

Recruit three people who did not implement Casimir. Each uses their own repository and machine,
the same release candidate, and their usual provider permissions. Record platform, candidate
commit, harness/version and a pseudonymous user ID. Do not replace a pilot with an agent or fixture.

Give each pilot the installation and CLI documentation, without coaching them through commands.
Ask them to complete this journey and explain the report in their own words:

1. Verify the native archive checksum/provenance, install it and run `casimir --version`.
   A pinned `cargo install --git https://github.com/mikigraf/Casimir --rev COMMIT --locked`
   is also supported; record which installation route was used.
2. Run `casimir doctor --json`, identify the installed harness and authentication state,
   and explain its permissions and the limits of worktree isolation.
3. Select an existing session using `casimir list`. Prepare a JSON executable check for a
   meaningful requirement in the repository. Preview an experiment with `rerun --dry-run`,
   then complete it using `--checks FILE` and a fresh output directory. Confirm the original
   checkout is unchanged. Record required-check results even when the model attempt fails.
4. Explain execution status, check outcome, judge assessment (if requested), unavailable
   costs and inconclusive evidence. Ask whether a successful process proves task success.
5. Interrupt a multi-turn run after its first turn. Use `resume`; explain why an ambiguous
   prompt is refused. Explicitly retry it, inspect the new attempt and confirm the completed
   first turn was not sent again. The platform/version must already have validated checkpoint
   compatibility; a refusal for an unvalidated format is a release blocker, not a pilot pass.
6. Preview `cleanup RUN --checkpoints`, apply it and verify that the source repository is
   intact. Explain shared checkpoint retention and the documented Git-cache retention.

Record each step as passed, failed or blocked, with a short observation. Preserve screenshots
or recordings privately and publish only reviewed, redacted summaries. Each summary must include
`schemaVersion: 1`, `redacted: true`, `userId`, `commit`, `platform`, the six `steps` keys
(`install`, `doctor`, `experiment`, `interpret`, `recover`, `cleanup`) and unresolved issues.

Every data-loss, incorrect-result or onboarding blocker must be fixed and the affected journey
repeated. Model task failure alone is acceptable when orchestration and reporting are correct.
Add each completed summary and its hash to the release-evidence bundle. No pilot has been
attested by this protocol's existence.
