# 2026-02-22 Elision Per-Bin Runtime Stats (300-Variant Decks)

## Run Setup
- Used bin-elided OSDI artifacts from `artifacts/osdi/pfet_01v8_bins/bsim4_bin_*.osdi`.
- Single-sweep bin-matched method (not 108 full sweeps):
  - `track_and_hold` bound to `bsim4_bin_026.osdi`
  - `sweep` bound to `bsim4_bin_035.osdi`
- Benchmark scripts and trials:
  - `tests/sky-use/skywater-examples/benchmark-scripts/bench_track_hold_osdi_300.py` with `TRIALS=5`
  - `tests/sky-use/skywater-examples/benchmark-scripts/bench_vgs_sweep_osdi_300.py` with `TRIALS=5`
- Raw outputs:
  - `/tmp/bench_track_hold_osdi_300_single_sweep_bin026_trials5_seq.txt`
  - `/tmp/bench_vgs_sweep_osdi_300_single_sweep_bin035_trials5_seq.txt`

## Runtime Statistics (seconds)

| Benchmark | Variation | n | Mean | Median | Q1 | Q3 | Min | Max |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| track_and_hold | unelided | 10 | 17.4558 | 17.5615 | 12.6132 | 22.2068 | 12.4540 | 22.6180 |
| track_and_hold | elided | 10 | 17.4583 | 17.5195 | 12.8120 | 22.1067 | 12.5230 | 22.2130 |
| sweep | unelided | 5 | 8.9432 | 8.8660 | 8.8500 | 8.9380 | 8.8340 | 9.2280 |
| sweep | elided | 5 | 9.0748 | 9.0000 | 8.9190 | 9.2990 | 8.8550 | 9.3010 |

## Notes
- `track_and_hold` aggregates sim1+sim2 timings from each run.
- Quartiles are inclusive quartiles.
- This run uses only bins matched to the benchmarked sweep/device setup, not all 108 bins across every sweep.
