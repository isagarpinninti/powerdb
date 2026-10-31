# Changelog

## 0.1.0 (unreleased)
- Storage: LSM records + content-defined chunked, deduplicated, refcounted file store; atomic record+file commits.
- Security: rule expressions, ChaCha20-Poly1305 at rest, signed URLs, magic-byte validation, JWT/API keys, Argon2id.
- API, TypeScript SDK, embedded React dashboard.
- Operations: data-dir lock, `check`, offline backup/restore, `init`/`status`, settings file, graceful shutdown.
- Packaging and CI written; verified on macOS arm64 only.
