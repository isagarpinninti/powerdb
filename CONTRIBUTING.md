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
