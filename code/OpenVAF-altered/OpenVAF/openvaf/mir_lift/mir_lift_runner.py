#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
from pathlib import Path


def main() -> int:
    script_path = Path(__file__).resolve()
    crate_root = script_path.parent
    workspace_root = crate_root.parent.parent
    manifest_path = workspace_root / "Cargo.toml"

    parser = argparse.ArgumentParser()
    parser.add_argument("verilog_file")
    parser.add_argument("-o", "--output")
    args = parser.parse_args()

    verilog_file = Path(args.verilog_file).expanduser().resolve()
    output = (
        Path(args.output).expanduser().resolve()
        if args.output
        else verilog_file.with_suffix(".py")
    )

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

    return subprocess.run(cmd, cwd=workspace_root, env=env).returncode


if __name__ == "__main__":
    raise SystemExit(main())
