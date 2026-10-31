# PowerDB

One database for structured records and the files that belong to them. A record and its files are stored by the same engine and become visible together or not at all.

Status: **in development.** What exists, what is tested and what is still to build is listed in [docs/DESIGN.md](docs/DESIGN.md) and below; nothing here claims more than that.

| Part | State |
|---|---|
| Storage engine (LSM metadata + content-defined chunk log, atomic commit, dedup, GC, compaction) | built, 17 tests, `powerdb bench` |
| Security (chunk encryption, signed URLs, magic-byte checks, access rules, auth) | not started |
| HTTP API, filter language, TypeScript SDK | not started |
| Dashboard | not started |

```bash
cargo test --release           # storage tests
cargo run --release -- bench   # measured numbers for this machine
```
