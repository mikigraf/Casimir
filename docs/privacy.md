# Privacy and sharing

Casimir stores a lot of sensitive material on your machine: prompts, agent transcripts,
workspace checkpoints and model responses. Any of these can contain credentials, ignored config
files, proprietary code or personal data. Treat everything under `~/.casimir` as private.

## Local storage

On Unix, storage directories are created with mode `0700` and new metadata files with `0600`.
On Windows, private storage and metadata get an ACL that only allows the owner and SYSTEM
([Microsoft reference](https://learn.microsoft.com/en-us/windows/win32/secauthz/security-information)).

This doesn't protect anything from an agent running as the same user, which can read these
files like any other.

## Credentials

The agent CLIs use your saved subscription login or subscription OAuth tokens from the
environment. Casimir removes API keys and custom API endpoint overrides from their environment,
so a key in your shell can't quietly be used for metered API calls.

The optional direct Anthropic backend (`--llm api`) uses an API key you supply explicitly, and
makes HTTPS requests in-process with redirects turned off.

`casimir doctor` reports login status and method, never the credentials themselves, and makes no
model calls. When an agent subprocess fails, the error you see says what kind of failure it was
(authentication, rate limit, sandbox and so on) without copying the provider's stderr. The full
stderr is kept in the private run logs.

## Judge, simulator and brief helpers

When these run through Claude Code, they use a temporary working directory and `--safe-mode`,
which turns off your project and user customizations but keeps the login. Their system prompts
are written to private files rather than passed on the command line.

When they run through Codex, they use `codex exec` in a temporary directory with a read-only
sandbox, ignore your user config and rules, and don't save a rollout.

None of this changes the permissions or config of the agent you're actually testing.

## Sharing results

To share a run, make a redacted export:

```sh
casimir export RUN --share -o shared.json   # or shared.md
```

A sharing export:

- redacts known credential patterns, sensitive structured keys and any credential values that
  are set in your current environment;
- carries an explicit marker saying it was redacted;
- leaves out checkpoint references and native log paths (JSON exports).

Blinded pair exports for human review are also redacted and include a `sharing.json`. Keep the
answer key to yourself, or you'll unblind the reviewers.

Redaction is best effort. Not every secret can be recognized, so read the file before you share
it. Exports made without `--share` contain everything.
