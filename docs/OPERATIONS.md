# Operating PowerDB

## Network and TLS
PowerDB listens on `127.0.0.1:8090` by default. Two ways to serve other machines safely:

1. **Built-in HTTPS.** `powerdb serve --addr 0.0.0.0:8443 --tls-cert cert.pem --tls-key key.pem` (or `tls_cert` / `tls_key` in
   `powerdb.toml`). TLS 1.2/1.3 via rustls. The files are re-read hourly, so a renewed certificate (certbot, step-ca)
   takes effect without a restart; a bad renewal is logged and the old certificate keeps serving.
   Tested by hand with a self-signed certificate on macOS (HTTPS served, plain HTTP refused, bad key refused);
   not yet covered by an automated test.
2. **A TLS proxy** (Caddy, nginx, a load balancer) in front, with `--trust-proxy` so rate limiting sees client addresses.
   `docker-compose.yml` is a Caddy example (unrun).

Never expose plain HTTP beyond localhost: bearer tokens travel in headers. `powerdb status` works against either mode.

## Configuration
Order: flags > environment (`POWERDB_*`) > `powerdb.toml` in the data dir > defaults. Unknown keys in the file are errors.

## Backups
`powerdb backup --to DIR` is **offline**: stop the server first (the data-dir lock refuses otherwise).
The secret key is left out unless `--include-secret`; without it, encrypted chunks and signed URLs
cannot be recovered, so store the secret separately (or set `POWERDB_SECRET`). Restore with
`powerdb restore --from DIR --data NEWDIR`, then `powerdb check`.
There is no hot backup yet.

## Monitoring
`GET /api/v1/healthz` is unauthenticated liveness. `GET /api/v1/metrics` is Prometheus text (blob log size, dead bytes, GC backlog, version) and needs an admin API key: `authorization: Bearer pdb_live_...`. Alert on `powerdb_blob_dead_bytes` growing without bound and on a failing `powerdb check`.

## Health and integrity
`powerdb status` (liveness), `powerdb check` (re-hashes all chunks, recomputes reference counts; `--quick` skips chunk hashing).

## Upgrades
Stop, back up, replace the binary, start. The data directory records its layout version in `FORMAT`: a binary refuses (without touching anything) a directory written by a newer layout. Today there is only layout 1, so there are no migrations yet; keep the backup until `powerdb check` passes on the new version.

## Durability
`--durability commit` (default, fsync per commit, ~333 commits/s measured on one Mac), `interval` (background fsync), `never` (tests only).

## Security checklist
- The first admin API key is shown once by `init`; store it safely and create scoped keys for apps. Keep `--open-registration` off unless you want public sign-up.
- Leave CORS off unless a browser app on another origin needs it (`--cors-origin`).
- Keep `secret.key` mode 0600, backed up separately.
- Collections with no rule are locked; an empty rule means public.
