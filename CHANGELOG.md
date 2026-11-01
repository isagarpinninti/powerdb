# Changelog

## 0.1.0 (unreleased)
- Storage: LSM records + content-defined chunked, deduplicated, refcounted file store; atomic record+file commits.
- Security: rule expressions, ChaCha20-Poly1305 at rest, signed URLs, magic-byte validation, JWT/API keys, Argon2id.
- API, TypeScript SDK, embedded React dashboard.
- Operations: data-dir lock, `check`, offline backup/restore, `init`/`status`, settings file, graceful shutdown.
- Built-in HTTPS (rustls) with hourly certificate reload; `status` probes HTTP or HTTPS.
- Data format marker (`FORMAT`); newer layouts are refused untouched.
- Crash test: a committing writer is killed repeatedly; acknowledged writes survive.
- Prometheus `/api/v1/metrics` (admin only).
- Fix: the lock could outlive `drop(db)` while the fsync thread finished (found by CI on macOS Intel).
- Online (hot) backup: API, SDK, dashboard button; consistent point-in-time copy, self-verified; `backup_dir` setting.
- Fix (data loss): compaction could delete the segment holding chunks of an upload still being staged, so the later commit indexed a missing file. Staged chunks are now tracked per segment and compaction skips those segments.
- Dashboard fallback answers only GET/HEAD (was 200 to any method).
- Fuzzing: deterministic in-process fuzzers for the rule language, upload type checks, stored formats, tokens, settings, damaged storage, tampered backups and the HTTP API; libFuzzer targets in `fuzz/`.
- Packaging and CI written; verified on macOS arm64 only.
