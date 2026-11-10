# Changelog

## Unreleased
- One application: the dashboard is the app's **Data** section, not a separate window.
- New app layout: grouped sidebar, command palette (Cmd/Ctrl+K), always-visible status bar, health overview, log levels.
- Security: a panicking request handler no longer takes the whole server down (the release profile unwinds, one request gets a 500);
  stalled uploads are cut off (body idle timeout) and JSON requests have a deadline; API answers are `no-store`; HSTS when serving TLS;
  the app's permission to open URLs is limited to this project and the local server.
- The app no longer blocks on the Keychain while drawing its window (it showed a blank window while macOS asked for permission).
- The server no longer aborts or hangs on shutdown when its log pipe has gone (it did when the app that started it crashed).
- `PATCH /collections/{name}`: the name comes from the address; a body without `fields` is refused (it would have erased the schema).
- `powerdb create-key --admin` now means full access by itself (it used to fail on its own default).
- Docs: a how-to guide whose commands were all run against a real server; README rewritten; screenshots and the tools that take them.

## 0.2.1 (2026-11-01)
- Same code as 0.2.0, which was never published: its release workflow built everything but failed to gather the installers. Fixed.

## 0.2.0 (2026-11-01, not published)
- Desktop app (PowerDB Manager): .dmg for macOS and an installer .exe for Windows; server manager, logs, backups, restore, integrity check, tray, dashboard in its own window, admin key in the OS credential store.
- `powerdb service install|uninstall|start|stop|status`: Windows Service, launchd, systemd. Replaces the hand-written unit files.
- OAuth2 sign-in (Google, GitHub, generic) with PKCE; SDK helpers.
- Data-format migration framework (no real migration yet: only format 1 exists).
- Docker: bind-mounted data folders work (entrypoint fixes ownership, then drops privileges).
- `serve --shutdown-on-stdin-close`; the dashboard signs in from `/#key=...`.
- Build provenance attestations for release artifacts.

## 0.1.1 (2026-11-01)
- Docker image is now multi-arch (linux/amd64 and linux/arm64), each built natively and merged into one tag.
- CI builds and runs the container on both architectures: health check, API use, hot backup, and a clean stop/start.

## 0.1.0 (2026-11-01)
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
- Packaging, CI and release workflows written. CI runs the tests on Linux, macOS and Windows; packaging and releases are unrun.
