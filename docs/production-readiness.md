# Casimir 1.0 release ledger

Status: implementation and acceptance in progress. **Not a 1.0 release certification.**

The contract is a local developer/research CLI for trusted repositories, with Claude Code
and Codex on macOS, Linux, and native Windows. Copilot and Gemini are experimental.
Results are validated diagnostics on a recorded corpus, never causal proof.

## Engineering verification

- Shared subprocess lifetime and bounded capture: Unix process groups; Windows Job Objects.
- Configurable 900-second harness turns and 300-second judge/simulator calls.
- Atomic metadata, exclusive run locks, raw streaming logs, explicit recovery journal.
- `resume` requires explicit retry for a potentially executed turn; new attempts restore checkpoints.
- Default permission preservation; bypass requires `--allow-unrestricted`, including passthrough.
- Authenticated Claude Code and Codex CLIs are the default model transports for harnesses,
  simulator and judge. The explicit legacy Anthropic API backend executes in-process; no
  credential-bearing curl arguments.
- `doctor --json` performs version/login-status probes, without model calls.
- Content-addressed repository/index/conversation checkpoints, retained Git bundles and staged objects; verified fresh-worktree restore even after source deletion.
- Frozen executable checks and separate execution, check, judge, and overall outcomes; rubric/model matching for attribution and retained judge-failure/contradiction findings.
- Frozen-corpus prediction runner and offline scoring against independent human review; revision 2 includes executable check source, captured results and explicit final snapshots.
- Claude helper calls disable ambient customizations and use temporary working directories.
- Redacted sharing exports, ownership-based cleanup previews, and optional reference-aware checkpoint reclamation.
- Compiled protocol fixtures; deterministic CI configured for all three operating systems and Rust 1.85.

These items must be checked against actual test artifacts at the release commit. A configured
workflow or a fixture success is not evidence that an authenticated provider workflow passes.

## Mandatory acceptance evidence

| Gate | Required evidence | Current disposition |
|---|---|---|
| Reliability | Full deterministic suite on Linux/macOS/Windows; no orphan processes, lost records, duplicate completed turns, or source checkout edits | Deterministic OS/toolchain runs are recorded in GitHub Actions; the release evidence bundle must include a passing run for its exact commit |
| Live Linux | 10 maintained multi-file tasks × 2 harnesses × 2 replicates; both cross-harness replay directions | 40 real attempts, all executable checks, both cross-harness directions, and both native checkpoint/recovery workflows passed on Linux; repeat from the clean release commit for certification |
| Live macOS/Windows | Authenticated replay, checkpoint fork, interrupted resume | Pending native authenticated environments |
| Evaluation | Frozen 40-pair corpus, two independent reviewers, adjudication; ≥90% decisive agreement; abstentions and false positives reported | Human review and calibration pending |
| Simulator/attribution | Reviewed simulator cases and seeded recoverable/unrecoverable checkpoint cases | Acceptance review pending |
| Pilot users | Three independent users complete installation, doctor, experiment, interpretation, recovery, cleanup on their own repositories | Pilot users pending; see acceptance/pilots/README.md |
| Distribution | Four native archives, checksums, provenance, clean installation verification | Native archive workflow and protected evidence staging implemented; release candidate build pending |

## Release evidence and provenance

`scripts/release-gates.py` reads `schemaVersion: 1` redacted JSON artifacts from the
evidence manifest. Every artifact must have its own path and SHA-256. A passing manifest
field or a matching hash alone is insufficient: artifact kind, commit, platform, run ID,
and required results are checked. The publishing workflow also runs
`--verify-github`, which authenticates CI and acceptance run/job IDs with GitHub and
compares each authenticated acceptance receipt with the exact artifact uploaded by
that workflow. Local validation without `--verify-github` is a structural preflight,
not a release authorization.

The authenticated acceptance workflow keeps raw reports and transcripts on its
protected runner. After its 40 task attempts and both native workflow checks pass,
it uploads a small `authenticated-PLATFORM` receipt. The receipt contains task IDs,
harnesses, replicate numbers, orchestration outcomes, and native gate status, but no
paths, prompts, transcripts, credentials, or raw model output. Download those three
receipts into the protected release-evidence directory, give each a distinct manifest
record with `status`, `artifact`, and `sha256`, and keep the originating run ID intact.
The release gate refuses a locally rewritten receipt because it compares its bytes
with the workflow upload.

Reliability artifacts use `kind: reliability`, the release commit, platform, CI push
run ID, and both Rust 1.85/stable successful test job IDs. A 1.0 bundle additionally
needs `kind: evaluation` with the actual calibration report, two distinct human
reviewer IDs, adjudication and corpus hash; `kind: simulator` and `kind: attribution`
with reviewed case counts and no unresolved failures; and three distinct
`kind: pilot` artifacts with six passed journey steps each. Human review and pilot
attestations remain a protected maintainer trust boundary; software cannot verify
that people actually performed them. Do not manufacture these records to clear a
gate. The 1.0 candidate artifact must refer to a published `v1.0.0-rc.N` release;
the gate downloads all four native archives, verifies their checksums and installation
receipts, and checks their signed build provenance.

The current candidate version in `Cargo.toml` is `1.0.0-rc.1`. Packaging requires
the requested version to match it. A new commit requires fresh CI reliability
evidence from that commit. Native authenticated runs, reviewed calibration, pilots,
and a published release candidate are still required before a 1.0 tag.

Do not mark real transcript formats checkpoint-compatible before recording the corresponding
live acceptance evidence in `compatibility/harnesses.json`. Fixture-only entries are not provider
certifications. Never tag 1.0 before every gate has evidence tied to the release commit.

Authenticated Codex and Claude Code subscription access is available for Linux validation.
Remaining external prerequisites include native macOS and Windows acceptance machines,
independent reviewers/adjudicator, and three pilot users.

Reliability coverage includes authenticated HTTP failure responses and rate limits, bounded
response/stream sizes, malformed and truncated JSON, concurrent locks and metadata readers,
checkpoint corruption, interrupted turn retry without repeating completed turns, process
descendant cleanup, and Unix signal cancellation and file-size-limit write failure injection.
The write-failure injection exercises storage errors; it does not certify every physical
full-disk behavior on every filesystem. Native authenticated and human acceptance remain open.
