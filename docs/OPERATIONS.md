# Operating PowerDB

## Network and TLS
PowerDB listens on `127.0.0.1:8090` by default and does **not** terminate TLS. For anything beyond
localhost put a TLS proxy in front (Caddy, nginx, a cloud load balancer) and pass `--trust-proxy`
so rate limiting sees the real client address. `docker-compose.yml` is a working Caddy example
(unrun). Bearer tokens travel in headers; never expose plain HTTP to the internet.

## Configuration
Order: flags > environment (`POWERDB_*`) > `powerdb.toml` in the data dir > defaults. Unknown keys in the file are errors.

## Backups
`powerdb backup --to DIR` is **offline**: stop the server first (the data-dir lock refuses otherwise).
The secret key is left out unless `--include-secret`; without it, encrypted chunks and signed URLs
cannot be recovered, so store the secret separately (or set `POWERDB_SECRET`). Restore with
`powerdb restore --from DIR --data NEWDIR`, then `powerdb check`.
There is no hot backup yet.

## Health and integrity
`powerdb status` (liveness), `powerdb check` (re-hashes all chunks, recomputes reference counts; `--quick` skips chunk hashing).

## Upgrades
Stop, back up, replace the binary, start. There is no migration tooling yet; keep the backup until `powerdb check` passes on the new version.

## Durability
`--durability commit` (default, fsync per commit, ~333 commits/s measured on one Mac), `interval` (background fsync), `never` (tests only).

## Security checklist
- The first admin API key is shown once by `init`; store it safely and create scoped keys for apps. Keep `--open-registration` off unless you want public sign-up.
- Leave CORS off unless a browser app on another origin needs it (`--cors-origin`).
- Keep `secret.key` mode 0600, backed up separately.
- Collections with no rule are locked; an empty rule means public.
