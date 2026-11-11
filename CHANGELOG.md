# Changelog

## Unreleased
- **Buckets and objects (storage layer).** Files that exist without a record: `bucket` + `key` names a file, with a stable object id, filename, MIME type, size, BLAKE3 checksum and custom metadata. Optional versioning per bucket (every put kept; a delete adds a marker; a named version can be removed for good or restored). Objects use the same chunk store as record files, so deduplication, encryption, garbage collection, compaction, backup and `powerdb check` cover them; a record and an object can share one stored file. Administrator-only for now (per-bucket permissions come later).
- **Buckets and objects over HTTP and the command line.** `/api/v1/buckets`, `PUT|GET|HEAD|DELETE /api/v1/buckets/{bucket}/objects/{key}` (the file is the raw request body, streamed; ranges, `ETag`/`If-None-Match`, custom metadata as `X-PowerDB-Meta-*` headers, `?version=`), listing with prefix and cursor, version listing and restore. The same content-type check as record uploads applies (a program named `.png` is refused). New commands: `powerdb bucket create|ls|rm|versioning`, `powerdb object put|get|ls|rm|versions|restore`, which stream and write downloads atomically. The new routes are pinned by the API contract test.
- **Buckets in the SDK and the dashboard.** `db.buckets` and `db.bucket(name)` in `powerdb-sdk` (put with a string, bytes, Blob or stream; ranged download; list; versions; restore). A **Files** section in the dashboard (and so in the app): create buckets, upload, filter by prefix, download, delete, and a versions dialog with restore.
- Measured on one Apple-silicon laptop through the CLI: a 1 GiB object uploads at about 445 MiB/s and downloads at about 1,140 MiB/s, and the server's memory peaks at 55 MiB (26 MiB idle). One run each, loopback, default durability.
- The command-line client prints plain error messages (`powerdb: ...`), not the server's internal error kinds.
- **On-disk layout 2.** Adds the `buckets` and `objects` keyspaces. A layout-1 directory is copied aside (`pre-migration-v1/`) and upgraded in place on first open; an older binary refuses a layout-2 directory. Going back means restoring a backup or the safety copy.
- **Fixed a data-loss bug in file garbage collection** (present in every release up to 0.3.1). If a file was deleted, attached again before the background collector ran (it runs every few seconds), and deleted again, the collector released it twice in one pass. Chunks it shared with another file that was still in use were counted down twice; once compaction ran, those chunks could be deleted and the other file became unreadable (`powerdb check` reports it as a count mismatch or a missing chunk). It needs the same file content (identical bytes) to be re-used within that window, so it is uncommon, but it is real. Upgrading fixes it for future operations; it cannot repair a database where it already happened, though `powerdb check` will show whether that happened.
- Failure injection: with the `failpoints` feature (never in a release build) a test can abort the process at 11 named points in the commit, collection and compaction paths. A new test crashes at each of them, reopens the data, and requires `powerdb check` to pass, every acknowledged write to be present, and the database to keep working. A deliberately broken compaction order is caught by it.
- API contract test: every `/api/v1` route is driven once and its status code and answer fields are pinned, so a breaking change to the API fails CI (docs/COMPATIBILITY.md).
- Upgrade test in CI: data written by the latest published release (over HTTP, including shared and encrypted files and a delete) must open on the new build, pass `powerdb check`, read back byte for byte, accept writes, and pass `check` again. `tools/upgrade-test.py` runs it by hand.
- New model-based test: random creates, updates, deletes, collection, compaction and restarts against an in-memory model, checking the stored data and the reference counts after each step. It found the bug above.

## 0.3.1 (2026-11-11)
- New look: layered near-black surfaces with hairline borders, a light theme to match, and tighter type. Amber stays the one accent.
- Pages use the full width of the window instead of a centred column (app and dashboard).
- The dashboard embedded in the server is rebuilt with these changes (0.3.0 shipped before them).
- README screenshots retaken.

## 0.3.0 (2026-11-10)
- macOS: the app is signed with a stable certificate, so its identity is the same in every build and the Keychain stops asking again after each update (choose **Always Allow** once). The certificate is self-signed: Gatekeeper still warns on first open.
- New logo: a P built from a stem and a closed bowl, with an amber bar beneath it (the file that belongs to the record). Replaces the lightning bolt. Source files in docs/brand; app, dashboard and favicon updated.
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
