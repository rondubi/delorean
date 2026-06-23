#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$script_dir/symex_raw_eval.py"

if [[ ! -f "$script" ]]; then
  echo "error: missing $script" >&2
  exit 1
fi

exec python3 "$script" --max-ifs 40 "$@"
