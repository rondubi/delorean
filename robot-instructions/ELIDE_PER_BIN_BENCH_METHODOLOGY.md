# Elide Per-Bin Benchmark Methodology

1. Keep all 108 bin-elided OSDI artifacts available at `artifacts/osdi/pfet_01v8_bins/bsim4_bin_*.osdi`.
2. For each benchmark sweep, choose the bin-elided OSDI matching the benchmarked device/bin region.
3. Relink `tests/sky-use/skywater-examples/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.elided.osdi` to that bin OSDI.
4. Run benchmark script (`TRIALS=5`) and collect elapsed times from `[i/N] ... <seconds>s` lines.
5. Compute mean, median, Q1, Q3, min, max for elided and unelided.
6. Write report to `artifacts/results/02-22-26-elide-per-bin.md`.
