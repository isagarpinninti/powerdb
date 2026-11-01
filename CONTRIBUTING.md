# Contributing

```bash
cargo fmt --check
cargo clippy --release --all-targets -- -D warnings
cargo test --release
(cd sdk && npm ci && npm test)
(cd dashboard && npm ci && npm run build)   # rebuilds engine/ui, which is committed
```
Keep claims measured: benchmark numbers and platform support in docs must come from a run, not an assumption.
Security-relevant changes need a test in `engine/tests/security.rs` or `api.rs`.

## Fuzzing
`cargo test --release --test fuzz` runs deterministic fuzzers (a few seconds). They are not coverage-guided: they mutate known-good
inputs and grammar pieces. For a long run: `POWERDB_FUZZ_ITERS=1000000 POWERDB_FUZZ_SEED=$RANDOM cargo test --release --test fuzz`.
A failure prints the seed and iteration; replay it with `POWERDB_FUZZ_START=<n> POWERDB_FUZZ_ITERS=1`.
Coverage-guided targets live in `fuzz/` (`cargo +nightly fuzz run --fuzz-dir ../fuzz expr`, from `engine/`); a weekly workflow runs them.
When a fuzzer finds a bug, add a plain regression test beside the fix.
