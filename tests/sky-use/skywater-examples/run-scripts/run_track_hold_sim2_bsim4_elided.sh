#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"

# Runs the track-and-hold sim2 deck with the elided BSIM4 OSDI plugin.

NGSPICE_BIN="${NGSPICE_BIN:-${HOME}/opt/ngspice/bin/ngspice}"
OSDI_PATH="${OSDI_PATH:-${REPO_ROOT}/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.elided.osdi}"
NETLIST="${NETLIST:-${REPO_ROOT}/netlists/track_hold_sim2.spice}"
LOG="${LOG:-${REPO_ROOT}/artifacts/logs/run_track_hold_sim2_bsim4_elided.log}"
RAW="${RAW:-${REPO_ROOT}/artifacts/raw/track_hold_sim2_bsim4_elided.raw}"
WRDATA="${WRDATA:-${REPO_ROOT}/artifacts/wrdata/track_hold_sim2_bsim4_elided_out.txt}"

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
