#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compare_timeout="${COMPARE_RANDOM_TIMEOUT:-120s}"

exec timeout "$compare_timeout" python3 "$script_dir/direct_compare.py" "$@"
