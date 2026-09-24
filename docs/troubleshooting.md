# Troubleshooting

Start with `casimir doctor --json`. An unknown login state is not proof of valid credentials;
login-status commands cannot guarantee a token has not expired or been revoked. Authenticate
using the provider CLI, then retry deliberately. Casimir does not automatically resend a
possibly executed prompt on authentication errors, rate limits or timeouts.

For subscription runs, look for `subscriptionReady: true` for the chosen harness. Use
`claude auth login` or `codex login`; on a headless Codex machine, `codex login --device-auth`
can complete a supported browser sign-in. An API-key login is reported separately and does
not satisfy the subscription gate. API-key environment variables are omitted from provider
CLI subprocesses; choose the explicit `--llm api` backend for direct Anthropic API calls.
On Linux, `runtimeReady: false` for Codex means a local sandbox self-test failed before any
model call. Install a working `bubblewrap` and check whether your container grants unusual
ambient Linux capabilities; the sandbox must work with the permissions of the process running
Casimir. Do not disable the sandbox to make a failing readiness check pass.
For `bwrap: Unexpected capabilities but not setuid` in a container with inherited ambient
capabilities, run Casimir from a privilege-dropped shell, for example
`setpriv --bounding-set=-all --inh-caps=-all --ambient-caps=-all casimir doctor --json`.
Use the same prefix for the experiment after the sandbox probe passes. This changes the
process's Linux capabilities; it does not bypass the Codex sandbox or alter harness
permission settings.

A locked run has another active owner. Wait for it to finish or interrupt that process. Do not
delete the lock file while a process is active; the OS releases the lock on process exit.

For a timeout, inspect `RUN/turns/N/stdout.log`, `stderr.log`, `session.json` and `recovery.json`.
Increase `--turn-timeout` or `--llm-timeout` for a subsequent experiment if appropriate. An
interrupted turn requires `resume --retry-interrupted`; completed turns are not repeated.

A checkpoint compatibility refusal means the native format or installed version has not passed
acceptance. Full reruns and inspection remain available. Do not substitute a timestamp-derived
commit for the missing checkpoint. See `compatibility/harnesses.json` and `docs/checkpoints.md`.

Disk exhaustion and permission failures are infrastructure errors, not task failures. Preserve
what was already written. Preview owned artifact removal with `casimir cleanup RUN` before
using `--apply`. Content-addressed checkpoint blobs may be shared by other runs and are retained
by per-run cleanup.

Native Windows requires Git and runnable harness executables on PATH. Codex offers a native
Windows sandbox; Claude Code currently has no native Windows OS sandbox. Permission modes and
worktrees do not provide universal isolation.
