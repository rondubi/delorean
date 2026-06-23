#!/usr/bin/env python3
from __future__ import annotations

import os
import shutil
import tempfile
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
WORKSPACE_ROOT = SCRIPT_DIR.parent.parent
MANIFEST_PATH = WORKSPACE_ROOT / "Cargo.toml"


def require_workspace_root() -> Path:
    if not MANIFEST_PATH.is_file():
        raise SystemExit(f"missing workspace Cargo.toml: {MANIFEST_PATH}")
    if not (WORKSPACE_ROOT / "integration_tests").is_dir():
        raise SystemExit(f"missing integration_tests directory under workspace: {WORKSPACE_ROOT}")
    return WORKSPACE_ROOT


def model_source(target: str) -> Path:
    require_workspace_root()
    known = {
        "diode": WORKSPACE_ROOT / "integration_tests/DIODE/diode.va",
        "bsim4": WORKSPACE_ROOT / "integration_tests/BSIM4/bsim4.va",
    }
    path = known.get(target, Path(target).expanduser())
    if not path.is_absolute():
        path = Path.cwd() / path
    path = path.resolve()
    if not path.is_file():
        raise SystemExit(f"missing Verilog-A source for {target!r}: {path}")
    return path


def default_run_root(name: str) -> Path:
    return Path(tempfile.gettempdir()) / name


def checked_root() -> Path:
    return default_run_root("mir_lift_checked")


def compare_root() -> Path:
    return default_run_root("mir_lift_compare_checked")


def current_root() -> Path:
    return default_run_root("mir_lift_current")


def output_path(output_dir: Path, verilog_file: Path) -> Path:
    return output_dir / f"{verilog_file.stem}.py"


def current_output_path(verilog_file: Path) -> Path:
    return output_path(current_root(), verilog_file)


def publish_current_output(output: Path, verilog_file: Path) -> Path:
    current = current_output_path(verilog_file)
    current.parent.mkdir(parents=True, exist_ok=True)
    current.unlink(missing_ok=True)
    try:
        current.symlink_to(output)
    except OSError:
        shutil.copy2(output, current)
    return current


def tool_env(target_dir: Path | None = None) -> dict[str, str]:
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
    if target_dir is not None:
        target_dir.mkdir(parents=True, exist_ok=True)
        env["CARGO_TARGET_DIR"] = str(target_dir)
    return env


def rustup_path() -> str:
    env = tool_env()
    rustup = shutil.which("rustup", path=env["PATH"])
    if rustup is not None:
        return rustup
    for candidate in ("/home/ron/.cargo/bin/rustup", "/root/.cargo/bin/rustup"):
        if Path(candidate).exists():
            return candidate
    raise SystemExit("could not find rustup")
