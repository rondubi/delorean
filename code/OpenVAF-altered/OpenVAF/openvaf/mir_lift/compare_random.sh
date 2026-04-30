#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="${1:-diode}"
cases="${2:-8}"
seed="${3:-1}"

exec python3 "$script_dir/direct_compare.py" "$target" "$cases" "$seed"
