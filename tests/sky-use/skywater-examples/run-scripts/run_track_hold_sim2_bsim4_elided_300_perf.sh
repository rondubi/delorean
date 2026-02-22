#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"

# Runs the track-and-hold sim2 deck with the elided BSIM4 OSDI plugin, with perf + strace.

NGSPICE_BIN="${NGSPICE_BIN:-${HOME}/opt/ngspice/bin/ngspice}"
OSDI_PATH="${OSDI_PATH:-${REPO_ROOT}/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.elided.osdi}"
NETLIST="${NETLIST:-${REPO_ROOT}/netlists/track_hold_sim2_300.spice}"
LOG="${LOG:-${REPO_ROOT}/artifacts/logs/run_track_hold_sim2_bsim4_elided_300_perf.log}"
RAW="${RAW:-${REPO_ROOT}/artifacts/raw/run_track_hold_sim2_bsim4_elided_300_perf.raw}"
WRDATA="${WRDATA:-${REPO_ROOT}/artifacts/wrdata/run_track_hold_sim2_bsim4_elided_300_perf_out.txt}"

PERF_BIN="${PERF_BIN:-perf}"
PERF_EVENTS="${PERF_EVENTS:-task-clock,cycles,instructions,branches,branch-misses,cache-misses,context-switches,cpu-migrations}"
STRACE_BIN="${STRACE_BIN:-strace}"
OSDI_PROBES="${OSDI_PROBES:-1}"
OSDI_PROBE_GROUP="${OSDI_PROBE_GROUP:-osdi}"
OSDI_PROBE_EVENTS="${OSDI_PROBE_EVENTS:-eval_0,setup_model_0,setup_instance_0}"

RUN_ID="${RUN_ID:-$(date +%s%N)}"
PERF_LOG="${PERF_LOG:-${REPO_ROOT}/artifacts/perf/run_track_hold_sim2_bsim4_elided_300_perf_${RUN_ID}.csv}"
STRACE_LOG="${STRACE_LOG:-${REPO_ROOT}/artifacts/strace/run_track_hold_sim2_bsim4_elided_300_strace_${RUN_ID}.log}"

mkdir -p "$(dirname "${LOG}")" "$(dirname "${RAW}")" "$(dirname "${WRDATA}")" "$(dirname "${PERF_LOG}")" "$(dirname "${STRACE_LOG}")"
if [ -n "${OSDI_LOG:-}" ] && [ "${OSDI_LOG}" != "/dev/null" ]; then
  mkdir -p "$(dirname "${OSDI_LOG}")"
fi

export LC_ALL=C
export LANG=C

# shellcheck source=${SCRIPT_DIR}/osdi_sky130_swap.sh
source "${SCRIPT_DIR}/osdi_sky130_swap.sh"

probe_names=()
cleanup() {
  if [ "${OSDI_PROBES:-0}" != "0" ] && [ "${#probe_names[@]}" -gt 0 ]; then
    if command -v "${PERF_BIN}" >/dev/null 2>&1; then
      for probe_name in "${probe_names[@]}"; do
        "${PERF_BIN}" probe --del "${OSDI_PROBE_GROUP}:${probe_name}" >/dev/null 2>&1 || true
      done
    fi
  fi
  osdi_sky130_swap_restore
}
trap cleanup EXIT

if ! command -v "${PERF_BIN}" >/dev/null 2>&1; then
  echo "perf not found: ${PERF_BIN}" >&2
  exit 1
fi

if ! command -v "${STRACE_BIN}" >/dev/null 2>&1; then
  echo "strace not found: ${STRACE_BIN}" >&2
  exit 1
fi

osdi_sky130_swap_apply

PROBE_ENABLED=0
PERF_PROBE_EVENTS=""

if [ "${OSDI_PROBES}" != "0" ]; then
  probe_ok=1
  IFS=',' read -r -a probe_names <<< "${OSDI_PROBE_EVENTS}"
  for probe_name in "${probe_names[@]}"; do
    if ! "${PERF_BIN}" probe -x "${OSDI_PATH}" --add "${OSDI_PROBE_GROUP}:${probe_name}=${probe_name}" -q -f; then
      probe_ok=0
      break
    fi
  done
  if [ "${probe_ok}" -eq 1 ]; then
    PROBE_ENABLED=1
    if [ "${#probe_names[@]}" -gt 0 ]; then
      PERF_PROBE_EVENTS="${OSDI_PROBE_GROUP}:${probe_names[0]}"
      for probe_name in "${probe_names[@]:1}"; do
        PERF_PROBE_EVENTS="${PERF_PROBE_EVENTS},${OSDI_PROBE_GROUP}:${probe_name}"
      done
    fi
  fi
fi

EVENTS="${PERF_EVENTS}"
if [ "${PROBE_ENABLED}" -eq 1 ] && [ -n "${PERF_PROBE_EVENTS}" ]; then
  EVENTS="${EVENTS},${PERF_PROBE_EVENTS}"
fi

"${PERF_BIN}" stat -x , -e "${EVENTS}" -o "${PERF_LOG}" -- \
  "${STRACE_BIN}" -f -e trace=openat,open -s 0 -o "${STRACE_LOG}" -- \
  "${NGSPICE_BIN}" -b -o "${LOG}" -r "${RAW}" <<EOFNG
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
EOFNG

if command -v rg >/dev/null 2>&1; then
  osdi_count="$(rg -cF -- "${OSDI_PATH}" "${STRACE_LOG}" || true)"
else
  osdi_count="$(grep -cF -- "${OSDI_PATH}" "${STRACE_LOG}" || true)"
fi

osdi_count="${osdi_count:-0}"

{
  echo "OSDI_PATH=${OSDI_PATH}"
  echo "OSDI_INVOCATIONS=${osdi_count}"
  echo "OSDI_FILE_OPENS=${osdi_count}"
  echo "OSDI_PROBES_ENABLED=${PROBE_ENABLED}"
  echo "OSDI_PROBE_GROUP=${OSDI_PROBE_GROUP}"
  echo "OSDI_PROBE_EVENTS=${OSDI_PROBE_EVENTS}"
  echo "OSDI_PROBE_EVENT_LIST=${PERF_PROBE_EVENTS}"
  echo "PERF_LOG=${PERF_LOG}"
  echo "STRACE_LOG=${STRACE_LOG}"
} | tee "${OSDI_LOG:-/dev/null}"
