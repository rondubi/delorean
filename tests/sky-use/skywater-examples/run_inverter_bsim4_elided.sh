#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"

# Runs the SkyWater inverter netlist with the elided BSIM4 OSDI plugin.

NGSPICE_BIN="${NGSPICE_BIN:-${HOME}/opt/ngspice/bin/ngspice}"
OSDI_PATH="${OSDI_PATH:-${REPO_ROOT}/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.elided.osdi}"
NETLIST="${NETLIST:-${SCRIPT_DIR}/c7552_ann_skywater_inverter_osdi.net}"
LOG="${LOG:-${SCRIPT_DIR}/artifacts/logs/run_inverter_bsim4_elided.log}"
RAW="${RAW:-${SCRIPT_DIR}/artifacts/raw/inverter_bsim4_elided.raw}"

mkdir -p "$(dirname "${LOG}")" "$(dirname "${RAW}")"
if [ -n "${WRDATA:-}" ]; then
  mkdir -p "$(dirname "${WRDATA}")"
fi

exec "${NGSPICE_BIN}" -b -o "${LOG}" -r "${RAW}" <<EOF
* driver deck
.control
pre_osdi ${OSDI_PATH}
source ${NETLIST}
run
quit
.endc
.end
EOF
