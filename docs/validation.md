# End-to-end validation

Validation date: 2026-09-23. These are software integration checks, not evidence of model quality
or statistical power for research conclusions.

## Automated checks

- 53 Rust tests cover four log parsers, subprocess protocols, resume, worktree isolation,
  recorded commits and untracked edits, saved-reference reuse, forks, simulator decisions,
  AB/BA judging with recorded tool evidence, controls, attribution, record envelopes, and blinded-pair export/scoring.
- Empty, truncated, and failed harness streams cannot count as completed turns. Gemini's
  error result is retained even without a separate error event. Failed simulation and attribution
  save diagnostic reports and return a nonzero CLI status.
- Stable Rust tests, strict Clippy (`-D warnings`), and the release build pass.
- Rust 1.85 tests pass. CI repeats stable/MSRV tests and release/Clippy checks.

## Live Claude Code checks

Claude Code 2.1.280, reporting model `claude-sonnet-5`, successfully ran these checks using its
existing OAuth environment credential:

| Workflow | Checked result |
|---|---|
| Two-turn replay | Created `hello\n`, resumed the same native session, appended `world\n` |
| Native transcript fork | Preserved turn 1, changed turn 2, produced `hello\nforked\n`; source remained unchanged |
| Committed workspace fork | Restored the commit made in turn 1; turn 2 verified the existing file before appending |
| Reference snapshot | Reused the saved full patch, including the uncommitted second line |
| Matched controls | Separate source worktrees; matching reported model; expected final files and complete AB/BA judges; a control that omitted requested verification correctly failed its rubric |
| Simulator and intent coverage | Follow-up processed by the real simulator; correct final file, passing judge, coverage report saved |
| Attribution | Resampled turn 2 successfully; withheld a point of commitment because the original already succeeded |

The original authentication failure was caused by Casimir removing `CLAUDE_CODE_OAUTH_TOKEN`
along with nesting markers. It now removes only the nesting markers and preserves credentials
and provider configuration. The [Claude environment-variable reference](https://code.claude.com/docs/en/env-vars)
documents these separate settings.

The committed-task smoke check also exposed an ungrounded draft-rubric requirement to narrate
verification. Rubric drafting now distinguishes performing verification from reporting it, and judges
receive bounded tool inputs/results, including early commit and final verification evidence. Draft
rubrics remain advisory to the actual user requests and should still be reviewed. Rejudging the
exact previously rejected artifact with tool evidence scored it 9/10 with no invalidity finding.
Another control actually omitted the requested post-edit verification and was correctly flagged;
the smoke test checks that such outcomes are reported consistently, rather than requiring every
model attempt to succeed.

`scripts/smoke-claude.py` reproduces the live workflow and checks actual artifact contents. It uses
one replicate per group to keep the integration check small; that is insufficient to estimate
model differences. Native transcript forks remain experimental and do not restore uncommitted
prefix changes automatically.

## Provider and evaluation limits

- Codex 0.155.1 is installed but is not authenticated here. Its parsing, rerun/resume protocol,
  token accumulation, native transcript preparation, and failures are fixture-tested; a successful
  provider-backed Codex run or fork has not been verified here.
- Copilot and Gemini executables are absent here. Their parsers and two-turn subprocess/resume
  workflows are fixture-tested, including completion and failure handling. Protocol checks use
  the [Copilot CLI reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
  and [Gemini headless reference](https://geminicli.com/docs/cli/headless/); live provider compatibility remains unverified.
- The live simulator kept the follow-up verbatim, as expected for this small task. Adaptation,
  skipped-turn alignment, malformed replies, retries, and blinded human-pair scoring are covered
  by fixtures, not human calibration. The direct Anthropic API backend was not exercised live.
- Executable file assertions validate this smoke task. Casimir does not yet provide general
  task-specific outcome oracles or a full environment checkpoint. See the
  [research audit](research.md) for the scope of its scientific claims.
