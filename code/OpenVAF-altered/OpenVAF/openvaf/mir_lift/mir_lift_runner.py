#!/usr/bin/env python3
from __future__ import annotations

import argparse
import subprocess
from pathlib import Path

from mir_lift_paths import MANIFEST_PATH, current_root, output_path, require_workspace_root, rustup_path, tool_env


def default_output_path(verilog_file: Path, current_dir: Path) -> Path:
    return output_path(current_dir, verilog_file)


def main() -> int:
    workspace_root = require_workspace_root()

    parser = argparse.ArgumentParser()
    parser.add_argument("verilog_file")
    parser.add_argument("-o", "--output")
    parser.add_argument("--work-root", type=Path, default=current_root())
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--current-dir", type=Path)
    parser.add_argument("--dump-lir", action="store_true")
    args = parser.parse_args()

    verilog_file = Path(args.verilog_file).expanduser().resolve()
    if not verilog_file.is_file():
        raise SystemExit(f"missing Verilog-A source: {verilog_file}")
    work_root = args.work_root.expanduser().resolve()
    target_dir = (args.target_dir.expanduser().resolve() if args.target_dir else work_root / "target")
    current_dir = (args.current_dir.expanduser().resolve() if args.current_dir else work_root / "out")
    output = (
        Path(args.output).expanduser().resolve()
        if args.output
        else default_output_path(verilog_file, current_dir)
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    output.unlink(missing_ok=True)

    cmd = [
        rustup_path(),
        "run",
        "stable-aarch64-unknown-linux-gnu",
        "cargo",
        "run",
        "--manifest-path",
        str(MANIFEST_PATH),
        "-p",
        "openvaf-driver",
        "--",
        "--backend",
        "mir-lift",
        "-o",
        str(output),
        str(verilog_file),
    ]
    if args.dump_lir:
        cmd.insert(-1, "--dump-lir")

    status = subprocess.run(cmd, cwd=workspace_root, env=tool_env(target_dir)).returncode
    if status != 0:
        return status
    if not output.exists():
        print(f"mir_lift_runner: expected output was not written: {output}")
        return 1
    if not output.is_file() or output.stat().st_size == 0:
        print(f"mir_lift_runner: output is empty: {output}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
