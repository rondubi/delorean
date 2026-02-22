#!/usr/bin/env bash
# Shared helpers to force SKY130 PMOS VGS sweep devices through OSDI.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"

SKY130_OSDI_SWAP_ENABLE="${SKY130_OSDI_SWAP_ENABLE:-1}"

SKY130_PFET_SVT_MODEL_FILE="${SKY130_PFET_SVT_MODEL_FILE:-${DELOREAN_ROOT}/sky130/sky130/sky130A/libs.ref/sky130_fd_pr/spice/sky130_fd_pr__pfet_01v8__tt.pm3.spice}"
SKY130_PFET_LVT_MODEL_FILE="${SKY130_PFET_LVT_MODEL_FILE:-${DELOREAN_ROOT}/sky130/sky130/sky130A/libs.ref/sky130_fd_pr/spice/sky130_fd_pr__pfet_01v8_lvt__tt.pm3.spice}"
SKY130_PFET_HVT_MODEL_FILE="${SKY130_PFET_HVT_MODEL_FILE:-${DELOREAN_ROOT}/sky130/sky130/sky130A/libs.ref/sky130_fd_pr/spice/sky130_fd_pr__pfet_01v8_hvt__tt.pm3.spice}"

__sky130_osdi_swap_applied=0
__sky130_osdi_swap_backups=()

_osdi_backup_and_patch() {
  local model_file="$1"
  local m_instance="$2"
  local n_instance="$3"
  local model_name="$4"
  local model_card="$5"

  if [ ! -f "${model_file}" ]; then
    echo "missing SKY130 model file: ${model_file}" >&2
    return 1
  fi

  if [ ! -w "${model_file}" ]; then
    echo "model file is not writable: ${model_file}" >&2
    echo "run as root/sudo, or set SKY130_OSDI_SWAP_ENABLE=0 to bypass forced OSDI swap" >&2
    return 1
  fi

  local backup_file="${model_file}.bak.osdi_swap.$$"
  cp -f "${model_file}" "${backup_file}"
  __sky130_osdi_swap_backups+=("${model_file}:${backup_file}")

  # Bind the model card directly to the exact instance line we rewrite so it
  # lands in the correct subckt, even when files contain multiple subckts.
  perl -0777 -i -pe "s@^\\s*${m_instance}\\s+.*\$@${n_instance} d g s b ${model_name} m = {mult}\\n${model_card}@m" "${model_file}"
}

osdi_sky130_swap_apply() {
  if [ "${SKY130_OSDI_SWAP_ENABLE}" = "0" ]; then
    return 0
  fi

  _osdi_backup_and_patch \
    "${SKY130_PFET_LVT_MODEL_FILE}" \
    "msky130_fd_pr__pfet_01v8_lvt" \
    "nsky130_fd_pr__pfet_01v8_lvt" \
    "sky130_fd_pr__pfet_01v8_lvt__osdi" \
    ".model sky130_fd_pr__pfet_01v8_lvt__osdi bsim4va type=-1 l=0.35 w=4 nf=1"

  _osdi_backup_and_patch \
    "${SKY130_PFET_SVT_MODEL_FILE}" \
    "msky130_fd_pr__pfet_01v8" \
    "nsky130_fd_pr__pfet_01v8" \
    "sky130_fd_pr__pfet_01v8__osdi" \
    ".model sky130_fd_pr__pfet_01v8__osdi bsim4va type=-1 l=0.15 w=2 nf=1"

  _osdi_backup_and_patch \
    "${SKY130_PFET_HVT_MODEL_FILE}" \
    "msky130_fd_pr__pfet_01v8_hvt" \
    "nsky130_fd_pr__pfet_01v8_hvt" \
    "sky130_fd_pr__pfet_01v8_hvt__osdi" \
    ".model sky130_fd_pr__pfet_01v8_hvt__osdi bsim4va type=-1 l=0.15 w=2 nf=1"

  __sky130_osdi_swap_applied=1
}

osdi_sky130_swap_restore() {
  if [ "${__sky130_osdi_swap_applied}" != "1" ]; then
    return 0
  fi

  local pair model_file backup_file
  for pair in "${__sky130_osdi_swap_backups[@]}"; do
    model_file="${pair%%:*}"
    backup_file="${pair#*:}"
    if [ -f "${backup_file}" ]; then
      mv -f "${backup_file}" "${model_file}"
    fi
  done

  __sky130_osdi_swap_applied=0
  __sky130_osdi_swap_backups=()
}
