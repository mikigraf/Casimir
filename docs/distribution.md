# Distribution and releases

## Installing a release

Release archives are built for macOS (Apple Silicon and Intel), Linux x64 and Windows x64.

1. Check the archive's SHA-256 checksum and its build provenance:

   ```sh
   gh attestation verify ARCHIVE --repo mikigraf/Casimir
   ```

2. Extract it somewhere you own and put `casimir` (or `casimir.exe`) on your `PATH`.
3. Run `casimir --version` and `casimir doctor --json`. Neither makes any paid model calls.
4. Sign in to the agent you want to use (`claude auth login` or `codex login`) and check that
   `casimir doctor --json` shows `subscriptionReady: true`.

You don't need an Anthropic API key. The pinned agent versions are listed in the
[compatibility manifest](../compatibility/harnesses.json).

Building from source with `cargo install --path . --locked` is still supported.

## How releases are made

This part is for maintainers.

The release workflow is triggered by hand. It runs the tests, builds the native binaries,
packages them with the docs and compatibility information, installs each archive into a clean
directory to check it works, writes SHA-256 checksums and asks GitHub for build provenance. The
result is a **draft** release, and the release environment should require a maintainer to
approve it.

### Release evidence

A release also needs evidence that the acceptance gates passed.

- Use `acceptance/release-evidence.json` as the template for a private evidence bundle.
- Upload the finished bundle with the protected `release-evidence.yml` workflow, and give its
  run ID to the release workflow.
- Point the release environment's `CASIMIR_RELEASE_EVIDENCE_DIR` variable at the reviewed
  bundle on the protected runner.
- The manifest and every JSON summary it references must say `redacted: true`. The staging
  script uploads only those summaries, never raw transcripts or checkpoints.
- The archive workflow checks that the evidence came from the right workflow, from a successful
  manual run, and matches the expected commit and artifact hashes.

Evidence has to name the release commit, so it's generated after that commit exists rather
than checked into it.

A release candidate needs passing deterministic results on all three platforms. Tagging 1.0
also needs the live, evaluation, simulator, attribution, pilot and release-candidate evidence.
`scripts/release-gates.py` fails if anything is missing. Never change a placeholder to "passed"
without the real evidence behind it.

Only run the authenticated acceptance workflow on protected infrastructure that has provider
access. Credentials must never be exposed to pull-request code or public logs.

The current version is a release candidate, not a certified 1.0.
