#!/bin/bash
# Launch the independent macOS terminal log viewer; it never opens or controls Parsec.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd -P)
if ! command -v python3 >/dev/null 2>&1; then
    printf 'console-macos requires Python 3 (python3) on PATH.\n' >&2
    exit 127
fi

exec python3 "$script_dir/console-macos.py" "$@"
