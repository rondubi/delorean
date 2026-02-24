# Shared-Parameter Elision Benchmark Report (2026-02-23)

## Artifacts
- Shared elision list: `/home/ron/delorean/artifacts/sky130_bin_elision_lists/sky130_fd_pr__pfet_01v8__model_shared_intersection.txt`
- Compiled OSDI: `/home/ron/delorean/artifacts/osdi/bsim4_shared_intersection.osdi` (463264 bytes)
- Track/hold benchmark log: `/home/ron/delorean/artifacts/logs/shared-elision/2026-02-23-shared-elision-bench-track_hold.txt`
- Sweep benchmark log: `/home/ron/delorean/artifacts/logs/shared-elision/2026-02-23-shared-elision-bench-sweep.txt`
- OSDI build log: `/home/ron/delorean/artifacts/logs/shared-elision/2026-02-23-shared-elision-osdi-build.log`

## Shared Elision List
- Shared parameter count across all bin lists: **182**
- Method: strict intersection across all `*_bin_*.txt` files (exact `key = value` lines).

## Detailed Stats (TRIALS=3)
| Benchmark | Variant | n | mean (s) | median (s) | stddev (s) | p25 (s) | p75 (s) | min (s) | max (s) |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| track_hold_sim1 | unelided | 3 | 12.662 | 12.674 | 0.030 | 12.629 | 12.684 | 12.629 | 12.684 |
| track_hold_sim1 | shared-elided | 3 | 12.610 | 12.633 | 0.073 | 12.529 | 12.668 | 12.529 | 12.668 |
| track_hold_sim2 | unelided | 3 | 22.128 | 22.131 | 0.171 | 21.956 | 22.298 | 21.956 | 22.298 |
| track_hold_sim2 | shared-elided | 3 | 22.131 | 22.052 | 0.150 | 22.037 | 22.304 | 22.037 | 22.304 |
| vgs_sweep | unelided | 3 | 9.002 | 8.960 | 0.162 | 8.865 | 9.182 | 8.865 | 9.182 |
| vgs_sweep | shared-elided | 3 | 9.032 | 9.045 | 0.080 | 8.947 | 9.105 | 8.947 | 9.105 |

## Mean Delta Summary
| Benchmark | Unelided mean (s) | Shared-elided mean (s) | Delta (s) | Delta (%) |
|---|---:|---:|---:|---:|
| track_hold_sim1 | 12.662 | 12.610 | -0.052 | -0.41% |
| track_hold_sim2 | 22.128 | 22.131 | +0.003 | +0.01% |
| vgs_sweep | 9.002 | 9.032 | +0.030 | +0.33% |
