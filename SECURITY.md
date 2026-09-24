# Security policy

## Reporting a vulnerability

Please report security issues privately through
[GitHub private vulnerability reporting](https://github.com/mikigraf/Casimir/security/advisories/new).
If that isn't available to you, open a public issue asking for a private contact, and leave out
any secrets or exploit details.

Things we especially want to hear about:

- credentials leaking into logs, reports or exports
- agent subprocesses that outlive Casimir or escape its supervision
- checkpoint restores that write somewhere they shouldn't
- cleanup deleting files it doesn't own

## What to include

- your Casimir version (`casimir --version`) and platform
- a minimal reproduction, with anything sensitive redacted

Please don't attach raw transcripts, checkpoints, credentials or private repositories to a
public issue.

This is a small project and there's no guaranteed response time, but reports are taken
seriously.
