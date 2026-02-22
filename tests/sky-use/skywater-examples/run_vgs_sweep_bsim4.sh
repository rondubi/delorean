#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"

# Runs the VGS sweep deck with the standard BSIM4 OSDI plugin.

NGSPICE_BIN="${NGSPICE_BIN:-${HOME}/opt/ngspice/bin/ngspice}"
OSDI_PATH="${OSDI_PATH:-${REPO_ROOT}/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.osdi}"
NETLIST="${NETLIST:-${SCRIPT_DIR}/vgs_sweep_netlist.spice}"
LOG="${LOG:-${SCRIPT_DIR}/artifacts/logs/run_vgs_sweep_bsim4.log}"
RAW="${RAW:-${SCRIPT_DIR}/artifacts/raw/vgs_sweep_bsim4.raw}"
WRDATA="${WRDATA:-${SCRIPT_DIR}/artifacts/wrdata/vgs_sweep_bsim4_out.txt}"

mkdir -p "$(dirname "${LOG}")" "$(dirname "${RAW}")"
if [ -n "${WRDATA:-}" ]; then
  mkdir -p "$(dirname "${WRDATA}")"
fi

exec "${NGSPICE_BIN}" -b -o "${LOG}" -r "${RAW}" <<EOFNG
* driver deck
.control
pre_osdi ${OSDI_PATH}
set ngbehavior=hsa
set ng_nomodcheck
source ${NETLIST}
set wr_singlescale
set wr_vecnames
save all
op
dc vsgp -0.5 1.8 0.01
wrdata ${WRDATA} \
  @m.XML.msky130_fd_pr__pfet_01v8_lvt[gm] @m.XML.msky130_fd_pr__pfet_01v8_lvt[id] \
  @m.XMS.msky130_fd_pr__pfet_01v8[gm] @m.XMS.msky130_fd_pr__pfet_01v8[id] \
  @m.XMH.msky130_fd_pr__pfet_01v8_hvt[gm] @m.XMH.msky130_fd_pr__pfet_01v8_hvt[id]
quit
.endc
.end
EOFNG
