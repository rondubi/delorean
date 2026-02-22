#!/usr/bin/env bash
# Shared helpers to force SKY130 nfet_01v8_lvt through OSDI for benchmark runs.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"

SKY130_OSDI_SWAP_ENABLE="${SKY130_OSDI_SWAP_ENABLE:-1}"
SKY130_OSDI_MODEL_FILE="${SKY130_OSDI_MODEL_FILE:-${DELOREAN_ROOT}/sky130/sky130/sky130A/libs.ref/sky130_fd_pr/spice/sky130_fd_pr__nfet_01v8_lvt__tt.pm3.spice}"
SKY130_OSDI_MODEL_CARD="${SKY130_OSDI_MODEL_CARD:-.model sky130_fd_pr__nfet_01v8_lvt__osdi bsim4va type=1 l=0.15 w=5 nf=1}"

__sky130_osdi_swap_applied=0
__sky130_osdi_swap_backup=""

osdi_sky130_swap_apply() {
  if [ "${SKY130_OSDI_SWAP_ENABLE}" = "0" ]; then
    return 0
  fi

  if [ ! -f "${SKY130_OSDI_MODEL_FILE}" ]; then
    echo "missing SKY130 model file: ${SKY130_OSDI_MODEL_FILE}" >&2
    return 1
  fi

  if [ ! -w "${SKY130_OSDI_MODEL_FILE}" ]; then
    echo "model file is not writable: ${SKY130_OSDI_MODEL_FILE}" >&2
    echo "run as root/sudo, or set SKY130_OSDI_SWAP_ENABLE=0 to bypass forced OSDI swap" >&2
    return 1
  fi

  __sky130_osdi_swap_backup="${SKY130_OSDI_MODEL_FILE}.bak.osdi_swap.$$"
  cp -f "${SKY130_OSDI_MODEL_FILE}" "${__sky130_osdi_swap_backup}"

  perl -0777 -i -pe 's@^\s*msky130_fd_pr__nfet_01v8_lvt\s+.*$@nsky130_fd_pr__nfet_01v8_lvt d g s b sky130_fd_pr__nfet_01v8_lvt__osdi m = {mult}@m' "${SKY130_OSDI_MODEL_FILE}"
  perl -0777 -i -pe 's@(\.param\s+l\s*=\s*1\s+w\s*=\s*1\s+nf\s*=\s*1\.0\s+ad\s*=\s*0\s+as\s*=\s*0\s+pd\s*=\s*0\s+ps\s*=\s*0\s+nrd\s*=\s*0\s+nrs\s*=\s*0\s+sa\s*=\s*0\s+sb\s*=\s*0\s+sd\s*=\s*0\s+mult\s*=\s*1\s*\n)(?!\.model\s+sky130_fd_pr__nfet_01v8_lvt__osdi\b)@$1'"${SKY130_OSDI_MODEL_CARD}"'\n@ms' "${SKY130_OSDI_MODEL_FILE}"

  __sky130_osdi_swap_applied=1
}

osdi_sky130_swap_restore() {
  if [ "${__sky130_osdi_swap_applied}" != "1" ]; then
    return 0
  fi

  if [ -n "${__sky130_osdi_swap_backup}" ] && [ -f "${__sky130_osdi_swap_backup}" ]; then
    mv -f "${__sky130_osdi_swap_backup}" "${SKY130_OSDI_MODEL_FILE}"
  fi

  __sky130_osdi_swap_applied=0
  __sky130_osdi_swap_backup=""
}
