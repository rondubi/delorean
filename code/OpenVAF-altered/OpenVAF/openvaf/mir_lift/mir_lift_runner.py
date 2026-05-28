#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
from pathlib import Path


def default_output_path(verilog_file: Path) -> Path:
    output_dir = Path("/tmp/mir_lift_current")
    return output_dir / f"{verilog_file.stem}.py"


def main() -> int:
    script_path = Path(__file__).resolve()
    crate_root = script_path.parent
    workspace_root = crate_root.parent.parent
    manifest_path = workspace_root / "Cargo.toml"

    parser = argparse.ArgumentParser()
    parser.add_argument("verilog_file")
    parser.add_argument("-o", "--output")
    parser.add_argument("--dump-lir", action="store_true")
    args = parser.parse_args()

    verilog_file = Path(args.verilog_file).expanduser().resolve()
    output = (
        Path(args.output).expanduser().resolve()
        if args.output
        else default_output_path(verilog_file)
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    before_mtime = output.stat().st_mtime_ns if output.exists() else None

    env = os.environ.copy()
    env["PATH"] = ":".join(
        [
            "/root/.rustup/toolchains/stable-aarch64-unknown-linux-gnu/bin",
            "/home/ron/.cargo/bin",
            "/root/.cargo/bin",
            "/opt/LLVM/bin",
            env.get("PATH", ""),
        ]
    )

    rustup = shutil.which("rustup", path=env["PATH"])
    if rustup is None:
        for candidate in ("/home/ron/.cargo/bin/rustup", "/root/.cargo/bin/rustup"):
            if Path(candidate).exists():
                rustup = candidate
                break
    if rustup is None:
        raise SystemExit("could not find rustup")

    cmd = [
        rustup,
        "run",
        "stable-aarch64-unknown-linux-gnu",
        "cargo",
        "run",
        "--manifest-path",
        str(manifest_path),
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

    status = subprocess.run(cmd, cwd=workspace_root, env=env).returncode
    if status != 0:
        return status
    if not output.exists():
        print(f"mir_lift_runner: expected output was not written: {output}")
        return 1
    after_mtime = output.stat().st_mtime_ns
    if before_mtime is not None and after_mtime == before_mtime:
        print(f"mir_lift_runner: output was not updated: {output}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
