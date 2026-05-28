#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./export_lifted_python.sh [source-or-stem] [target-name-or-stem]

Exports a generated lifted Python file to ~/macadder.

Defaults:
  source: /tmp/mir_lift_current/bsim4.py
  target: ~/macadder/bsim4.py

Examples:
  ./export_lifted_python.sh
  ./export_lifted_python.sh diode
  ./export_lifted_python.sh /tmp/mir_lift_current/bsim4.py
  ./export_lifted_python.sh /tmp/mir_lift_current/bsim4.py bsim4-latest

Deletion behavior:
  Only the matching destination file under ~/macadder is removed before copy.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

source_arg="${1:-bsim4}"
target_arg="${2:-}"
current_dir="/tmp/mir_lift_current"
export_dir="$HOME/macadder"

if [[ "$source_arg" == */* || "$source_arg" == *.py ]]; then
  source_path="$source_arg"
else
  source_path="$current_dir/$source_arg.py"
fi

if [[ ! -f "$source_path" ]]; then
  echo "error: source file not found: $source_path" >&2
  exit 1
fi

if [[ -n "$target_arg" ]]; then
  target_name="$(basename "$target_arg")"
  if [[ "$target_name" != *.py ]]; then
    target_name="$target_name.py"
  fi
else
  target_name="$(basename "$source_path")"
fi

if [[ -z "$target_name" || "$target_name" == "." || "$target_name" == ".." ]]; then
  echo "error: invalid target name" >&2
  exit 1
fi

mkdir -p "$export_dir"
target_path="$export_dir/$target_name"

if [[ -e "$target_path" ]]; then
  rm -f "$target_path"
fi

cp "$source_path" "$target_path"
echo "exported $source_path -> $target_path"
