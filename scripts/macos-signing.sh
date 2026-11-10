#!/usr/bin/env bash
# Prepares the code-signing identity for the macOS desktop build (used by CI and the release workflow).
#
# With POWERDB_SIGNING_P12 (base64 of a .p12) and POWERDB_SIGNING_P12_PASSWORD set, the certificate is imported into a
# throwaway keychain and APPLE_SIGNING_IDENTITY is exported for `tauri build`. Without them (forks, pull requests from
# outside) it falls back to ad-hoc signing, so the build still works but the app has no stable identity.
set -euo pipefail
out="${GITHUB_ENV:-/dev/stdout}"
name="${POWERDB_SIGNING_IDENTITY:-PowerDB Release Signing}"

if [ -z "${POWERDB_SIGNING_P12:-}" ]; then
  echo "No signing certificate available: signing ad-hoc."
  echo "APPLE_SIGNING_IDENTITY=-" >> "$out"
  exit 0
fi

kc="$RUNNER_TEMP/powerdb-signing.keychain-db"
pw="$(uuidgen)"
echo "$POWERDB_SIGNING_P12" | base64 --decode > "$RUNNER_TEMP/signing.p12"
security create-keychain -p "$pw" "$kc"
security set-keychain-settings -lut 21600 "$kc"
security unlock-keychain -p "$pw" "$kc"
security import "$RUNNER_TEMP/signing.p12" -k "$kc" -P "$POWERDB_SIGNING_P12_PASSWORD" -T /usr/bin/codesign
rm -f "$RUNNER_TEMP/signing.p12"
security set-key-partition-list -S apple-tool:,apple: -s -k "$pw" "$kc" >/dev/null
# put it first in the search list, keeping the existing keychains
# shellcheck disable=SC2046
security list-keychains -d user -s "$kc" $(security list-keychains -d user | tr -d '"')
echo "APPLE_SIGNING_IDENTITY=$name" >> "$out"
echo "Signing with \"$name\" (self-signed: stable identity, not trusted by Gatekeeper)."
