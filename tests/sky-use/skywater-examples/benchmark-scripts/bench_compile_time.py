#!/usr/bin/env python3
"""
Benchmark OpenVAF compile time on BSIM4.

Compiles integration_tests/BSIM4/bsim4.va TRIALS times (default: 10)
with a given openvaf binary and reports mean/median/stddev timing.

Usage:
    python3 bench_compile_time.py <openvaf-binary> [va-file]

Example — compare baseline vs optimized:
    python3 bench_compile_time.py ~/openvaf-baseline
    python3 bench_compile_time.py ./target/release/openvaf-r
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import statistics
from typing import List


TRIALS = int(os.environ.get("TRIALS", "10"))
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(SCRIPT_DIR, "..", "..", "..", ".."))
OPENVAF_ROOT = os.path.join(REPO_ROOT, "code", "OpenVAF-altered", "OpenVAF")
DEFAULT_VA = os.path.join(OPENVAF_ROOT, "integration_tests", "BSIM4", "bsim4.va")

FINISHED_RE = re.compile(r"Finished.*in\s+([\d.]+)s")


def compile_once(binary: str, va_file: str, output: str) -> float:
    """Compile once and return elapsed seconds reported by openvaf."""
    result = subprocess.run(
        [binary, va_file, "-o", output],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    for line in result.stdout.splitlines():
        m = FINISHED_RE.search(line)
        if m:
            return float(m.group(1))
    sys.stderr.write(f"Could not parse timing from output:\n{result.stdout}\n")
    sys.exit(1)


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
    if len(sys.argv) < 2:
        sys.stderr.write("Usage: bench_compile_time.py <openvaf-binary> [va-file]\n")
        return 1

    binary = sys.argv[1]
    va_file = sys.argv[2] if len(sys.argv) > 2 else DEFAULT_VA
    output = "/tmp/bench_compile.osdi"

    print(f"Binary:  {binary}")
    print(f"Input:   {va_file}")
    print(f"Trials:  {TRIALS}")
    print()

    times: List[float] = []
    for i in range(1, TRIALS + 1):
        t = compile_once(binary, va_file, output)
        times.append(t)
        print(f"[{i:02d}/{TRIALS}] {t:.3f}s")

    print(f"\nSummary: {summarize(times)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
