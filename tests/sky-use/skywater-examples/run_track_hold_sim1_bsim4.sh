#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"

# Runs the track-and-hold sim1 deck with the standard BSIM4 OSDI plugin.

NGSPICE_BIN="${NGSPICE_BIN:-${HOME}/opt/ngspice/bin/ngspice}"
OSDI_PATH="${OSDI_PATH:-${REPO_ROOT}/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.osdi}"
NETLIST="${NETLIST:-${SCRIPT_DIR}/track_hold_sim1.spice}"
LOG="${LOG:-${SCRIPT_DIR}/artifacts/logs/run_track_hold_sim1_bsim4.log}"
RAW="${RAW:-${SCRIPT_DIR}/artifacts/raw/track_hold_sim1_bsim4.raw}"
WRDATA="${WRDATA:-${SCRIPT_DIR}/artifacts/wrdata/track_hold_sim1_bsim4_out.txt}"

mkdir -p "$(dirname "${LOG}")" "$(dirname "${RAW}")"
if [ -n "${WRDATA:-}" ]; then
  mkdir -p "$(dirname "${WRDATA}")"
fi

exec "${NGSPICE_BIN}" -b -o "${LOG}" -r "${RAW}" <<EOF
* driver deck
.control
pre_osdi ${OSDI_PATH}
set ngbehavior=hsa
set ng_nomodcheck
source ${NETLIST}
set wr_singlescale
set wr_vecnames
run
wrdata ${WRDATA} out
quit
.endc
.end
EOF
