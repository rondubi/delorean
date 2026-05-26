#!/usr/bin/env python3
from __future__ import annotations

import argparse
import importlib.util
import inspect
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


def main() -> int:
    script_path = Path(__file__).resolve()
    crate_root = script_path.parent
    workspace_root = crate_root.parent.parent

    parser = argparse.ArgumentParser(
        description="Build OSDI + lifted Python, then run the OSDI through Delorean's ngspice substitution harness."
    )
    parser.add_argument("target", nargs="?", default="bsim4", help="bsim4, diode, or a Verilog-A file")
    parser.add_argument("--work-dir", default=None)
    parser.add_argument("--ngspice-bin", default=None)
    parser.add_argument(
        "--netlist",
        default=None,
        help="Delorean skywater-examples netlist path. Defaults to the BSIM4 VGS sweep for bsim4.",
    )
    parser.add_argument("--kind", choices=["auto", "track_hold", "vgs_sweep", "generic"], default="auto")
    parser.add_argument("--swap-model", action="append", default=None)
    parser.add_argument("--with-perf", action="store_true")
    parser.add_argument("--keep-work", action="store_true")
    parser.add_argument(
        "--replay-json",
        default=None,
        help="Optional JSON call trace with function/args/expected entries to replay against lifted Python.",
    )
    args = parser.parse_args()

    verilog_file = resolve_target(crate_root, args.target)
    module = parse_module_name(verilog_file)
    work_dir = Path(args.work_dir).expanduser().resolve() if args.work_dir else Path(tempfile.mkdtemp(prefix="mir_lift_ngspice_"))
    work_dir.mkdir(parents=True, exist_ok=True)

    osdi_path = work_dir / f"{module}.osdi"
    python_path = work_dir / f"{module}.py"
    summary_path = work_dir / "summary.json"

    env = runner_env()
    compile_osdi(workspace_root, env, verilog_file, osdi_path)
    lift_python(crate_root, verilog_file, python_path)
    ngspice_summary = run_delorean_ngspice(crate_root, osdi_path, args)

    python_summary = inspect_lifted_python(python_path, module)
    replay_summary = None
    if args.replay_json:
        replay_summary = replay_python_trace(python_path, Path(args.replay_json).expanduser().resolve())

    summary = {
        "target": str(verilog_file),
        "module": module,
        "work_dir": str(work_dir),
        "osdi": str(osdi_path),
        "lifted_python": str(python_path),
        "ngspice": ngspice_summary,
        "lifted_python_load": python_summary,
        "python_replay": replay_summary,
        "note": (
            "The ngspice reference is run through Delorean's run_ngspice_osdi.py substitution harness. "
            "Numeric lifted Python comparison still needs a compatible replay trace or "
            "an OSDI-to-Python result bridge."
        ),
    }
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8")

    print(f"ngspice reference: {ngspice_summary['artifact']}")
    print(f"lifted Python: {python_path}")
    print(f"summary: {summary_path}")
    if replay_summary is None:
        print("numeric Python comparison: pending MIR-argument trace bridge")
    elif replay_summary["mismatches"]:
        print(f"numeric Python comparison: {len(replay_summary['mismatches'])} mismatches")
        return 1
    else:
        print(f"numeric Python comparison: {replay_summary['checked']} calls matched")

    if not args.keep_work and args.work_dir is None:
        print(f"temporary work directory retained for inspection: {work_dir}")

    return 0


def resolve_target(crate_root: Path, target: str) -> Path:
    root = crate_root.parent.parent
    known = {
        "diode": root / "integration_tests/DIODE/diode.va",
        "bsim4": root / "integration_tests/BSIM4/bsim4.va",
    }
    return known.get(target, Path(target)).expanduser().resolve()


def parse_module_name(verilog_file: Path) -> str:
    for line in verilog_file.read_text(encoding="utf-8", errors="ignore").splitlines():
        stripped = line.strip()
        if stripped.startswith("module "):
            return stripped.split()[1].split("(")[0]
    raise SystemExit(f"could not find module declaration in {verilog_file}")


def runner_env() -> dict[str, str]:
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
    return env


def rustup_bin(env: dict[str, str]) -> str:
    rustup = shutil.which("rustup", path=env["PATH"])
    if rustup:
        return rustup
    for candidate in ("/home/ron/.cargo/bin/rustup", "/root/.cargo/bin/rustup"):
        if Path(candidate).exists():
            return candidate
    raise SystemExit("could not find rustup")


def compile_osdi(workspace_root: Path, env: dict[str, str], verilog_file: Path, osdi_path: Path) -> None:
    cmd = [
        rustup_bin(env),
        "run",
        "stable-aarch64-unknown-linux-gnu",
        "cargo",
        "run",
        "--manifest-path",
        str(workspace_root / "Cargo.toml"),
        "-p",
        "openvaf-driver",
        "--",
        "-O",
        "0",
        "-o",
        str(osdi_path),
        str(verilog_file),
    ]
    subprocess.run(cmd, cwd=workspace_root, env=env, check=True)


def lift_python(crate_root: Path, verilog_file: Path, python_path: Path) -> None:
    subprocess.run(
        [sys.executable, str(crate_root / "mir_lift_runner.py"), str(verilog_file), "-o", str(python_path)],
        cwd=crate_root,
        check=True,
    )


def run_delorean_ngspice(crate_root: Path, osdi_path: Path, args: argparse.Namespace) -> dict[str, Any]:
    delorean_root = Path("/home/ron/delorean/tests/sky-use/skywater-examples")
    runner = delorean_root / "run-scripts/run_ngspice_osdi.py"
    if not runner.exists():
        raise SystemExit(f"missing Delorean ngspice runner: {runner}")

    netlist = args.netlist or "netlists/vgs_sweep_netlist_300.spice"
    swap_models = args.swap_model
    if swap_models is None:
        swap_models = [
            "sky130_fd_pr__pfet_01v8_lvt",
            "sky130_fd_pr__pfet_01v8",
            "sky130_fd_pr__pfet_01v8_hvt",
        ]

    tag = f"mir_lift_{osdi_path.stem}"
    cmd = [
        sys.executable,
        str(runner),
        "--osdi",
        str(osdi_path),
        "--netlist",
        netlist,
        "--kind",
        args.kind,
        "--tag",
        tag,
    ]
    for model in swap_models:
        cmd.extend(["--swap-model", model])
    if args.ngspice_bin:
        cmd.extend(["--ngspice-bin", args.ngspice_bin])
    if args.with_perf:
        cmd.append("--with-perf")

    subprocess.run(cmd, cwd=delorean_root, check=True)

    kind = args.kind
    if kind == "auto":
        name = Path(netlist).name
        kind = "vgs_sweep" if name.startswith("vgs_sweep_") else "track_hold" if name.startswith("track_hold_") else "generic"
    artifact = delorean_root / f"artifacts/raw/{tag}.raw"
    if kind == "track_hold":
        artifact = delorean_root / f"artifacts/wrdata/{tag}_out.txt"
    return {
        "runner": str(runner),
        "netlist": netlist,
        "swap_models": swap_models,
        "kind": kind,
        "artifact": str(artifact),
    }


def inspect_lifted_python(python_path: Path, module: str) -> dict[str, Any]:
    spec = importlib.util.spec_from_file_location("lifted_model", python_path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"could not load lifted Python module {python_path}")
    lifted = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lifted)
    functions = {}
    for suffix in ("model", "init", "eval"):
        name = f"{module}_{suffix}"
        func = getattr(lifted, name, None)
        if func is not None:
            functions[name] = len(inspect.signature(func).parameters)
    return {"ok": True, "functions": functions}


def replay_python_trace(python_path: Path, trace_path: Path) -> dict[str, Any]:
    spec = importlib.util.spec_from_file_location("lifted_model_replay", python_path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"could not load lifted Python module {python_path}")
    lifted = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lifted)

    trace = json.loads(trace_path.read_text(encoding="utf-8"))
    calls = trace.get("calls", trace if isinstance(trace, list) else [])
    mismatches = []
    checked = 0
    for index, call in enumerate(calls):
        func = getattr(lifted, call["function"])
        actual = func(*call.get("args", []))
        expected = call.get("expected")
        if expected is not None:
            checked += 1
            if actual != expected:
                mismatches.append({"index": index, "function": call["function"], "actual": actual, "expected": expected})
    return {"checked": checked, "mismatches": mismatches}


if __name__ == "__main__":
    raise SystemExit(main())
