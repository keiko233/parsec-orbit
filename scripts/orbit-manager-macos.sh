#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/orbit-manager-macos.sh {install|upgrade|uninstall} [options]

Install, upgrade, or uninstall the signed macOS arm64 Parsec Orbit release.
The manager requires macOS arm64, Python 3, and an ordinary user account.

Options:
  --manifest HTTPS_URL  release manifest (default: latest GitHub release)
  --keys FILE           trusted Ed25519 public-key list
  --allow-unsigned      use HTTPS and manifest digests without signature trust
  --app PATH            Parsec.app path (default: /Applications/Parsec.app)
  --orbit-root PATH     Orbit data and manager cache directory
  -h, --help            show help

Uninstall uses the validated local release cache and does not need a network.
EOF
}

if [ "${1-}" = "--help" ] || [ "${1-}" = "-h" ] || [ "${1-}" = "help" ]; then
    usage
    exit 0
fi

python=$(command -v python3 2>/dev/null || true)
if [ -z "$python" ]; then
    printf '%s\n' 'error: Python 3 is required; install Python 3 and retry.' >&2
    exit 1
fi
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
exec "$python" "$script_dir/orbit-manager-macos.py" "$@"
