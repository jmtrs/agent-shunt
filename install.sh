#!/bin/sh
set -eu

repo='jmtrs/agent-shunt'
base_url="https://github.com/$repo/releases/latest/download"

case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) target='aarch64-apple-darwin' ;;
  Darwin:x86_64) target='x86_64-apple-darwin' ;;
  Linux:aarch64|Linux:arm64) target='aarch64-unknown-linux-gnu' ;;
  Linux:x86_64) target='x86_64-unknown-linux-gnu' ;;
  *) printf '%s\n' 'Unsupported platform. See the release assets or build from source.' >&2; exit 1 ;;
esac

command -v curl >/dev/null 2>&1 || { printf '%s\n' 'curl is required.' >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { printf '%s\n' 'tar is required.' >&2; exit 1; }

if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  printf '%s\n' 'sha256sum or shasum is required to verify the download.' >&2
  exit 1
fi

asset="agent-shunt-$target.tar.gz"
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM

curl -fLsS "$base_url/$asset" -o "$tmpdir/$asset"
curl -fLsS "$base_url/$asset.sha256" -o "$tmpdir/$asset.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmpdir" && sha256sum -c "$asset.sha256")
else
  (cd "$tmpdir" && shasum -a 256 -c "$asset.sha256")
fi
tar -xzf "$tmpdir/$asset" -C "$tmpdir" agent-shunt

install_dir=${AGENT_SHUNT_INSTALL_DIR:-"$HOME/.local/bin"}
mkdir -p "$install_dir"
install -m 755 "$tmpdir/agent-shunt" "$install_dir/agent-shunt"
"$install_dir/agent-shunt" --version

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) printf 'Add this to your shell profile: export PATH="%s:$PATH"\n' "$install_dir" ;;
esac

if ! command -v rg >/dev/null 2>&1; then
  printf '%s\n' 'Install ripgrep (rg) before using agent-shunt.'
fi
