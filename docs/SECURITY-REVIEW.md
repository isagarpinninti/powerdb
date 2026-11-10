# Internal security review

This is a review by the project's own author and tooling, **not an independent audit**. It lists what was examined, what was found and
fixed, and what is still open. An independent review of the areas under "Next" is the sensible step before trusting PowerDB with data you
cannot afford to lose or expose.

## Method
Each change was tested with the attacks that applied to it, and the parsers and decoders are fuzzed (`engine/tests/fuzz.rs`, plus
libFuzzer targets in `fuzz/`). Platform-specific code is exercised in CI on Linux, macOS and Windows.

## Found and fixed
| Area | Finding | Fix |
|---|---|---|
| Storage | Compaction could delete the segment holding chunks of an upload still being staged; the commit then indexed a missing file. Data loss. | Staged chunks are counted per segment under the lock that seals segments; compaction skips those segments. |
| Storage | A damaged metadata version file made the storage library request tens of gigabytes and abort the process (the library never checks its own checksum before parsing). | The version files are verified against their recorded checksums before the library reads them. |
| Storage | A damaged manifest size could trigger a giant allocation. | The size must equal the sum of its chunks; no up-front allocation from stored sizes. |
| Storage | The directory lock outlived `drop` while a background thread finished, refusing an immediate reopen. | The last handle stops and joins the thread. |
| API | The dashboard fallback answered every method with 200. | Only GET and HEAD. |
| Service install | A data path with a space broke the unit; a newline in an argument could add systemd directives. | Arguments are quoted for systemd and validated; account names are restricted. |
| OAuth | The unauthenticated `start` endpoint could be used to fill the table of sign-ins in progress. | Per-client rate limit on `start` and `callback`. |

## Designed in (and tested)
- **OAuth:** authorization-code flow with PKCE; single-use, provider-bound, expiring state; exact-match redirect allow-list (no open redirect);
  token in the URL fragment; verified emails only; administrators excluded unless `allow_admin`; accounts created only with open registration.
- **Desktop app:** the admin key lives in the OS credential store, not in a file. The app window's permissions are limited to dialogs, revealing a file,
  and opening this project's page or the local server; the dashboard it embeds is a page from the local server and is granted none. The key reaches that page only in the URL
  fragment. The app opens no network connections other than to its own loopback server.
- **Backups:** written only under the configured backup directory; names are restricted to `[A-Za-z0-9._-]` and cannot start with a dot.
- **Docker:** the entrypoint drops to an unprivileged user after fixing ownership of the data folder; `chown -R` does not follow symlinks.

## Known limits and open items
- **No independent audit; no power-loss testing.** The crash tests kill the process, which cannot show what a disk does with unflushed writes.
- **Not signed by Apple or Microsoft.** The macOS app has a stable self-signed signature (so the Keychain grant survives updates), which Gatekeeper does not trust. Installers and binaries are covered by checksums and GitHub build attestations, not by Apple or Microsoft signatures.
- **OAuth has not run against real Google or GitHub**, only against a mock that checks the protocol details.
- **The storage library** (`fjall` / `lsm-tree`) has at least one parser that trusts stored lengths. One was found and guarded; others
  could exist in files the fuzzers did not mutate. Treat a data directory from an untrusted source as hostile.
- **Rate limits are in memory** and per process; behind a proxy, set `trust_proxy` correctly or every client shares one address.
- **TLS** is rustls (ring); there is no OCSP stapling or client-certificate authentication.
- **Dashboard and Manager have no automated test of the native shell itself** (tray, window, keychain prompts); CI installs the app, starts
  it and checks the server comes up and goes away, but does not click through it.

## Next (worth an independent review)
The rule expression evaluator and access rules; the upload pipeline (type sniffing, size limits, encryption at rest); JWT/API-key handling;
the signed-URL scheme; the OAuth flow against real providers; and the storage engine's recovery paths.
