#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
export DELOREAN_ROOT="${DELOREAN_ROOT:-${REPO_ROOT}}"


OPENVAF_BIN="${OPENVAF_BIN:-${REPO_ROOT}/code/OpenVAF-altered/OpenVAF/target/debug/openvaf-r}"
VA_FILE="${VA_FILE:-${REPO_ROOT}/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.va}"
ELISION_DIR="${ELISION_DIR:-${REPO_ROOT}/artifacts/sky130_bin_elision_lists}"
OUT_DIR="${OUT_DIR:-/artifacts/osdi/pfet_01v8_bins}"
LOG_DIR="${LOG_DIR:-/artifacts/logs/osdi_build_bins}"

mkdir -p "${OUT_DIR}" "${LOG_DIR}"

SUMMARY_CSV="${OUT_DIR}/build_summary.csv"
echo "bin,elision_file,osdi_file,status,duration_sec,size_bytes,log_file" > "${SUMMARY_CSV}"

if [ ! -x "${OPENVAF_BIN}" ]; then
  echo "Missing executable OPENVAF_BIN: ${OPENVAF_BIN}" >&2
  exit 1
fi

if [ ! -f "${VA_FILE}" ]; then
  echo "Missing VA_FILE: ${VA_FILE}" >&2
  exit 1
fi

mapfile -t ELISION_FILES < <(find "${ELISION_DIR}" -maxdepth 1 -type f -name '*_bin_*.txt' | sort)
if [ "${#ELISION_FILES[@]}" -eq 0 ]; then
  echo "No elision files found in ${ELISION_DIR}" >&2
  exit 1
fi

success=0
fail=0

for elision_file in "${ELISION_FILES[@]}"; do
  fname="$(basename "${elision_file}")"
  if [[ "${fname}" =~ _bin_([0-9]{3})\.txt$ ]]; then
    bin="${BASH_REMATCH[1]}"
  else
    echo "Skipping file with unexpected name format: ${fname}" >&2
    continue
  fi

  osdi_file="${OUT_DIR}/bsim4_bin_${bin}.osdi"
  log_file="${LOG_DIR}/build_bin_${bin}.log"

  start_s="$(date +%s)"
  set +e
  "${OPENVAF_BIN}" "${VA_FILE}" --elision-file "${elision_file}" -o "${osdi_file}" >"${log_file}" 2>&1
  rc=$?
  set -e
  end_s="$(date +%s)"
  dur="$((end_s - start_s))"

  if [ "${rc}" -eq 0 ] && [ -f "${osdi_file}" ]; then
    status="ok"
    size_bytes="$(stat -c '%s' "${osdi_file}")"
    success=$((success + 1))
  else
    status="fail"
    size_bytes="0"
    fail=$((fail + 1))
  fi

  echo "${bin},${fname},$(basename "${osdi_file}"),${status},${dur},${size_bytes},$(basename "${log_file}")" >> "${SUMMARY_CSV}"
  printf '[%s] bin=%s status=%s duration=%ss\n' "$(date '+%H:%M:%S')" "${bin}" "${status}" "${dur}"
done

echo
echo "Built OSDI models from ${#ELISION_FILES[@]} elision files."
echo "Success: ${success}"
echo "Failed: ${fail}"
echo "Output dir: ${OUT_DIR}"
echo "Summary: ${SUMMARY_CSV}"
