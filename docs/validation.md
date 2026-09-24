# Validation

Last updated 2026-09-23.

This page records what has been tested and how. These are integration checks: they show that
Casimir drives the agents correctly and reports what happened. They say nothing about model
quality, and the sample sizes are far too small to support research conclusions.

## Automated tests

The suite has 54 CLI integration tests, 25 reliability tests and 5 library/HTTP tests. Between
them they cover:

- all four log parsers and the subprocess protocols
- resume, worktree isolation, recorded commits and untracked edits
- reusing a saved reference, and forks
- simulator decisions, and AB/BA judging with recorded tool evidence
- control groups, attribution, record envelopes, and exporting and scoring blinded pairs

Some specific behaviours they pin down:

- Empty, truncated or failed agent output never counts as a completed turn. A Gemini error
  result is kept even when there's no separate error event.
- Failed simulation and failed attribution still save a diagnostic report, and exit with a
  nonzero status.

The tests, strict Clippy (`-D warnings`) and the release build all pass on stable Rust. The
tests also pass on Rust 1.85. CI repeats the stable and 1.85 tests and the release and Clippy
checks.

## Live testing with Claude Code

These ran on Claude Code 2.1.280 (reporting model `claude-sonnet-5`), using an existing OAuth
login from the environment.

| Workflow | Result |
|---|---|
| Two-turn replay | Created `hello\n`, resumed the same session, appended `world\n` |
| Native transcript fork | Kept turn 1, changed turn 2, produced `hello\nforked\n`; source left unchanged |
| Committed workspace fork | Restored the commit made in turn 1; turn 2 checked the file existed before appending |
| Reference snapshot | Reused the saved full patch, including the uncommitted second line |
| Matched controls | Separate worktrees; matching model; expected final files; complete AB/BA judging. One control skipped the verification the user asked for and was correctly failed against its rubric |
| Simulator and intent coverage | The real simulator handled the follow-up; correct final file, passing judge, coverage report saved |
| Attribution | Resampled turn 2; correctly declined to name a point of commitment because the original had already succeeded |

### Bugs these runs found

**Lost OAuth token.** Authentication first failed because Casimir was removing
`CLAUDE_CODE_OAUTH_TOKEN` along with the variables Claude Code uses to detect nesting. It now
removes only the nesting markers and keeps credentials and provider settings. The
[Claude Code environment variable reference](https://code.claude.com/docs/en/env-vars) lists
these separately.

**Over-strict draft rubric.** The committed-task check turned up a draft rubric that required
the agent to *describe* its verification, which the user never asked for. Rubric drafting now
distinguishes doing verification from reporting it, and the judge now sees a bounded sample of
tool inputs and outputs, including evidence of early commits and final verification. Draft
rubrics are still subordinate to what the user actually asked for, and should still be reviewed.
When the previously rejected run was judged again with the tool evidence, it scored 9/10 with no
invalidity findings. A different control really had skipped the requested verification, and was
correctly flagged.

The smoke test checks that outcomes like these are reported consistently. It doesn't require
every model attempt to succeed.

### Reproducing it

`scripts/smoke-claude.py` reruns this workflow and checks the actual contents of the files it
produces. It uses one replicate per group to keep it small, which isn't enough to measure
differences between models. Forks now need a verified Casimir checkpoint and a matching
version/platform entry in the compatibility manifest.

### Subscription runs

After switching to subscription logins, the following was checked with Claude Code 2.1.280 on
Linux:

- Replay, checkpoint fork and explicit recovery of an interrupted turn all passed their
  executable checks, and the source checkout was left unchanged.
- With an invalid `ANTHROPIC_API_KEY` set in the parent shell, a real judge call still picked
  `claude-cli`, completed both AB and BA calls, and recorded usage. That's one candidate pair,
  so it's an integration check, not a calibrated evaluation.

The full Linux subscription run did 40 attempts: ten maintained tasks, both providers, two
replicates each. All 40 executions completed and all 40 passed their executable checks. The
source repositories were unchanged, and cross-agent replay passed in both directions. The
[evidence receipt](../acceptance/live/evidence/linux-subscription-2026-09-23.json) contains
only hashes. It records the repository `HEAD`, but explicitly doesn't certify a release commit,
because the binary was built from the working tree before that `HEAD` was committed. The run
needs repeating from a clean release commit.

## Live testing with Codex

A ChatGPT subscription with the official Codex CLI 0.156.1 passed, on Linux:

- replay
- checkpoint fork
- explicit recovery of an interrupted turn
- resuming an already completed run (a no-op, as expected)

The source checkout was unchanged and executable checks passed in every completed branch.

The test machine had unusual ambient Linux capabilities, so the test process dropped them
before starting Codex. Codex's own workspace sandbox stayed on. This version and platform is the
only Codex combination with a checkpoint compatibility entry. The private artifacts are
summarized by hash in
[`compatibility/evidence/codex-0.156.1-linux.json`](../compatibility/evidence/codex-0.156.1-linux.json).

Codex subscription calls also completed both orders of a judge check and a two-turn simulator
check, where the simulator kept the follow-up verbatim. Again, these check the plumbing; they
don't calibrate the judge or simulator against humans.

## Known gaps

- **Codex versions and platforms.** Codex 0.155.1 hasn't been validated for checkpoint forks.
  0.156.1 has Linux evidence only. macOS and Windows still need their own testing.
- **Copilot and Gemini.** Neither CLI was installed on the test machine. Their parsers and
  two-turn run/resume workflows, including completion and failure handling, are covered by
  fixtures based on the
  [Copilot CLI reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
  and the [Gemini headless reference](https://geminicli.com/docs/cli/headless/). They haven't
  been tested against the real services.
- **Simulator.** In the live test the simulator kept the follow-up verbatim, which was the right
  call for such a small task. Adapting messages, skipped-turn alignment, malformed replies,
  retries and blinded pair scoring are covered by fixtures, not by human calibration.
- **Direct API backend.** The `--llm api` backend hasn't been exercised live.
- **Scope.** Your own executable checks give task-specific evidence. Checkpoints restore the
  repository and the conversation, but not external services, other files or process memory.
  See [checkpoints](checkpoints.md) and the [research notes](research.md).
