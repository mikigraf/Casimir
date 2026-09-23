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
- Anthropic HTTPS transport executes in-process; no credential-bearing curl arguments.
- `doctor --json` performs version/login-status probes, without model calls.
- Content-addressed repository/index/conversation checkpoints, retained Git bundles and staged objects; verified fresh-worktree restore even after source deletion.
- Frozen executable checks and separate execution, check, judge, and overall outcomes; rubric/model matching for attribution and retained judge-failure/contradiction findings.
- Frozen-corpus prediction runner and offline scoring against independent human review.
- Redacted sharing exports, ownership-based cleanup previews, and optional reference-aware checkpoint reclamation.
- Compiled protocol fixtures; deterministic CI configured for all three operating systems and Rust 1.85.

These items must be checked against actual test artifacts at the release commit. A configured
workflow or a fixture success is not evidence that an authenticated provider workflow passes.

## Mandatory acceptance evidence

| Gate | Required evidence | Current disposition |
|---|---|---|
| Reliability | Full deterministic suite on Linux/macOS/Windows; no orphan processes, lost records, duplicate completed turns, or source checkout edits | 8f9351a passed all six OS/toolchain jobs and lint/release build in two CI runs; final changes require a fresh passing CI run |
| Live Linux | 10 maintained multi-file tasks × 2 harnesses × 2 replicates; both cross-harness replay directions | Linux Claude 2.1.280 replay/fork/interrupted resume validated; full 40-attempt suite blocked on Codex/API access |
| Live macOS/Windows | Authenticated replay, checkpoint fork, interrupted resume | Pending native authenticated environments |
| Evaluation | Frozen 40-pair corpus, two independent reviewers, adjudication; ≥90% decisive agreement; abstentions and false positives reported | Human review and calibration pending |
| Simulator/attribution | Reviewed simulator cases and seeded recoverable/unrecoverable checkpoint cases | Acceptance review pending |
| Pilot users | Three independent users complete installation, doctor, experiment, interpretation, recovery, cleanup on their own repositories | Pilot users pending; see acceptance/pilots/README.md |
| Distribution | Four native archives, checksums, provenance, clean installation verification | Native archive workflow and protected evidence staging implemented; release candidate build pending |

Do not mark real transcript formats checkpoint-compatible before recording the corresponding
live acceptance evidence in `compatibility/harnesses.json`. Fixture-only entries are not provider
certifications. Never tag 1.0 before every gate has evidence tied to the release commit.

External prerequisites: authenticated Codex and Anthropic API test access, native macOS and
Windows acceptance machines, independent reviewers/adjudicator, and three pilot users.

Reliability coverage includes authenticated HTTP failure responses and rate limits, bounded
response/stream sizes, malformed and truncated JSON, concurrent locks and metadata readers,
checkpoint corruption, interrupted turn retry without repeating completed turns, process
descendant cleanup, and Unix signal cancellation and file-size-limit write failure injection.
The write-failure injection exercises storage errors; it does not certify every physical
full-disk behavior on every filesystem. Native authenticated and human acceptance remain open.
