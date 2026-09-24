# 1.0 release ledger

**Status:** in progress. This is not a 1.0 certification.

This page tracks what has to be true before Casimir is tagged 1.0, and where each item stands.

## What 1.0 promises

Casimir 1.0 is a local command-line tool for developers and researchers working in repositories
they trust. Claude Code and Codex are supported on macOS, Linux and native Windows. Copilot and
Gemini are experimental. Results are diagnostics validated against a recorded corpus; they are
never proof of cause.

## Engineering work

Implemented:

- Subprocess lifetime and bounded output capture, using process groups on Unix and Job Objects
  on Windows.
- Configurable timeouts: 900 seconds per agent turn and 300 seconds per judge or simulator
  call by default.
- Atomic metadata writes, exclusive run locks, raw streaming logs and an explicit recovery
  journal.
- `resume` needs an explicit retry for a turn that may already have run, and each new attempt
  restores from a checkpoint.
- Agent permission settings are kept by default. Bypassing them needs `--allow-unrestricted`,
  including for flags passed after `--`.
- Signed-in Claude Code and Codex CLIs are the default way to reach models, for the agents
  themselves and for the simulator and judge. The optional Anthropic API backend runs
  in-process and never puts credentials on a `curl` command line.
- `doctor --json` checks versions and login status without calling a model.
- Content-addressed checkpoints of the repository, index and conversation, with Git bundles and
  staged objects kept, so a fresh worktree can be restored and verified even after the source is
  deleted.
- Frozen executable checks, with execution, checks, judge and overall outcome reported
  separately. Attribution requires matching rubrics and models, and keeps judge failures and
  contradictions.
- A runner for predictions on the frozen evaluation corpus, and offline scoring against
  independent human review. Revision 2 of the corpus includes executable check source, captured
  results and explicit final snapshots.
- Claude-based helper calls turn off ambient customizations and run in temporary directories.
- Redacted sharing exports, cleanup previews based on ownership, and optional checkpoint
  reclamation that respects references from other runs.
- Compiled protocol fixtures, and deterministic CI on all three operating systems and on
  Rust 1.85.

Each of these has to be confirmed against real test artifacts at the release commit. A
configured workflow, or a passing fixture, doesn't show that the real authenticated provider
workflow works.

## Acceptance gates

| Gate | What's needed | Where it stands |
|---|---|---|
| Reliability | Full deterministic suite on Linux, macOS and Windows, with no orphaned processes, lost records, duplicated turns or edits to the source checkout | Runs are recorded in GitHub Actions. The evidence bundle needs a passing run for the exact release commit |
| Live Linux | 10 maintained multi-file tasks × 2 agents × 2 replicates, plus cross-agent replay in both directions | 40 real attempts passed their checks, in both directions, along with both native checkpoint and recovery workflows. Needs repeating from the clean release commit |
| Live macOS and Windows | Authenticated replay, checkpoint fork and interrupted resume | Waiting for native authenticated machines |
| Evaluation | Frozen 40-pair corpus, two independent reviewers and adjudication; at least 90% agreement on decisive cases; abstentions and false positives reported | Waiting for human review and calibration |
| Simulator and attribution | Reviewed simulator cases, and seeded recoverable and unrecoverable checkpoint cases | Waiting for review |
| Pilot users | Three independent users install, run doctor, run an experiment, interpret it, recover and clean up on their own repositories | Waiting for pilot users; see [acceptance/pilots](../acceptance/pilots/README.md) |
| Distribution | Four native archives, checksums, provenance and a clean install check | Archive workflow and protected evidence staging are done; release candidate build pending |

## Release evidence

`scripts/release-gates.py` reads the redacted JSON artifacts (`schemaVersion: 1`) listed in the
evidence manifest. Each artifact needs its own path and SHA-256. A "passed" field or a matching
hash isn't enough by itself: the script also checks the artifact's kind, commit, platform, run ID
and required results.

The publishing workflow also runs the gates with `--verify-github`. That authenticates the CI and
acceptance run and job IDs against GitHub, and compares each acceptance receipt with the exact
artifact that workflow uploaded. Running the script locally without `--verify-github` is only a
structural preflight. It doesn't authorize a release.

### Acceptance receipts

The authenticated acceptance workflow keeps the raw reports and transcripts on its protected
runner. Once its 40 task attempts and both native workflow checks pass, it uploads a small
`authenticated-PLATFORM` receipt. The receipt lists task IDs, agents, replicate numbers,
orchestration outcomes and native gate status. It contains no paths, prompts, transcripts,
credentials or raw model output.

Download the three receipts into the protected release-evidence directory. Give each its own
manifest record with `status`, `artifact` and `sha256`, and leave the originating run ID as it
is. The gate compares the receipt's bytes with the workflow's upload, so a locally edited receipt
is rejected.

### Artifact kinds

- **`reliability`:** the release commit, platform, CI push run ID, and the successful test job
  IDs for both Rust 1.85 and stable.
- **`evaluation`:** the actual calibration report, two distinct human reviewer IDs, the
  adjudication and the corpus hash.
- **`simulator`** and **`attribution`:** reviewed case counts and no unresolved failures.
- **`pilot`:** three separate artifacts, each with all six journey steps passed.

Human review and pilot attestations are a trust boundary held by the maintainer. Software can't
check that people actually did them, so never manufacture these records to clear a gate.

The 1.0 candidate artifact has to point to a published `v1.0.0-rc.N` release. The gate
downloads all four native archives and checks their checksums and install receipts, the signed
build provenance, and each archive's embedded target, commit, workflow run ID and executable
format. It also resolves the Git tag to confirm the candidate commit.

## Current state

The version in `Cargo.toml` is `1.0.0-rc.1`, and packaging requires the requested version to
match it. Every new commit needs fresh CI reliability evidence from that commit. Before a 1.0
tag we still need native authenticated runs, reviewed calibration, pilots and a published
release candidate.

Don't mark a real transcript format as checkpoint-compatible in
`compatibility/harnesses.json` until the matching live acceptance evidence is recorded there.
Fixture-only entries aren't provider certifications. Never tag 1.0 until every gate has
evidence tied to the release commit.

Authenticated Codex and Claude Code subscriptions are available for Linux testing. Still
missing: native macOS and Windows acceptance machines, independent reviewers and an
adjudicator, and three pilot users.

## Reliability coverage

The reliability tests cover authenticated HTTP failures and rate limits, limits on response and
stream size, malformed and truncated JSON, concurrent locks and metadata readers, checkpoint
corruption, retrying an interrupted turn without repeating completed ones, cleaning up child
processes, and, on Unix, signal cancellation and write failures injected with a file-size
limit.

The injected write failures exercise storage error handling. They don't prove how every
filesystem behaves when the disk is actually full. Native authenticated acceptance and human
acceptance are still open.
