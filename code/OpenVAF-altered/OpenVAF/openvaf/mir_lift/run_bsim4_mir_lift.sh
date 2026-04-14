#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$script_dir/mir_lift_runner.py" "$script_dir/../../integration_tests/BSIM4/bsim4.va" "$@"
