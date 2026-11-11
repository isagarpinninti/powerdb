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

## Sign in with Google, GitHub or another provider
Optional. Configure it in `powerdb.toml` (see the commented `[oauth]` block there):
```toml
[oauth]
public_url = "https://db.example.com"            # how browsers reach this server
redirects  = ["https://app.example.com/signed-in"]  # where a finished sign-in may send the browser (exact matches)
[oauth.providers.github]
client_id = "..."
[oauth.providers.google]
client_id = "...apps.googleusercontent.com"
```
and set the secrets in the environment, not the file: `POWERDB_OAUTH_GITHUB_CLIENT_SECRET`, `POWERDB_OAUTH_GOOGLE_CLIENT_SECRET`.
At the provider, register `https://db.example.com/api/v1/auth/oauth/<provider>/callback` as the redirect URI.
Any other OAuth2 provider works with `auth_url`, `token_url` and `userinfo_url` (it must report `email` and `email_verified`).

An app starts a sign-in at `db.auth.oauthUrl('github', redirect)` and reads the result with `db.auth.finishOAuth(location.hash)`.
The flow is the authorization-code flow with PKCE, run on the server. The rules it enforces:
- the browser is only ever sent to a URL listed under `redirects`, so the endpoint cannot be used as an open redirect;
- the session token comes back in the URL *fragment*, which servers and proxies never see or log;
- the provider must vouch for the email address; an unverified one is refused;
- the account is the one with that email. A missing account is created (as a normal user, with a password nobody knows)
  only when `open_registration` is on; otherwise sign-in is refused with `no_account`;
- administrator accounts cannot sign in this way unless `allow_admin = true`, because the provider would otherwise be a way into the whole database.

*How this was verified:* tests drive the full flow through the real router against a mock provider that checks PKCE, the client
secret, the exact redirect URI and single-use codes. It has **not** been run against real Google or GitHub, which needs credentials
registered to your domain; try it with a test app there before relying on it.

## Configuration
Order: flags > environment (`POWERDB_*`) > `powerdb.toml` in the data dir > defaults. Unknown keys in the file are errors.

## Backups
**Online (while serving)** — the normal way. An administrator calls `POST /api/v1/maintenance/backups`
(`{"name": "nightly", "include_secret": false, "deep": false}`; all optional), the SDK's `db.admin.backup()`,
or the **Back up now** button on the dashboard's System page. List them with `GET /api/v1/maintenance/backups`.
```bash
curl -X POST -H "authorization: Bearer $POWERDB_KEY" -H 'content-type: application/json' \
     -d '{"name":"nightly"}' https://db.example.com/api/v1/maintenance/backups
```
It is written on the server, under `<data>/backups` unless `backup_dir` is set (put it on another disk, then copy it off the machine).
It does not block reads or writes. The copy is a point-in-time image: every write acknowledged before the call began is in it,
and nothing half-done is. Before it is declared good the copy is opened like a server would open it and its structure and
reference counts are checked (`"deep": true` also re-reads every stored chunk); a backup that fails is deleted, not left looking usable.
Compaction waits while a backup runs.

*How this was verified:* an automated test takes three backups while three writers, a deleter and continuous compaction run,
then restores each and checks completeness, file contents and a full integrity scan (passed 15 consecutive runs after a compaction bug it exposed was fixed).
That is a process-level test on one machine; it does not prove a power cut during a backup is safe.

**Offline** — `powerdb backup --to DIR` with the server stopped (the data-dir lock refuses otherwise). Same format.

The secret key is left out unless `include_secret` / `--include-secret`; without it, encrypted chunks and signed URLs cannot be
recovered, so store the secret separately (or set `POWERDB_SECRET`). **Restore** either kind with
`powerdb restore --from DIR --data NEWDIR` (verifies every checksum first), then `powerdb check`.
A backup directory is not itself encrypted at rest beyond what the encrypted collections already are.

## Monitoring
`GET /api/v1/healthz` is unauthenticated liveness. `GET /api/v1/metrics` is Prometheus text (blob log size, dead bytes, GC backlog, version) and needs an admin API key: `authorization: Bearer pdb_live_...`. Alert on `powerdb_blob_dead_bytes` growing without bound and on a failing `powerdb check`.

## Health and integrity
`powerdb status` (liveness), `powerdb check` (re-hashes all chunks, recomputes reference counts; `--quick` skips chunk hashing).

## Upgrades
Stop, back up, replace the binary, start. Check `powerdb check` and keep the backup until you are satisfied.

The data directory records its layout version in `FORMAT`. A binary refuses, without touching anything, a directory written
by a newer layout. A directory written by an older layout is upgraded in place, one step at a time: the metadata (`meta/`) is
first copied to `pre-migration-v<N>/` (the blob log is append-only and never rewritten, so it is not copied), `FORMAT` is
rewritten after each completed step, and a failed step stops there and names the copy, so the next start resumes.
The first upgrade is 0.3.x (layout 1) to 0.4.0 (layout 2), which adds buckets and objects and rewrites nothing. It is tested on data written by the released 0.3.1 server, but not on large or long-lived databases. **Once a folder has been opened by 0.4.0, 0.3.x refuses it**; going back means restoring the backup you took (or `pre-migration-v1/`, which holds the metadata from before).
Delete `pre-migration-v<N>/` once you no longer need it.

## Durability
`--durability commit` (default, fsync per commit, ~333 commits/s measured on one Mac), `interval` (background fsync), `never` (tests only).

## Security checklist
- The first admin API key is shown once by `init`; store it safely and create scoped keys for apps. Keep `--open-registration` off unless you want public sign-up.
- Leave CORS off unless a browser app on another origin needs it (`--cors-origin`).
- Keep `secret.key` mode 0600, backed up separately.
- Collections with no rule are locked; an empty rule means public.
