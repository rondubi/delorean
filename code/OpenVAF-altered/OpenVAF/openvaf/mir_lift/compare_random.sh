#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="${1:-diode}"
cases="${2:-8}"
seed="${3:-1}"

run_root="${MIR_LIFT_COMPARE_ROOT:-/tmp/mir_lift_compare_checked}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$run_root/target}"
export MIR_LIFT_CURRENT_DIR="${MIR_LIFT_CURRENT_DIR:-$run_root/out}"
mkdir -p "$CARGO_TARGET_DIR" "$MIR_LIFT_CURRENT_DIR"

exec python3 "$script_dir/direct_compare.py" "$target" "$cases" "$seed"
