# Local evidence and sharing

Raw prompts, harness transcripts, workspace checkpoints and model responses are local private
evidence. They can contain credentials, ignored configuration files, proprietary source and
personal data. Unix storage directories use mode 0700 and newly written metadata uses mode
0600. On Windows, private storage and atomic metadata receive a protected ACL granting access to
the object owner and SYSTEM. This does not isolate a harness running as the same user.

Provider credentials are obtained from environment variables or provider credential stores.
The direct Anthropic backend uses in-process HTTPS with redirects disabled. Doctor reports
status, not credential contents, and makes no model completions.

Use `casimir export RUN --share -o shared.json` (or Markdown) for sharing. Sharing exports
redact known credential patterns, sensitive structured keys, and credential values currently
present in the environment. They carry an explicit redaction marker. Checkpoint references
and native log paths are omitted from JSON sharing exports. Human-review pair exports are
redacted and include `sharing.json`. Keep the answer key private to avoid unblinding reviewers.

Redaction is a best-effort transformation: arbitrary secrets cannot always be recognized.
Review the resulting file before sharing. Raw exports without `--share` retain evidence.

Windows implementation reference: [Microsoft security information flags](https://learn.microsoft.com/en-us/windows/win32/secauthz/security-information).
