#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./run_lift_checked.sh [all|lift|compare] [model] [cases] [seed]

Defaults:
  mode:  all
  model: bsim4
  cases: 1
  seed:  1

Runs the current working tree with an isolated Cargo target directory:
  /tmp/mir_lift_checked/target

Fresh lifted Python is written to:
  /tmp/mir_lift_checked/out/<model>.py
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mode="${1:-all}"
model="${2:-bsim4}"
cases="${3:-1}"
seed="${4:-1}"

case "$mode" in
  all|lift|compare) ;;
  *)
    echo "error: mode must be all, lift, or compare" >&2
    usage >&2
    exit 2
    ;;
esac

run_root="${MIR_LIFT_CHECKED_ROOT:-/tmp/mir_lift_checked}"
target_dir="${MIR_LIFT_CHECKED_TARGET_DIR:-$run_root/target}"
out_dir="${MIR_LIFT_CHECKED_OUT_DIR:-$run_root/out}"
output="$out_dir/$model.py"

mkdir -p "$target_dir" "$out_dir"
export CARGO_TARGET_DIR="$target_dir"
export MIR_LIFT_CURRENT_DIR="$out_dir"

echo "CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
echo "MIR_LIFT_CURRENT_DIR=$MIR_LIFT_CURRENT_DIR"

if [[ "$mode" == "all" || "$mode" == "lift" ]]; then
  rm -f "$output"
  echo "lifting $model -> $output"
  "$script_dir/lift.sh" "$model" -o "$output"
  test -s "$output"
fi

if [[ "$mode" == "all" || "$mode" == "compare" ]]; then
  echo "compare_random $model $cases $seed"
  "$script_dir/compare_random.sh" "$model" "$cases" "$seed"
fi
