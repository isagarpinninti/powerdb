# PowerDB

One database for structured records and the files that belong to them. A record and its files are stored by the same engine and become visible together or not at all.

Status: **in development.** What exists, what is tested and what is still to build is listed in [docs/DESIGN.md](docs/DESIGN.md) and below; nothing here claims more than that.

| Part | State |
|---|---|
| Storage engine (LSM metadata + content-defined chunk log, atomic commit, dedup, GC, compaction) | built, tested, `powerdb bench`; killed-writer crash test and a hot-backup stress test |
| Security (chunk encryption, signed URLs, magic-byte checks, access rules, auth) | built, tested; fuzzed (rules language, upload checks, tokens, damaged storage, HTTP) |
| HTTP API (CRUD, filter/sort/pagination, streaming uploads, ranged downloads, keys, throttling) | built, tested over the real router |
| TypeScript SDK | built, tested against a real server process |
| Dashboard (collections, schema builder, virtualized grid with image thumbnails, access-rule editor, code generator, users, keys, backups) | built, embedded in the binary; component tests run against a real server |
| OAuth2 sign-in (Google, GitHub, generic) | built, tested against a mock provider that checks PKCE and the redirect rules; not run against the real services |
| Desktop app (PowerDB Manager): `.dmg` for macOS, `.exe` installer for Windows | built; CI installs it, starts it and checks its server; unsigned (see [docs/INSTALL.md](docs/INSTALL.md)) |
| Run as a service (Windows Service, launchd, systemd) | built; CI installs and drives each one on its own platform |
| Hot backup, restore, integrity check, data-format migrations | built and tested (no real migration has run: only format 1 exists) |

```bash
cargo test --release                      # the whole suite, about a minute
cargo run --release -- bench              # engine numbers for this machine (add --sync or --interval)
cargo run --release -- serve              # API and dashboard on 127.0.0.1:8090; the first run prints an admin key
```

Access is locked by default: a collection with no rule for an action can be used only by an administrator. A rule is an expression such as
`@request.auth.id != ""` (anyone signed in), `@request.auth.id == owner_id` (only the owner), or an empty string (public). Performance figures and what each durability mode risks are in [docs/DESIGN.md](docs/DESIGN.md).

## Dashboard

`powerdb serve` serves the dashboard on the same port as the API. Sign in with the admin key printed on first start, or with an administrator account. The theme uses the same tokens as the hypout project (neutral zinc surfaces, amber only as an accent, dark by default, light on request or when the OS asks for it). To rebuild it: `cd dashboard && npm run build`, then `cargo build --release` (the compiled assets live in `engine/ui` and are embedded).

Known gaps: PDFs show a labelled tile rather than a rendered thumbnail (that needs a PDF renderer); the dashboard is tested with a simulated DOM, not a real browser.

## Install and operate
- **Desktop (macOS, Windows):** download the `.dmg` or the setup `.exe` from the [latest release](https://github.com/isagarpinninti/powerdb/releases/latest).
- **Command line (macOS, Linux, Windows):** `scripts/install.sh` / `scripts/install.ps1`, or `docker run ghcr.io/isagarpinninti/powerdb`.
- **As a service:** `powerdb service install`.

See [docs/INSTALL.md](docs/INSTALL.md) and [docs/OPERATIONS.md](docs/OPERATIONS.md). The test suite runs in CI on Linux (x64, arm64), macOS (arm64, Intel)
and Windows. Releases are checksummed and carry GitHub build-provenance attestations; **the installers are not code-signed**.
What has been reviewed, and what has not, is in [docs/SECURITY-REVIEW.md](docs/SECURITY-REVIEW.md): this is pre-1.0 software that has had no independent audit.
