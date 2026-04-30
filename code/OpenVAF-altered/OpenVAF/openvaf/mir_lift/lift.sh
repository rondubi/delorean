#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

target="diode"
if [[ $# -gt 0 && "$1" != -* ]]; then
    target="$1"
    shift
fi

case "$target" in
    diode)
        input="$script_dir/../../integration_tests/DIODE/diode.va"
        ;;
    bsim4)
        input="$script_dir/../../integration_tests/BSIM4/bsim4.va"
        ;;
    -h|--help|help)
        cat <<'EOF'
usage:
  ./lift.sh                 # lift the DIODE example
  ./lift.sh diode           # lift the DIODE example
  ./lift.sh bsim4           # lift the BSIM4 example
  ./lift.sh path/to/file.va # lift a Verilog-A file

Any extra arguments are passed through to the runner, for example:
  ./lift.sh diode -o /tmp/diode_lir.py
EOF
        exit 0
        ;;
    *)
        input="$target"
        ;;
esac

exec python3 "$script_dir/mir_lift_runner.py" "$input" "$@"
