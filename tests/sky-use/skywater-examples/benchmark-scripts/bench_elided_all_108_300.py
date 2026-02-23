#!/usr/bin/env python3
from __future__ import annotations

import csv
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
TRACK_SCRIPT = ROOT / "tests/sky-use/skywater-examples/benchmark-scripts/bench_track_hold_osdi_300.py"
SWEEP_SCRIPT = ROOT / "tests/sky-use/skywater-examples/benchmark-scripts/bench_vgs_sweep_osdi_300.py"
ELIDED_LINK = ROOT / "tests/sky-use/skywater-examples/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.elided.osdi"
BIN_DIR = ROOT / "artifacts/osdi/pfet_01v8_bins"
RESULTS_DIR = ROOT / "artifacts/results"
RAW_CSV = RESULTS_DIR / "02-22-26-elide-per-bin-raw.csv"
OUT_MD = RESULTS_DIR / "02-22-26-elide-per-bin.md"

TRACK_RE = re.compile(r"\[\d+/\d+\]\s+(sim[12]_bsim4(?:_elided)?_300)\s+([0-9]+\.[0-9]+)s")
SWEEP_RE = re.compile(r"\[\d+/\d+\]\s+(vgs_bsim4(?:_elided)?_300)\s+([0-9]+\.[0-9]+)s")


def run_script(path: Path, env: dict[str, str]) -> str:
    proc = subprocess.run(
        [sys.executable, str(path)],
        cwd=str(ROOT),
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if proc.returncode != 0:
        print(proc.stdout)
        raise RuntimeError(f"run failed: {path} rc={proc.returncode}")
    return proc.stdout


def stats(xs: list[float]) -> dict[str, float | int]:
    q1, _, q3 = statistics.quantiles(xs, n=4, method="inclusive")
    return {
        "n": len(xs),
        "mean": statistics.mean(xs),
        "median": statistics.median(xs),
        "q1": q1,
        "q3": q3,
        "min": min(xs),
        "max": max(xs),
    }


def main() -> int:
    bins = sorted(BIN_DIR.glob("bsim4_bin_*.osdi"))
    if len(bins) != 108:
        raise RuntimeError(f"expected 108 bin files in {BIN_DIR}, found {len(bins)}")

    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    ELIDED_LINK.parent.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env["TRIALS"] = "1"
    env["NGSPICE_BIN"] = "/home/ron/opt/ngspice/bin/ngspice"

    track_unelided: list[float] = []
    track_elided: list[float] = []
    sweep_unelided: list[float] = []
    sweep_elided: list[float] = []

    with RAW_CSV.open("w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow([
            "bin",
            "sim1_unelided_s",
            "sim2_unelided_s",
            "sim1_elided_s",
            "sim2_elided_s",
            "sweep_unelided_s",
            "sweep_elided_s",
        ])

        for i, bin_file in enumerate(bins, 1):
            bin_id = bin_file.stem.split("_")[-1]
            if ELIDED_LINK.exists() or ELIDED_LINK.is_symlink():
                ELIDED_LINK.unlink()
            os.symlink(bin_file, ELIDED_LINK)

            t0 = time.monotonic()
            track_out = run_script(TRACK_SCRIPT, env)
            sweep_out = run_script(SWEEP_SCRIPT, env)

            tvals = {name: float(sec) for name, sec in TRACK_RE.findall(track_out)}
            svals = {name: float(sec) for name, sec in SWEEP_RE.findall(sweep_out)}

            needed_t = ["sim1_bsim4_300", "sim2_bsim4_300", "sim1_bsim4_elided_300", "sim2_bsim4_elided_300"]
            needed_s = ["vgs_bsim4_300", "vgs_bsim4_elided_300"]
            if any(k not in tvals for k in needed_t) or any(k not in svals for k in needed_s):
                raise RuntimeError(f"parse failure on bin {bin_id}")

            track_unelided.extend([tvals["sim1_bsim4_300"], tvals["sim2_bsim4_300"]])
            track_elided.extend([tvals["sim1_bsim4_elided_300"], tvals["sim2_bsim4_elided_300"]])
            sweep_unelided.append(svals["vgs_bsim4_300"])
            sweep_elided.append(svals["vgs_bsim4_elided_300"])

            writer.writerow([
                bin_id,
                f"{tvals['sim1_bsim4_300']:.6f}",
                f"{tvals['sim2_bsim4_300']:.6f}",
                f"{tvals['sim1_bsim4_elided_300']:.6f}",
                f"{tvals['sim2_bsim4_elided_300']:.6f}",
                f"{svals['vgs_bsim4_300']:.6f}",
                f"{svals['vgs_bsim4_elided_300']:.6f}",
            ])

            dt = time.monotonic() - t0
            print(f"[{i:03d}/108] bin={bin_id} elapsed={dt:.1f}s")
            sys.stdout.flush()

    tu = stats(track_unelided)
    te = stats(track_elided)
    su = stats(sweep_unelided)
    se = stats(sweep_elided)

    OUT_MD.write_text(
        "\n".join([
            "# 2026-02-22 Elision Per-Bin Runtime Stats (300-Variant Decks)",
            "",
            "## Run Setup",
            "- Elided OSDIs: all 108 bin-specific files in `artifacts/osdi/pfet_01v8_bins/bsim4_bin_*.osdi`.",
            "- Per-bin tie-in: `bsim4.elided.osdi` symlink relinked to each bin OSDI before each benchmark pair.",
            "- Benchmarks run per bin (`TRIALS=1` each):",
            "  - `bench_track_hold_osdi_300.py`",
            "  - `bench_vgs_sweep_osdi_300.py`",
            "- Total bins: 108",
            "- Raw per-bin timings: `artifacts/results/02-22-26-elide-per-bin-raw.csv`.",
            "",
            "## Runtime Statistics (seconds)",
            "",
            "| Benchmark | Variation | n | Mean | Median | Q1 | Q3 | Min | Max |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
            f"| track_and_hold | unelided | {tu['n']} | {tu['mean']:.4f} | {tu['median']:.4f} | {tu['q1']:.4f} | {tu['q3']:.4f} | {tu['min']:.4f} | {tu['max']:.4f} |",
            f"| track_and_hold | elided | {te['n']} | {te['mean']:.4f} | {te['median']:.4f} | {te['q1']:.4f} | {te['q3']:.4f} | {te['min']:.4f} | {te['max']:.4f} |",
            f"| sweep | unelided | {su['n']} | {su['mean']:.4f} | {su['median']:.4f} | {su['q1']:.4f} | {su['q3']:.4f} | {su['min']:.4f} | {su['max']:.4f} |",
            f"| sweep | elided | {se['n']} | {se['mean']:.4f} | {se['median']:.4f} | {se['q1']:.4f} | {se['q3']:.4f} | {se['min']:.4f} | {se['max']:.4f} |",
            "",
            "## Notes",
            "- `track_and_hold` uses sim1+sim2 timings from each run.",
            "- Quartiles are inclusive quartiles over collected samples.",
        ]) + "\n",
        encoding="utf-8",
    )

    print(f"wrote {RAW_CSV}")
    print(f"wrote {OUT_MD}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
