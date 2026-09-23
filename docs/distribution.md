# Distribution and release procedure

`cargo install --path . --locked` remains supported. Release archives target macOS ARM64 and
x64, Linux x64, and Windows x64. The manually triggered native release workflow runs tests,
builds the native binary, packages documentation and compatibility information, verifies an
extracted installation, writes SHA-256 checksums, and requests GitHub build provenance.

The workflow creates a **draft** release for review. The release environment should require a
maintainer reviewer. Run the authenticated acceptance workflow only on protected infrastructure
with provider access; never expose credentials to pull-request code or public logs.

Use `acceptance/release-evidence.json` as the template for a private evidence bundle. Upload
the completed bundle using the protected `release-evidence.yml` workflow and provide its run ID
to the release workflow. Configure the release environment's `CASIMIR_RELEASE_EVIDENCE_DIR`
variable to the reviewed bundle on the protected runner. Its manifest and referenced JSON
summaries must explicitly attest `redacted: true`; the staging script uploads only referenced
summaries, never the raw transcript/checkpoint directories. The archive workflow verifies the
evidence workflow identity, successful manual event, commit and artifact hashes.
Evidence must name the release commit; it is generated
after that commit, avoiding a self-referential checked-in commit hash. RC creation requires all three deterministic platform results. Tagging 1.0
requires the live, reviewed evaluation, simulator, attribution, pilot and RC evidence as well.
`scripts/release-gates.py` fails closed on missing evidence. Do not edit placeholders to passed
without corresponding real evidence. The current version is not a 1.0 release certification.

Before installing, verify the archive's SHA-256 checksum and provenance with
`gh attestation verify ARCHIVE --repo mikigraf/Casimir`. Extract into a user-owned directory,
place `casimir` (or `casimir.exe`) on PATH, then run `casimir --version` and `casimir doctor --json`.
These verification commands make no paid model calls. Authentication must be configured using
the supported provider CLI. See the compatibility manifest for pinned versions.
