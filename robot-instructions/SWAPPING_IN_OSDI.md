# Swapping SKY130 NMOS to OSDI (Working Procedure)

This document records the **working** method to force the `track_hold` SKY130 NMOS path to execute the OpenVAF OSDI BSIM4 model and verify it with `perf` uprobes.

## What failed (and why)

These did **not** work:

- Changing `.model ... nmos` to `.model ... bsim4va` while keeping `M...` transistor instances.
- Changing only one bin (`.model.<N>`) to `bsim4va`.
- Changing all bins to `bsim4va` while keeping original sky130 subckt internals.

Reason:

- ngspice OSDI MOS usage for this plugin needs an `N...` OSDI-style device path, not the default SKY130 `M...` + binning path.
- SKY130’s internal bin/model indirection does not automatically map to OSDI model lookup for this case.

## Working substitution

The successful forced substitution for `track_hold_sim1_300.spice` was:

1. Edit this file temporarily:
   - `/home/ron/open_pdks/sources/sky130_fd_pr/cells/nfet_01v8_lvt/sky130_fd_pr__nfet_01v8_lvt__tt.pm3.spice`
2. Inside subckt `sky130_fd_pr__nfet_01v8_lvt`, replace internal MOS call:
   - From: `msky130_fd_pr__nfet_01v8_lvt ... sky130_fd_pr__nfet_01v8_lvt__model ...`
   - To: `nsky130_fd_pr__nfet_01v8_lvt d g s b sky130_fd_pr__nfet_01v8_lvt__osdi m={mult}`
3. Add a dedicated OSDI model card in that subckt:
   - `.model sky130_fd_pr__nfet_01v8_lvt__osdi bsim4va type=1 l=0.15 w=5 nf=1`
4. Run with OSDI loaded (`pre_osdi .../bsim4.osdi`) and with perf probes enabled.
5. Restore the original file.

Important:

- This working substitution is **dimension-specific** (`l=0.15`, `w=5`) and validated for the track/hold transistor.
- This is intentionally a temporary test override, not a production PDK edit.

## Verified successful evidence

From a successful run:

- ngspice log showed `OSDI(debug)` lines for the substituted device.
- Perf CSV had non-zero probes:
  - `osdi:eval_0 = 53846`
  - `osdi:setup_model_0 = 2`
  - `osdi:setup_instance_0 = 2`

## Repro script pattern (recommended)

Use an automated script with:

- `cp model_file model_file.bak`
- `trap` cleanup that always restores backup and deletes probes
- `perf probe --add osdi:...` before run
- run script with `STRACE_BIN` replaced by a no-op wrapper (avoid ptrace restrictions)
- parse `run_track_hold_sim1_bsim4_300_perf_*.csv`
- cleanup via `trap`

## Hardened script (ready to run)

Use this script:

- `/home/ron/swap_in_osdi_track_hold.sh`

What it does:

1. Verifies required files and root privileges.
2. Backs up:
   - `/home/ron/open_pdks/sources/sky130_fd_pr/cells/nfet_01v8_lvt/sky130_fd_pr__nfet_01v8_lvt__tt.pm3.spice`
3. Rewrites that file for OSDI usage:
   - internal device call `M...` -> `N...`
   - inserts `.model ... bsim4va type=1 l=0.15 w=5 nf=1`
4. Adds perf probes:
   - `osdi:eval_0`
   - `osdi:setup_model_0`
   - `osdi:setup_instance_0`
5. Runs:
   - `/home/ron/sky-use/skywater-examples/run_track_hold_sim1_bsim4_300_perf.sh`
   - with `STRACE_BIN=/tmp/nostrace`
6. Prints:
   - tail of run log
   - `osdi:*` lines from newest `run_track_hold_sim1_bsim4_300_perf_*.csv`
   - ngspice OSDI/debug/error markers
7. Always restores original model file and removes probes on exit (success or failure).

Invoke:

```bash
sudo /home/ron/swap_in_osdi_track_hold.sh
```

Notes:

- Script expects `perf probe` to be permitted (e.g. `perf_event_paranoid` configured appropriately).
- `OSDI_INVOCATIONS`/`OSDI_FILE_OPENS` from wrapper logs are expected to be `0` in this mode because strace is intentionally bypassed.
- Trust probe counts in perf CSV (`osdi:*`) and ngspice `OSDI(debug)` log markers.

## One-shot command sequence (manual)

Run as root/sudo.

```bash
model_file="/home/ron/open_pdks/sources/sky130_fd_pr/cells/nfet_01v8_lvt/sky130_fd_pr__nfet_01v8_lvt__tt.pm3.spice"
cp "$model_file" "${model_file}.bak"

perl -0777 -i -pe 's@^\s*msky130_fd_pr__nfet_01v8_lvt\s+.*$@nsky130_fd_pr__nfet_01v8_lvt d g s b sky130_fd_pr__nfet_01v8_lvt__osdi m = {mult}@m' "$model_file"
perl -0777 -i -pe 's@(\.param\s+l\s*=\s*1\s+w\s*=\s*1\s+nf\s*=\s*1\.0\s+ad\s*=\s*0\s+as\s*=\s*0\s+pd\s*=\s*0\s+ps\s*=\s*0\s+nrd\s*=\s*0\s+nrs\s*=\s*0\s+sa\s*=\s*0\s+sb\s*=\s*0\s+sd\s*=\s*0\s+mult\s*=\s*1\n)@$1.model sky130_fd_pr__nfet_01v8_lvt__osdi bsim4va type=1 l=0.15 w=5 nf=1\n@ms' "$model_file"

perf probe -x /home/ron/CS191W/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.osdi --add osdi:eval_0=eval_0 -f
perf probe -x /home/ron/CS191W/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.osdi --add osdi:setup_model_0=setup_model_0 -f
perf probe -x /home/ron/CS191W/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.osdi --add osdi:setup_instance_0=setup_instance_0 -f

STRACE_BIN=/tmp/nostrace OSDI_PROBES=1 /home/ron/sky-use/skywater-examples/run_track_hold_sim1_bsim4_300_perf.sh
```

Restore immediately after test:

```bash
mv -f "${model_file}.bak" "$model_file"
perf probe --del osdi:eval_0 || true
perf probe --del osdi:setup_model_0 || true
perf probe --del osdi:setup_instance_0 || true
```

## For future Codex runs

Ask Codex to:

1. Back up `sky130_fd_pr__nfet_01v8_lvt__tt.pm3.spice`.
2. Apply the `M -> N` subckt-internal swap and add `.model ... bsim4va` with fixed dimensions.
3. Run `run_track_hold_sim1_bsim4_300_perf.sh` with perf OSDI probes and no strace.
4. Report `osdi:*` counts from the newest `run_track_hold_sim1_bsim4_300_perf_*.csv`.
5. Restore original PDK file and remove probes.
