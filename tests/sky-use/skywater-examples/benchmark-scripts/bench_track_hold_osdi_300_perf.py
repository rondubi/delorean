#!/usr/bin/env python3
"""
Benchmark the SKY130 track-and-hold simulations (sim1 and sim2) with perf + strace.

Collects perf stat counters, counts OSDI file opens from strace, and counts
OSDI runtime calls via perf uprobe events (eval/setup).
"""

from __future__ import annotations

import os
import random
import statistics
import subprocess
import sys
import time
from typing import Dict, List, Tuple


TRIALS = int(os.environ.get("TRIALS", "3"))
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT_DIR = os.path.dirname(SCRIPT_DIR)
RUN_SCRIPTS_DIR = os.path.join(ROOT_DIR, "run-scripts")
ARTIFACTS_DIR = os.path.join(ROOT_DIR, "artifacts")
PERF_DIR = f"{ARTIFACTS_DIR}/perf"
STRACE_DIR = f"{ARTIFACTS_DIR}/strace"
OSDI_DIR = f"{ARTIFACTS_DIR}/osdi"
SCRIPTS: Dict[str, str] = {
    "sim1_bsim4_300_perf       ": f"{RUN_SCRIPTS_DIR}/run_track_hold_sim1_bsim4_300_perf.sh",
    "sim1_bsim4_elided_300_perf": f"{RUN_SCRIPTS_DIR}/run_track_hold_sim1_bsim4_elided_300_perf.sh",
    "sim2_bsim4_300_perf       ": f"{RUN_SCRIPTS_DIR}/run_track_hold_sim2_bsim4_300_perf.sh",
    "sim2_bsim4_elided_300_perf": f"{RUN_SCRIPTS_DIR}/run_track_hold_sim2_bsim4_elided_300_perf.sh",
}

DEFAULT_EVENTS = (
    "task-clock,cycles,instructions,branches,branch-misses,"
    "cache-misses,context-switches,cpu-migrations"
)
PERF_EVENTS = os.environ.get("PERF_EVENTS", DEFAULT_EVENTS)


def parse_osdi_log(path: str) -> Dict[str, str]:
    data: Dict[str, str] = {}
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or "=" not in line:
                continue
            key, value = line.split("=", 1)
            data[key.strip()] = value.strip()
    return data


def parse_perf_csv(path: str) -> Dict[str, Tuple[float, str]]:
    metrics: Dict[str, Tuple[float, str]] = {}
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = [part.strip() for part in line.split(",")]
            if len(parts) < 3:
                continue
            value, unit, event = parts[0], parts[1], parts[2]
            if not event:
                continue
            if value in ("<not counted>", "<not supported>", ""):
                continue
            try:
                numeric = float(value.replace(",", ""))
            except ValueError:
                continue
            metrics[event] = (numeric, unit)
    return metrics


def parse_probe_events(osdi_data: Dict[str, str]) -> Tuple[str, List[str]]:
    group = osdi_data.get("OSDI_PROBE_GROUP", "osdi")
    events = osdi_data.get("OSDI_PROBE_EVENTS", "eval_0,setup_model_0,setup_instance_0")
    names = [event.strip() for event in events.split(",") if event.strip()]
    return group, names


def summarize(values: List[float], unit: str = "") -> str:
    mean = statistics.mean(values)
    median = statistics.median(values)
    stdev = statistics.stdev(values) if len(values) > 1 else 0.0
    sorted_vals = sorted(values)
    q1 = statistics.quantiles(sorted_vals, n=4)[0] if len(values) > 1 else median
    q3 = statistics.quantiles(sorted_vals, n=4)[2] if len(values) > 1 else median
    unit_sep = f" {unit}" if unit else ""
    return (
        f"n={len(values)} "
        f"mean={mean:.3f}{unit_sep} "
        f"median={median:.3f}{unit_sep} "
        f"stddev={stdev:.3f}{unit_sep} "
        f"p25={q1:.3f}{unit_sep} "
        f"p75={q3:.3f}{unit_sep} "
        f"min={min(values):.3f}{unit_sep} "
        f"max={max(values):.3f}{unit_sep}"
    )


def mean(values: List[float]) -> float:
    return statistics.mean(values) if values else 0.0


def stdev(values: List[float]) -> float:
    return statistics.stdev(values) if len(values) > 1 else 0.0


def print_summary_table(
    scripts: Dict[str, str],
    times: Dict[str, List[float]],
    osdi_file_opens: Dict[str, List[int]],
    osdi_runtime_calls: Dict[str, List[int]],
    osdi_probe_counts: Dict[str, Dict[str, List[int]]],
) -> None:
    headers = [
        "script",
        "n",
        "time_mean_s",
        "time_std_s",
        "file_opens_mean",
        "eval_mean",
        "setup_model_mean",
        "setup_instance_mean",
        "runtime_calls_mean",
    ]
    print("\nSummary Table:")
    print("\t".join(headers))
    for name in scripts:
        eval_mean = mean([float(v) for v in osdi_probe_counts[name].get("eval_0", [])])
        setup_model_mean = mean([float(v) for v in osdi_probe_counts[name].get("setup_model_0", [])])
        setup_instance_mean = mean([float(v) for v in osdi_probe_counts[name].get("setup_instance_0", [])])
        row = [
            name,
            str(len(times[name])),
            f"{mean(times[name]):.3f}",
            f"{stdev(times[name]):.3f}",
            f"{mean([float(v) for v in osdi_file_opens[name]]):.1f}",
            f"{eval_mean:.1f}",
            f"{setup_model_mean:.1f}",
            f"{setup_instance_mean:.1f}",
            f"{mean([float(v) for v in osdi_runtime_calls[name]]):.1f}",
        ]
        print("\t".join(row))


def run_once(
    name: str, cmd: str, run_id: str
) -> Tuple[float, int, int, Dict[str, int], Dict[str, Tuple[float, str]]]:
    perf_log = f"{PERF_DIR}/{name}_{run_id}.perf.csv"
    strace_log = f"{STRACE_DIR}/{name}_{run_id}.strace.log"
    osdi_log = f"{OSDI_DIR}/{name}_{run_id}.osdi.txt"

    os.makedirs(PERF_DIR, exist_ok=True)
    os.makedirs(STRACE_DIR, exist_ok=True)
    os.makedirs(OSDI_DIR, exist_ok=True)

    env = os.environ.copy()
    env["PERF_LOG"] = perf_log
    env["STRACE_LOG"] = strace_log
    env["OSDI_LOG"] = osdi_log
    env["PERF_EVENTS"] = PERF_EVENTS
    env["LC_ALL"] = "C"
    env["LANG"] = "C"

    start = time.monotonic()
    completed = subprocess.run(
        [cmd],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env=env,
        text=True,
    )
    elapsed = time.monotonic() - start

    if completed.returncode != 0:
        sys.stderr.write(f"{name} failed with exit code {completed.returncode}\n")
        if completed.stdout:
            sys.stderr.write(completed.stdout)
        if completed.stderr:
            sys.stderr.write(completed.stderr)
        sys.exit(1)

    osdi_data = parse_osdi_log(osdi_log)
    osdi_file_opens = int(osdi_data.get("OSDI_FILE_OPENS", osdi_data.get("OSDI_INVOCATIONS", "0")))
    perf_metrics = parse_perf_csv(perf_log)

    probe_group, probe_names = parse_probe_events(osdi_data)
    probe_counts: Dict[str, int] = {}
    for probe_name in probe_names:
        event = f"{probe_group}:{probe_name}"
        value = perf_metrics.get(event, (0.0, ""))[0]
        probe_counts[probe_name] = int(value)
    osdi_runtime_calls = sum(probe_counts.values())

    return elapsed, osdi_file_opens, osdi_runtime_calls, probe_counts, perf_metrics


def main() -> int:
    if TRIALS < 1:
        sys.stderr.write("TRIALS must be >= 1\n")
        return 1

    order = [name for name in SCRIPTS for _ in range(TRIALS)]
    random.shuffle(order)

    times: Dict[str, List[float]] = {name: [] for name in SCRIPTS}
    osdi_file_opens: Dict[str, List[int]] = {name: [] for name in SCRIPTS}
    osdi_runtime_calls: Dict[str, List[int]] = {name: [] for name in SCRIPTS}
    osdi_probe_counts: Dict[str, Dict[str, List[int]]] = {name: {} for name in SCRIPTS}
    perf_values: Dict[str, Dict[str, List[float]]] = {name: {} for name in SCRIPTS}
    perf_units: Dict[str, Dict[str, str]] = {name: {} for name in SCRIPTS}

    total_runs = len(order)
    print(f"Running {TRIALS} trials per script ({total_runs} total) in random order...\n")
    print(f"Using PERF_EVENTS={PERF_EVENTS}\n")

    for idx, name in enumerate(order, 1):
        run_id = f"{name}_{time.time_ns()}_{random.randint(0, 9999)}"
        elapsed, file_opens, runtime_calls, probe_counts, metrics = run_once(name, SCRIPTS[name], run_id)
        times[name].append(elapsed)
        osdi_file_opens[name].append(file_opens)
        osdi_runtime_calls[name].append(runtime_calls)
        for probe_name, value in probe_counts.items():
            osdi_probe_counts[name].setdefault(probe_name, []).append(value)
        for event, (value, unit) in metrics.items():
            perf_values[name].setdefault(event, []).append(value)
            perf_units[name].setdefault(event, unit)
        print(
            f"[{idx:02d}/{total_runs}] {name:26s} {elapsed:.3f}s "
            f"osdi_calls={runtime_calls} osdi_file_opens={file_opens}"
        )

    print_summary_table(SCRIPTS, times, osdi_file_opens, osdi_runtime_calls, osdi_probe_counts)

    print("\nPerf Counter Table:")
    perf_events = ["task-clock", "context-switches", "cpu-migrations"]
    headers = ["script"] + [f"{event}_mean" for event in perf_events]
    print("\t".join(headers))
    for name in SCRIPTS:
        row = [name]
        for event in perf_events:
            values = perf_values[name].get(event, [])
            row.append(f"{mean(values):.3f}" if values else "n/a")
        print("\t".join(row))
    return 0


if __name__ == "__main__":
    sys.exit(main())
