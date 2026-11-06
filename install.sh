#!/bin/sh
# Install the latest rsdu release binary.
#   INSTALL_DIR   target directory (default: ~/.local/bin)
#   RSDU_VERSION  release tag to install (default: latest)
set -eu

repo="awaiden/rsdu"
dir="${INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)          target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin-arm64)          target=aarch64-apple-darwin ;;
  *) echo "No prebuilt binary for $(uname -s) $(uname -m). Try: cargo install --git https://github.com/$repo" >&2; exit 1 ;;
esac

if [ -n "${RSDU_VERSION:-}" ]; then
  base="https://github.com/$repo/releases/download/$RSDU_VERSION"
else
  base="https://github.com/$repo/releases/latest/download"
fi
name="rsdu-$target"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $name..."
curl -fsSL "$base/$name.tar.gz" -o "$tmp/$name.tar.gz"
curl -fsSL "$base/$name.tar.gz.sha256" -o "$tmp/$name.tar.gz.sha256"

echo "Verifying checksum..."
(cd "$tmp" && if command -v sha256sum >/dev/null; then sha256sum -c "$name.tar.gz.sha256"; else shasum -a 256 -c "$name.tar.gz.sha256"; fi)

tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/$name/rsdu" "$dir/rsdu"
echo "Installed rsdu to $dir/rsdu"
case ":$PATH:" in *":$dir:"*) ;; *) echo "Note: $dir is not on your PATH." ;; esac
