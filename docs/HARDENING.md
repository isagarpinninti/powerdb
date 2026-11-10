# Hardening a deployment

[OPERATIONS.md](OPERATIONS.md) says how to run PowerDB. This page is a checklist for running it where other people depend on it or
where it can be reached from a network. Each item says why, so you can judge whether it applies to you.

## Before anything else

- [ ] **Keep it on `127.0.0.1` unless something outside this computer needs it.** The default address only accepts local connections.
  Every other item on this page only matters once you listen on a network address.
- [ ] **Never serve plain HTTP across a network.** Bearer tokens and API keys travel in headers. Use the built-in TLS
  (`--tls-cert`, `--tls-key`; the certificate is re-read hourly so renewals need no restart) or a proxy that terminates TLS.
  With built-in TLS the server sends `Strict-Transport-Security`.
- [ ] **Store the administrator key like a root password.** It bypasses every rule. Use it to set things up, then give programs
  scoped keys (`read`, `write`, limited to collections) and give people user accounts.
- [ ] **Back up, and test a restore.** A backup you have never restored is a guess. Copy backups off the machine, and keep
  `secret.key` somewhere separate from them: without it encrypted files cannot be read, and with it a backup is not private.

## The network

- [ ] **Put a firewall or proxy in front of anything public.** The server has request limits and timeouts, but it has no protection
  against a slow attacker opening many connections without sending headers. A reverse proxy (Caddy, nginx, a cloud load balancer)
  does. If you use one, start the server with `--trust-proxy` so rate limits see the real client address, and make sure only the proxy
  can reach the server's port. If clients can reach the port directly, they can forge `X-Forwarded-For` and sidestep the limits.
- [ ] **Leave CORS off** (no `--cors-origin`) unless a web page on another origin must call the API. List exact origins; there is no wildcard.
- [ ] **Leave open registration off** (`--open-registration`) unless anyone may create an account.

## Accounts and sign-in

- [ ] Passwords are hashed with Argon2id and sign-in is rate limited per address and per account, but there is no second factor.
  If that matters, put sign-in behind your own single sign-on using the OAuth settings, and consider disabling password accounts.
- [ ] **OAuth:** list only the redirect URLs you use. Administrator accounts cannot sign in through a provider unless you set
  `allow_admin`; leave that off, because the provider then becomes a way into the whole database.
- [ ] Revoke API keys you no longer use, and rotate a key if it may have been seen (the dashboard shows when each was created).

## The machine

- [ ] **Run it as an unprivileged account.** `powerdb service install` on Linux uses a transient system user (`DynamicUser`) with
  `NoNewPrivileges`, a read-only system and a private `/tmp`; on Windows the service runs as `LocalService`.
- [ ] **Docker:** the image starts as root only long enough to fix the ownership of a mounted data folder, then runs as user 10001.
  If you start it with `--user <uid>:<gid>` on a folder that user owns, it never has root at all.
- [ ] **Protect the data folder.** `secret.key` is created readable only by its owner. Anyone who can read the data folder as that
  user can read everything, encrypted collections included, because the key is next to the data. Disk encryption (FileVault,
  BitLocker, LUKS) is what protects a stolen disk.
- [ ] Keep the operating system and PowerDB up to date. [SECURITY.md](../SECURITY.md) says how to report a vulnerability, and the
  release notes say what each version fixed.

## Watching it

- [ ] **Monitoring.** `GET /api/v1/healthz` is a liveness check with no sign-in. `GET /api/v1/metrics` (administrator key) is
  Prometheus text with storage size, space held by deleted files and the compaction backlog.
- [ ] **Logs** go to standard error (`journalctl -u powerdb` under systemd, `<data>/powerdb.log` for services on macOS and Windows).
  They do not contain tokens or passwords. There is one exception: the first start of a *new* data folder prints the administrator
  key once, so you can copy it. Under a service that line is saved in the log. Avoid it by running `powerdb init <folder>` yourself
  first (it prints the key to your terminal), then installing the service on that folder; or, if it already happened, create a new
  key with `powerdb create-key --admin`, revoke the old one, and delete the old log line. Rotate log files with your usual tool.
- [ ] **Integrity.** Run `powerdb check` on a restored copy of a backup now and then. It re-reads every stored chunk and recomputes
  every count; it needs the database to itself, so do it on the copy, not the live data.

## What PowerDB does not do

So you do not assume it: there is no audit trail of administrator actions, no multi-factor sign-in, no per-record encryption keys
(encryption is per collection, with one server key), no clustering or replication, and no independent security review yet.
[SECURITY-REVIEW.md](SECURITY-REVIEW.md) lists what has and has not been examined.
