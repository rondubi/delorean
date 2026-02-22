#!/usr/bin/env python3
"""
Benchmark the SKY130 VGS sweep deck with both BSIM4 OSDI variants.
"""

from __future__ import annotations

import os
import random
import statistics
import subprocess
import sys
import time
from typing import Dict, List


TRIALS = int(os.environ.get("TRIALS", "3"))
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT_DIR = os.path.dirname(SCRIPT_DIR)
RUN_SCRIPTS_DIR = os.path.join(ROOT_DIR, "run-scripts")

SCRIPTS: Dict[str, str] = {
    "vgs_bsim4": f"{RUN_SCRIPTS_DIR}/run_vgs_sweep_bsim4.sh",
    "vgs_bsim4_elided": f"{RUN_SCRIPTS_DIR}/run_vgs_sweep_bsim4_elided.sh",
}


def run_once(name: str, cmd: str) -> float:
    """Run the given script once and return elapsed seconds."""
    start = time.monotonic()
    completed = subprocess.run(
        [cmd],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    elapsed = time.monotonic() - start
    if completed.returncode != 0:
        sys.stderr.write(f"{name} failed with exit code {completed.returncode}\n")
        if completed.stdout:
            sys.stderr.buffer.write(completed.stdout)
        if completed.stderr:
            sys.stderr.buffer.write(completed.stderr)
        sys.exit(1)
    return elapsed


def summarize(values: List[float]) -> str:
    mean = statistics.mean(values)
    median = statistics.median(values)
    stdev = statistics.stdev(values) if len(values) > 1 else 0.0
    sorted_vals = sorted(values)
    q1 = statistics.quantiles(sorted_vals, n=4)[0] if len(values) > 1 else median
    q3 = statistics.quantiles(sorted_vals, n=4)[2] if len(values) > 1 else median
    return (
        f"n={len(values)} "
        f"mean={mean:.3f}s "
        f"median={median:.3f}s "
        f"stddev={stdev:.3f}s "
        f"p25={q1:.3f}s "
        f"p75={q3:.3f}s "
        f"min={min(values):.3f}s "
        f"max={max(values):.3f}s"
    )


def main() -> int:
    if TRIALS < 1:
        sys.stderr.write("TRIALS must be >= 1\n")
        return 1

    order = [name for name in SCRIPTS for _ in range(TRIALS)]
    random.shuffle(order)

    results: Dict[str, List[float]] = {name: [] for name in SCRIPTS}

    total_runs = len(order)
    print(f"Running {TRIALS} trials per script ({total_runs} total) in random order...\n")

    for idx, name in enumerate(order, 1):
        cmd = SCRIPTS[name]
        elapsed = run_once(name, cmd)
        results[name].append(elapsed)
        print(f"[{idx:02d}/{total_runs}] {name:17s} {elapsed:.3f}s")

    print("\nSummary:")
    for name, values in results.items():
        print(f"- {name:17s} {summarize(values)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
