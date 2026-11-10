# Making a release

Releases are built by `.github/workflows/release.yml` when a tag `v*` is pushed. Everything is built on GitHub's runners, so what you publish is what the workflow built from the tagged commit.

## Before tagging

1. `main` is green: the `ci` workflow passed on the commit you will tag (all jobs, including the desktop and service ones).
2. Pick the version and change it everywhere it appears:
   `Cargo.toml` (`[workspace.package] version`), `desktop/src-tauri/Cargo.toml`, `desktop/src-tauri/tauri.conf.json`,
   and `version` in `sdk/package.json`, `dashboard/package.json`, `desktop/package.json` and their `package-lock.json`.
   Then `cargo update -p powerdb --offline` (and the same with `-p powerdb-desktop` in `desktop/src-tauri`) so the lock files follow.
3. Move the `Unreleased` section of [CHANGELOG.md](CHANGELOG.md) under the new version and date.
4. Commit, push, and wait for `ci` to pass on that commit.

## Tagging

```bash
git tag -a vX.Y.Z -m "PowerDB X.Y.Z" <commit>
git push origin vX.Y.Z
```

Do not move a tag once it is pushed. If a release run fails, fix forward with a new patch version.

## After the workflow finishes

Check the published release, not just the green tick:

```bash
gh release view vX.Y.Z --json assets --jq '.assets[].name'     # 5 archives, 2 .dmg, 1 setup .exe, SHA256SUMS
gh release download vX.Y.Z -D /tmp/pdb && (cd /tmp/pdb && shasum -a 256 -c SHA256SUMS)
gh attestation verify /tmp/pdb/PowerDB_X.Y.Z_aarch64.dmg --repo isagarpinninti/powerdb
POWERDB_REPO=isagarpinninti/powerdb ./scripts/install.sh && powerdb --version
```

Install the `.dmg` (and the Windows installer, if you have a Windows machine) and start it once. The `ci` workflow's `desktop`,
`service` and `install-scripts` jobs do this on every push, but they use the build from that commit, not the published file.

## Code signing (not set up yet)

The installers are currently unsigned: macOS shows "unidentified developer" and Windows SmartScreen warns. This needs certificates
that cost money and belong to a person or company, so it cannot be done from the repository alone. When you have them:

**macOS** (Apple Developer Program, about US$99 a year). Create a "Developer ID Application" certificate, export it as a `.p12`,
and add these repository secrets: `APPLE_CERTIFICATE` (the `.p12`, base64), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`
(for example `Developer ID Application: Your Name (TEAMID)`), and for notarization `APPLE_ID`, `APPLE_PASSWORD` (an app-specific
password) and `APPLE_TEAM_ID`. Pass them as environment variables to the `npx tauri build` step of the `desktop` job in
`release.yml`; Tauri then signs and notarizes. Remove `"signingIdentity": "-"` from `desktop/src-tauri/tauri.conf.json` at the
same time (that value means "ad-hoc"). Check the result with `spctl -a -vv PowerDB.app` and `xcrun stapler validate`.

**Windows.** Use an Authenticode certificate (or Azure Trusted Signing) and configure `bundle.windows.signCommand` or
`certificateThumbprint` in `tauri.conf.json`. Check with `signtool verify /pa PowerDB_X.Y.Z_x64-setup.exe`.

Test signing on a throwaway tag in a fork first; a mistake here fails the whole release job.

## Not yet automated

Publishing to Homebrew, winget or a package repository; automatic updates inside the app (they need a signing key of their own).
