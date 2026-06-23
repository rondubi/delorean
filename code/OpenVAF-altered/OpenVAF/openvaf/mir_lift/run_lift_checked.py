#!/usr/bin/env python3
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

from mir_lift_paths import SCRIPT_DIR, checked_root, model_source, output_path, publish_current_output


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run mir_lift with isolated deterministic build and output directories."
    )
    parser.add_argument("mode", nargs="?", choices=("all", "lift", "compare"), default="all")
    parser.add_argument("model", nargs="?", default="bsim4")
    parser.add_argument("cases", nargs="?", type=int, default=1)
    parser.add_argument("seed", nargs="?", type=int, default=1)
    parser.add_argument("--work-root", type=Path, default=checked_root())
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--current-dir", type=Path)
    args = parser.parse_args()

    if args.cases <= 0:
        raise SystemExit("cases must be positive")

    work_root = args.work_root.expanduser().resolve()
    target_dir = (args.target_dir.expanduser().resolve() if args.target_dir else work_root / "target")
    current_dir = (args.current_dir.expanduser().resolve() if args.current_dir else work_root / "out")
    target_dir.mkdir(parents=True, exist_ok=True)
    current_dir.mkdir(parents=True, exist_ok=True)

    verilog = model_source(args.model)
    output = output_path(current_dir, verilog)

    print(f"CARGO_TARGET_DIR={target_dir}", flush=True)
    print(f"current output dir={current_dir}", flush=True)

    if args.mode in ("all", "lift"):
        print(f"lifting {args.model} -> {output}", flush=True)
        status = subprocess.run(
            [
                sys.executable,
                str(SCRIPT_DIR / "mir_lift_runner.py"),
                str(verilog),
                "-o",
                str(output),
                "--work-root",
                str(work_root),
                "--target-dir",
                str(target_dir),
                "--current-dir",
                str(current_dir),
            ],
            cwd=SCRIPT_DIR,
        ).returncode
        if status != 0:
            return status
        if not output.is_file() or output.stat().st_size == 0:
            raise SystemExit(f"lift did not write output: {output}")
        current_output = publish_current_output(output, verilog)
        print(f"lifted python: {output}", flush=True)
        print(f"latest lifted python: {current_output}", flush=True)

    if args.mode in ("all", "compare"):
        print(f"compare_random {args.model} {args.cases} {args.seed}", flush=True)
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT_DIR / "direct_compare.py"),
                args.model,
                str(args.cases),
                str(args.seed),
                "--work-root",
                str(work_root),
                "--target-dir",
                str(target_dir),
                "--current-dir",
                str(current_dir),
            ],
            cwd=SCRIPT_DIR,
        ).returncode

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
