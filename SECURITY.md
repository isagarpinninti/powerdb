# Security policy

PowerDB is pre-1.0 and has not had an independent security audit.

Report vulnerabilities privately through GitHub's "Report a vulnerability" (Security tab), not a public issue.
Include the version, steps to reproduce and impact. Expect an acknowledgement, not an SLA.

Scope: the server, auth, rules engine, signed URLs, at-rest encryption and upload validation.
Known limits are listed in `docs/OPERATIONS.md` (no built-in TLS, offline backups only).
