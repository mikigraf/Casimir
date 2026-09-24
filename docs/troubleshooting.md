# Troubleshooting

Start with:

```sh
casimir doctor --json
```

It checks your agent CLIs, logins and storage without making any model calls.

Casimir never resends a prompt on its own after an authentication error, rate limit or timeout,
because the prompt may already have run. Fix the cause, then retry deliberately.

## Login problems

For subscription runs, look for `subscriptionReady: true` next to the agent you want to use. If
it isn't there, sign in again:

```sh
claude auth login
codex login
codex login --device-auth   # on a headless machine, if your account supports it
```

An "unknown" login state doesn't mean you're signed in. Login status commands can't tell whether
a token has expired or been revoked.

Logging in with an API key is reported separately and doesn't count as a subscription. API keys
are removed from the agent CLIs' environment. If you want to call the Anthropic API directly,
use `--llm api`.

## Codex says `runtimeReady: false` on Linux

The Codex sandbox self-test failed before any model call. Install a working `bubblewrap`, and
check whether your container gives processes unusual ambient Linux capabilities. The sandbox has
to work with the permissions of whatever process is running Casimir. Don't turn the sandbox off
to get the check to pass.

If you see `bwrap: Unexpected capabilities but not setuid` in a container with inherited
ambient capabilities, run Casimir from a shell that drops them:

```sh
setpriv --bounding-set=-all --inh-caps=-all --ambient-caps=-all casimir doctor --json
```

Once the sandbox check passes, use the same prefix for your experiment. This only changes the
process's Linux capabilities. It doesn't bypass the Codex sandbox or change the agent's
permission settings.

## "run is locked by another Casimir process"

Another process is using the run. Wait for it to finish or stop it. Don't delete the lock file
while that process is still running; the OS releases the lock when the process exits.

## A turn timed out

Look at these files in the run directory:

- `turns/N/stdout.log`
- `turns/N/stderr.log`
- `session.json`
- `recovery.json`

If the task just needs more time, raise `--turn-timeout` (agent turns) or `--llm-timeout` (judge
and simulator) next time. To continue the interrupted run, use
`casimir resume RUN --retry-interrupted`. Turns that already finished aren't repeated.

## Forks are refused as incompatible

You'll see "native transcript format/version has not passed checkpoint compatibility
validation" or "installed harness version does not match checkpoint compatibility version".

The agent's transcript format, or its installed version, hasn't passed acceptance testing yet.
You can still inspect the session and do a full rerun; you just can't fork it. Don't substitute
a commit guessed from timestamps. See [checkpoints](checkpoints.md) and
`compatibility/harnesses.json`.

## Disk full or permission denied

These are infrastructure errors, not task failures. Keep whatever was already written. Preview
what cleanup would remove with `casimir cleanup RUN` before adding `--apply`. Checkpoint data
can be shared between runs, so per-run cleanup keeps it.

## Windows

Git and the agent executables need to be on your `PATH`. Codex has a native Windows sandbox;
Claude Code currently doesn't. Neither permission modes nor worktrees isolate the agent from the
rest of your system.
