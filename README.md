# PowerDB

One database for structured records and the files that belong to them. A record and its files are stored by the same engine and become visible together or not at all.

Status: **in development.** What exists, what is tested and what is still to build is listed in [docs/DESIGN.md](docs/DESIGN.md) and below; nothing here claims more than that.

| Part | State |
|---|---|
| Storage engine (LSM metadata + content-defined chunk log, atomic commit, dedup, GC, compaction) | built, 18 tests, `powerdb bench` |
| Security (chunk encryption, signed URLs, magic-byte checks, access rules, auth) | built, 17 tests |
| HTTP API (CRUD, filter/sort/pagination, streaming uploads, ranged downloads, keys, throttling) | built, 15 tests over the real router, smoke-tested with curl |
| TypeScript SDK | built, 7 tests against a real server process |
| Dashboard (collections, schema builder, virtualized grid with image thumbnails, access-rule editor, code generator, users, keys) | built, embedded in the binary (5 MB); checked by eye in a browser, no automated UI tests |
| OAuth2 sign-in (Google, GitHub) | not started |

```bash
cargo test --release                      # 50 tests
cargo run --release -- bench              # engine numbers for this machine (add --sync or --interval)
cargo run --release -- serve              # API and dashboard on 127.0.0.1:8090; the first run prints an admin key
```

Access is locked by default: a collection with no rule for an action can be used only by an administrator. A rule is an expression such as
`@request.auth.id != ""` (anyone signed in), `@request.auth.id == owner_id` (only the owner), or an empty string (public). Performance figures and what each durability mode risks are in [docs/DESIGN.md](docs/DESIGN.md).

## Dashboard

`powerdb serve` serves the dashboard on the same port as the API. Sign in with the admin key printed on first start, or with an administrator account. The theme uses the same tokens as the hypout project (neutral zinc surfaces, amber only as an accent, dark by default, light on request or when the OS asks for it). To rebuild it: `cd dashboard && npm run build`, then `cargo build --release` (the compiled assets live in `engine/ui` and are embedded).

Known gaps: PDFs show a labelled tile rather than a rendered thumbnail (that needs a PDF renderer); OAuth2 sign-in (Google, GitHub) is not implemented; there are no automated browser tests for the dashboard.
