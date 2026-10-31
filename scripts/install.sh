#!/bin/sh
# Install the PowerDB binary for macOS or Linux. Usage: install.sh [version]   (default: latest)
set -eu
REPO="${POWERDB_REPO:?set POWERDB_REPO=owner/name}"
os=$(uname -s); arch=$(uname -m)
case "$os-$arch" in
  Linux-x86_64)  t=x86_64-unknown-linux-gnu ;;
  Linux-aarch64) t=aarch64-unknown-linux-gnu ;;
  Darwin-arm64)  t=aarch64-apple-darwin ;;
  Darwin-x86_64) t=x86_64-apple-darwin ;;
  *) echo "unsupported platform: $os $arch" >&2; exit 1 ;;
esac
v="${1:-latest}"
if [ "$v" = latest ]; then base="https://github.com/$REPO/releases/latest/download"; else base="https://github.com/$REPO/releases/download/$v"; fi
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$base/powerdb-$t.tar.gz" -o "$tmp/pkg.tar.gz"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
want=$(grep "powerdb-$t.tar.gz" "$tmp/SHA256SUMS" | cut -d' ' -f1)
if command -v sha256sum >/dev/null; then got=$(sha256sum "$tmp/pkg.tar.gz" | cut -d' ' -f1); else got=$(shasum -a 256 "$tmp/pkg.tar.gz" | cut -d' ' -f1); fi
[ -n "$want" ] && [ "$want" = "$got" ] || { echo "checksum mismatch; refusing to install" >&2; exit 1; }
tar -xzf "$tmp/pkg.tar.gz" -C "$tmp"
dest="${POWERDB_BIN_DIR:-$HOME/.local/bin}"; mkdir -p "$dest"
install -m 755 "$tmp/powerdb" "$dest/powerdb"
echo "installed $dest/powerdb"; "$dest/powerdb" --version
case ":$PATH:" in *":$dest:"*) ;; *) echo "add $dest to your PATH" ;; esac
