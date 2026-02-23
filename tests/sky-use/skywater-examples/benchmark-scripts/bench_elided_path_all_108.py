#!/usr/bin/env python3
from __future__ import annotations

import csv
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
RUN = ROOT / "tests/sky-use/skywater-examples/run-scripts"
ELIDED_LINK = ROOT / "tests/sky-use/skywater-examples/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.elided.osdi"
BIN_DIR = ROOT / "artifacts/osdi/pfet_01v8_bins"
RESULTS = ROOT / "artifacts/results"
RAW_ELIDED = RESULTS / "02-22-26-elide-per-bin-raw-elided.csv"
RAW_UNELIDED = RESULTS / "02-22-26-elide-per-bin-raw-unelided.csv"
OUT_MD = RESULTS / "02-22-26-elide-per-bin.md"
METH_MD = ROOT / "robot_instructions/ELIDE_PER_BIN_BENCH_METHODOLOGY.md"

SIM1_U = RUN / "run_track_hold_sim1_bsim4_300.sh"
SIM2_U = RUN / "run_track_hold_sim2_bsim4_300.sh"
SWEEP_U = RUN / "run_vgs_sweep_bsim4_300.sh"
SIM1_E = RUN / "run_track_hold_sim1_bsim4_elided_300.sh"
SIM2_E = RUN / "run_track_hold_sim2_bsim4_elided_300.sh"
SWEEP_E = RUN / "run_vgs_sweep_bsim4_elided_300.sh"


def run_timed(cmd: Path, env: dict[str, str]) -> float:
    t0 = time.monotonic()
    p = subprocess.run([str(cmd)], cwd=str(ROOT), env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    dt = time.monotonic() - t0
    if p.returncode != 0:
        print(p.stdout)
        raise RuntimeError(f"failed: {cmd}")
    return dt


def qstats(xs: list[float]) -> dict[str, float | int]:
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
        raise RuntimeError(f"expected 108 bins, found {len(bins)}")

    env = os.environ.copy()
    env["NGSPICE_BIN"] = "/home/ron/opt/ngspice/bin/ngspice"

    RESULTS.mkdir(parents=True, exist_ok=True)
    ELIDED_LINK.parent.mkdir(parents=True, exist_ok=True)

    # Unelided baseline (independent of bin elision files)
    baseline_trials = 10
    sim1_u_vals: list[float] = []
    sim2_u_vals: list[float] = []
    sweep_u_vals: list[float] = []
    with RAW_UNELIDED.open("w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["trial", "sim1_unelided_s", "sim2_unelided_s", "sweep_unelided_s"])
        for i in range(1, baseline_trials + 1):
            a = run_timed(SIM1_U, env)
            b = run_timed(SIM2_U, env)
            c = run_timed(SWEEP_U, env)
            sim1_u_vals.append(a)
            sim2_u_vals.append(b)
            sweep_u_vals.append(c)
            w.writerow([i, f"{a:.6f}", f"{b:.6f}", f"{c:.6f}"])
            print(f"[baseline {i:02d}/{baseline_trials}] done")
            sys.stdout.flush()

    # Elided all-108 bin sweep
    sim1_e_vals: list[float] = []
    sim2_e_vals: list[float] = []
    sweep_e_vals: list[float] = []
    with RAW_ELIDED.open("w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["bin", "sim1_elided_s", "sim2_elided_s", "sweep_elided_s"])
        for i, bin_file in enumerate(bins, 1):
            bin_id = bin_file.stem.split("_")[-1]
            if ELIDED_LINK.exists() or ELIDED_LINK.is_symlink():
                ELIDED_LINK.unlink()
            os.symlink(bin_file, ELIDED_LINK)

            a = run_timed(SIM1_E, env)
            b = run_timed(SIM2_E, env)
            c = run_timed(SWEEP_E, env)
            sim1_e_vals.append(a)
            sim2_e_vals.append(b)
            sweep_e_vals.append(c)
            w.writerow([bin_id, f"{a:.6f}", f"{b:.6f}", f"{c:.6f}"])
            print(f"[elided {i:03d}/108] bin={bin_id} done")
            sys.stdout.flush()

    track_u = sim1_u_vals + sim2_u_vals
    track_e = sim1_e_vals + sim2_e_vals

    tu, te = qstats(track_u), qstats(track_e)
    su, se = qstats(sweep_u_vals), qstats(sweep_e_vals)

    OUT_MD.write_text(
        "\n".join([
            "# 2026-02-22 Elision Per-Bin Runtime Stats (300-Variant Decks)",
            "",
            "## Run Setup",
            "- Elided path was run across all 108 bin-specific OSDIs: `artifacts/osdi/pfet_01v8_bins/bsim4_bin_*.osdi`.",
            "- For each bin, `bsim4.elided.osdi` was relinked to that bin file before running elided scripts.",
            "- Elided scripts per bin:",
            "  - `run_track_hold_sim1_bsim4_elided_300.sh`",
            "  - `run_track_hold_sim2_bsim4_elided_300.sh`",
            "  - `run_vgs_sweep_bsim4_elided_300.sh`",
            "- Unelided baseline: 10 trials each of:",
            "  - `run_track_hold_sim1_bsim4_300.sh`",
            "  - `run_track_hold_sim2_bsim4_300.sh`",
            "  - `run_vgs_sweep_bsim4_300.sh`",
            "- Raw data:",
            "  - `artifacts/results/02-22-26-elide-per-bin-raw-elided.csv`",
            "  - `artifacts/results/02-22-26-elide-per-bin-raw-unelided.csv`",
            "",
            "## Runtime Statistics (seconds)",
            "",
            "| Benchmark | Variation | n | Mean | Median | Q1 | Q3 | Min | Max |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
            f"| track_and_hold | unelided | {tu['n']} | {tu['mean']:.4f} | {tu['median']:.4f} | {tu['q1']:.4f} | {tu['q3']:.4f} | {tu['min']:.4f} | {tu['max']:.4f} |",
            f"| track_and_hold | elided | {te['n']} | {te['mean']:.4f} | {te['median']:.4f} | {te['q1']:.4f} | {te['q3']:.4f} | {te['min']:.4f} | {te['max']:.4f} |",
            f"| sweep | unelided | {su['n']} | {su['mean']:.4f} | {su['median']:.4f} | {su['q1']:.4f} | {su['q3']:.4f} | {su['min']:.4f} | {su['max']:.4f} |",
            f"| sweep | elided | {se['n']} | {se['mean']:.4f} | {se['median']:.4f} | {se['q1']:.4f} | {se['q3']:.4f} | {se['min']:.4f} | {se['max']:.4f} |",
        ]) + "\n",
        encoding="utf-8",
    )

    METH_MD.write_text(
        "\n".join([
            "# Elide Per-Bin Benchmark Methodology",
            "",
            "1. Build/collect 108 bin-specific elided OSDIs at `artifacts/osdi/pfet_01v8_bins/bsim4_bin_*.osdi`.",
            "2. For each bin file:",
            "- relink `tests/sky-use/skywater-examples/code/OpenVAF-altered/OpenVAF/integration_tests/BSIM4/bsim4.elided.osdi` to that bin file",
            "- run `run_track_hold_sim1_bsim4_elided_300.sh`",
            "- run `run_track_hold_sim2_bsim4_elided_300.sh`",
            "- run `run_vgs_sweep_bsim4_elided_300.sh`",
            "3. Collect elapsed wall-clock seconds for each run.",
            "4. Collect unelided baseline via repeated runs of the non-elided 300 scripts.",
            "5. Aggregate stats (mean, median, Q1, Q3, min, max) for elided vs unelided.",
            "6. Write report to `artifacts/results/02-22-26-elide-per-bin.md`.",
        ]) + "\n",
        encoding="utf-8",
    )

    print(f"wrote {OUT_MD}")
    print(f"wrote {METH_MD}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
