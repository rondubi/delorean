#!/usr/bin/env python3
from __future__ import annotations

import argparse
import ctypes
import importlib.util
import json
import math
import os
import random
import shutil
import subprocess
import sys
import tempfile
import traceback
from pathlib import Path


SETUP_MODEL = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p)
SETUP_INST = ctypes.CFUNCTYPE(
    None, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_double, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_void_p
)
EVAL = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p)
LOAD_NOISE = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_double, ctypes.POINTER(ctypes.c_double))
LOAD_RESIDUAL = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_double))
LOAD_SPICE_DC = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_double), ctypes.POINTER(ctypes.c_double))
LOAD_SPICE_TRAN = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_double), ctypes.POINTER(ctypes.c_double), ctypes.c_double)
LOAD_JACOBIAN = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p)
LOAD_JACOBIAN_ALPHA = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_double)
GIVEN_FLAG_MODEL = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32)
GIVEN_FLAG_INSTANCE = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32)
WRITE_JACOBIAN = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_double))
LOAD_JACOBIAN_OFFSET = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t)


class OsdiSimParas(ctypes.Structure):
    _fields_ = [
        ("names", ctypes.POINTER(ctypes.c_char_p)),
        ("vals", ctypes.POINTER(ctypes.c_double)),
        ("names_str", ctypes.POINTER(ctypes.c_char_p)),
        ("vals_str", ctypes.POINTER(ctypes.c_char_p)),
    ]


class OsdiInitInfo(ctypes.Structure):
    _fields_ = [("flags", ctypes.c_uint32), ("num_errors", ctypes.c_uint32), ("errors", ctypes.c_void_p)]


class OsdiSimInfo(ctypes.Structure):
    _fields_ = [
        ("paras", OsdiSimParas),
        ("abstime", ctypes.c_double),
        ("prev_solve", ctypes.POINTER(ctypes.c_double)),
        ("prev_state", ctypes.POINTER(ctypes.c_double)),
        ("next_state", ctypes.POINTER(ctypes.c_double)),
        ("flags", ctypes.c_uint32),
    ]


class OsdiDescriptor(ctypes.Structure):
    _fields_ = [
        ("name", ctypes.c_char_p),
        ("num_nodes", ctypes.c_uint32),
        ("num_terminals", ctypes.c_uint32),
        ("nodes", ctypes.c_void_p),
        ("num_jacobian_entries", ctypes.c_uint32),
        ("jacobian_entries", ctypes.c_void_p),
        ("num_collapsible", ctypes.c_uint32),
        ("collapsible", ctypes.c_void_p),
        ("collapsed_offset", ctypes.c_uint32),
        ("noise_sources", ctypes.c_void_p),
        ("num_noise_src", ctypes.c_uint32),
        ("num_params", ctypes.c_uint32),
        ("num_instance_params", ctypes.c_uint32),
        ("num_opvars", ctypes.c_uint32),
        ("param_opvar", ctypes.c_void_p),
        ("node_mapping_offset", ctypes.c_uint32),
        ("jacobian_ptr_resist_offset", ctypes.c_uint32),
        ("num_states", ctypes.c_uint32),
        ("state_idx_off", ctypes.c_uint32),
        ("bound_step_offset", ctypes.c_uint32),
        ("instance_size", ctypes.c_uint32),
        ("model_size", ctypes.c_uint32),
        ("access", ctypes.c_void_p),
        ("setup_model", SETUP_MODEL),
        ("setup_instance", SETUP_INST),
        ("eval", EVAL),
        ("load_noise", LOAD_NOISE),
        ("load_residual_resist", LOAD_RESIDUAL),
        ("load_residual_react", LOAD_RESIDUAL),
        ("load_limit_rhs_resist", LOAD_RESIDUAL),
        ("load_limit_rhs_react", LOAD_RESIDUAL),
        ("load_spice_rhs_dc", LOAD_SPICE_DC),
        ("load_spice_rhs_tran", LOAD_SPICE_TRAN),
        ("load_jacobian_resist", LOAD_JACOBIAN),
        ("load_jacobian_react", LOAD_JACOBIAN_ALPHA),
        ("load_jacobian_tran", LOAD_JACOBIAN_ALPHA),
        ("given_flag_model", GIVEN_FLAG_MODEL),
        ("given_flag_instance", GIVEN_FLAG_INSTANCE),
        ("num_resistive_jacobian_entries", ctypes.c_uint32),
        ("num_reactive_jacobian_entries", ctypes.c_uint32),
        ("write_jacobian_array_resist", WRITE_JACOBIAN),
        ("write_jacobian_array_react", WRITE_JACOBIAN),
        ("num_inputs", ctypes.c_uint32),
        ("inputs", ctypes.c_void_p),
        ("load_jacobian_with_offset_resist", LOAD_JACOBIAN_OFFSET),
        ("load_jacobian_with_offset_react", LOAD_JACOBIAN_OFFSET),
    ]


def main() -> int:
    ap = argparse.ArgumentParser(description="Compare random OSDI and lifted-Python evals.")
    ap.add_argument("target", nargs="?", default="diode")
    ap.add_argument("cases", nargs="?", type=int, default=8)
    ap.add_argument("seed", nargs="?", type=int, default=1)
    args = ap.parse_args()

    random.seed(args.seed)
    root = Path(__file__).resolve().parent
    verilog = resolve_target(root, args.target)
    module = parse_module(verilog)
    work = Path(tempfile.gettempdir()) / f"mir_lift_compare_{module}"
    work.mkdir(parents=True, exist_ok=True)
    osdi = work / f"{module}.osdi"
    py = work / f"{module}.py"
    metadata = Path(f"{py}.compile.json")

    compile_osdi(root, verilog, osdi)
    lift_python(root, verilog, py)

    osdi_results = run_osdi(osdi, args.cases)
    python_results = run_python(py, module, args.cases, metadata)

    print(f"osdi: {osdi}")
    print(f"python: {py}")
    print(f"metadata: {metadata}")
    print(f"osdi cases: {len(osdi_results)}")
    print(f"python calls: {len(python_results)}")

    if not comparable(osdi_results, python_results):
        print("not comparable: OSDI ctypes smoke currently captures flags/state; lifted Python captures flags/MIR outputs")
        print("needed next: map OSDI jacobian/state slots to the lifted Python output keys")
        return 2

    for i, (lhs, rhs) in enumerate(zip(osdi_results, python_results)):
        if not same(lhs, rhs):
            print(f"mismatch at case {i}: osdi={lhs!r} python={rhs!r}")
            return 1
    print("matched")
    return 0


def run_osdi(path: Path, cases: int) -> list[dict[str, object]]:
    lib = ctypes.CDLL(str(path))
    patch_osdi_log(lib)
    patch_lim_table(lib)
    ndesc = ctypes.c_uint32.in_dll(lib, "OSDI_NUM_DESCRIPTORS").value
    if ndesc < 1:
        raise SystemExit("OSDI_NUM_DESCRIPTORS is zero")
    desc = OsdiDescriptor.in_dll(lib, "OSDI_DESCRIPTORS")
    model = ctypes.create_string_buffer(desc.model_size)
    inst = ctypes.create_string_buffer(desc.instance_size)
    sim_paras, _sim_paras_keepalive = empty_sim_paras()
    init = OsdiInitInfo()
    handle = ctypes.c_char_p(b"direct_compare")
    handle_ptr = ctypes.cast(handle, ctypes.c_void_p)
    desc.setup_model(handle_ptr, ctypes.byref(model), ctypes.byref(sim_paras), ctypes.byref(init))
    check_init(init)
    desc.setup_instance(
        handle_ptr,
        ctypes.byref(inst),
        ctypes.byref(model),
        300.0,
        desc.num_terminals,
        ctypes.byref(sim_paras),
        ctypes.byref(init),
    )
    check_init(init)

    nsolve = max(1, desc.num_nodes)
    solve = (ctypes.c_double * nsolve)()
    prev_state = (ctypes.c_double * max(1, desc.num_states))()
    next_state = (ctypes.c_double * max(1, desc.num_states))()
    out = []
    for _ in range(cases):
        for i in range(nsolve):
            solve[i] = random.uniform(-1.0, 1.0)
        info = OsdiSimInfo(sim_paras, 0.0, solve, prev_state, next_state, 2048 | 4 | 8 | 64 | 128 | 512)
        flags = desc.eval(handle_ptr, ctypes.byref(inst), ctypes.byref(model), ctypes.byref(info))
        residual_len = max(1, desc.num_nodes)
        jacobian_len = max(1, desc.num_jacobian_entries)
        residual_resist = (ctypes.c_double * residual_len)()
        residual_react = (ctypes.c_double * residual_len)()
        limit_rhs_resist = (ctypes.c_double * residual_len)()
        limit_rhs_react = (ctypes.c_double * residual_len)()
        jacobian_resist = (ctypes.c_double * jacobian_len)()
        jacobian_react = (ctypes.c_double * jacobian_len)()
        desc.load_residual_resist(ctypes.byref(inst), ctypes.byref(model), residual_resist)
        desc.load_residual_react(ctypes.byref(inst), ctypes.byref(model), residual_react)
        desc.load_limit_rhs_resist(ctypes.byref(inst), ctypes.byref(model), limit_rhs_resist)
        desc.load_limit_rhs_react(ctypes.byref(inst), ctypes.byref(model), limit_rhs_react)
        desc.write_jacobian_array_resist(ctypes.byref(inst), ctypes.byref(model), jacobian_resist)
        desc.write_jacobian_array_react(ctypes.byref(inst), ctypes.byref(model), jacobian_react)
        out.append(
            {
                "flags": flags,
                "residual_resist": list(residual_resist),
                "residual_react": list(residual_react),
                "limit_rhs_resist": list(limit_rhs_resist),
                "limit_rhs_react": list(limit_rhs_react),
                "jacobian_resist": list(jacobian_resist),
                "jacobian_react": list(jacobian_react),
            }
        )
    return out


def run_python(path: Path, module: str, cases: int, metadata: Path) -> list[object]:
    spec = importlib.util.spec_from_file_location("lifted", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    if not metadata.exists():
        raise SystemExit(f"missing lift metadata: {metadata}")
    return run_python_with_metadata(mod, metadata, cases)


def run_python_with_metadata(mod: object, metadata: Path, cases: int) -> list[dict[str, object]]:
    db = json.loads(metadata.read_text())
    if not db.get("modules"):
        raise SystemExit(f"metadata has no modules: {metadata}")
    eval_meta = db["modules"][0]["eval"]
    func = getattr(mod, eval_meta["function"])
    params = eval_meta["params"]
    outputs = eval_meta["outputs"]
    out = []
    for _ in range(cases):
        try:
            result = func(*[0.5 for _ in params])
        except Exception as err:
            last = traceback.extract_tb(err.__traceback__)[-1]
            raise SystemExit(
                f"lifted Python failed: {type(err).__name__}: {err} at {last.filename}:{last.lineno}"
            ) from None
        if result is None:
            result = {}
        out.append(
            {
                "flags": 0,
                **{
                    name: [0.0 if value is None else result.get(value, 0.0) for value in values]
                    for name, values in outputs.items()
                },
            }
        )
    return out


def patch_osdi_log(lib: ctypes.CDLL) -> None:
    try:
        slot = ctypes.c_void_p.in_dll(lib, "osdi_log")
    except ValueError:
        return

    @ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32)
    def osdi_log(handle, msg, lvl):
        if msg is None:
            text = "<null>"
        else:
            text = msg.decode(errors="replace")
        if lvl & 16:
            print(f"osdi log format error: {text}", file=sys.stderr)

    patch_osdi_log._callback = osdi_log
    slot.value = ctypes.cast(osdi_log, ctypes.c_void_p).value


def patch_lim_table(lib: ctypes.CDLL) -> None:
    # Simple enough for current OpenVAF OSDI: if a limiter table exists, make pnjlim callable.
    try:
        table_len = ctypes.c_uint32.in_dll(lib, "OSDI_LIM_TABLE_LEN").value
        table_ptr = ctypes.c_void_p.in_dll(lib, "OSDI_LIM_TABLE").value
    except ValueError:
        return

    class Lim(ctypes.Structure):
        _fields_ = [("name", ctypes.c_char_p), ("num_args", ctypes.c_uint32), ("func_ptr", ctypes.c_void_p)]

    table = (Lim * table_len).from_address(table_ptr)

    @ctypes.CFUNCTYPE(ctypes.c_double, ctypes.c_bool, ctypes.POINTER(ctypes.c_bool), ctypes.c_double, ctypes.c_double, ctypes.c_double, ctypes.c_double)
    def pnjlim(_init, check, vnew, vold, vt, vcrit):
        check[0] = False
        if vnew > vcrit and abs(vnew - vold) > (vt + vt):
            if vold > 0.0:
                arg = 1.0 + (vnew - vold) / vt
                if arg > 0.0:
                    vnew = vold + vt * math.log(arg)
                else:
                    vnew = vcrit
            else:
                vnew = vt * math.log(vnew / vt)
            check[0] = True
        return vnew

    patch_lim_table._pnjlim = pnjlim
    for item in table:
        if item.name and item.name.decode() == "pnjlim":
            item.func_ptr = ctypes.cast(pnjlim, ctypes.c_void_p).value


def empty_sim_paras() -> tuple[OsdiSimParas, tuple[ctypes.c_char_p, ctypes.c_char_p]]:
    names = ctypes.c_char_p()
    names_str = ctypes.c_char_p()
    return (
        OsdiSimParas(
            ctypes.pointer(names),
            ctypes.POINTER(ctypes.c_double)(),
            ctypes.pointer(names_str),
            ctypes.POINTER(ctypes.c_char_p)(),
        ),
        (names, names_str),
    )


def check_init(info: OsdiInitInfo) -> None:
    if info.flags or info.num_errors:
        raise SystemExit(f"OSDI init failed: flags={info.flags} errors={info.num_errors}")


def comparable(lhs: list[object], rhs: list[object]) -> bool:
    if not lhs or not rhs or type(lhs[0]) is not type(rhs[0]):
        return False
    if isinstance(lhs[0], dict) and isinstance(rhs[0], dict):
        return set(lhs[0]) == set(rhs[0])
    return True


def same(lhs: object, rhs: object) -> bool:
    if isinstance(lhs, float) or isinstance(rhs, float):
        return math.isclose(float(lhs), float(rhs), rel_tol=1e-8, abs_tol=1e-10)
    if isinstance(lhs, list) and isinstance(rhs, list):
        return len(lhs) == len(rhs) and all(same(a, b) for a, b in zip(lhs, rhs))
    if isinstance(lhs, dict) and isinstance(rhs, dict):
        return set(lhs) == set(rhs) and all(same(lhs[key], rhs[key]) for key in lhs)
    return lhs == rhs


def resolve_target(root: Path, target: str) -> Path:
    repo = root.parent.parent
    known = {
        "diode": repo / "integration_tests/DIODE/diode.va",
        "bsim4": repo / "integration_tests/BSIM4/bsim4.va",
    }
    return known.get(target, Path(target)).resolve()


def parse_module(path: Path) -> str:
    for line in path.read_text(errors="ignore").splitlines():
        line = line.strip()
        if line.startswith("module "):
            return line.split()[1].split("(")[0]
    raise SystemExit(f"no module declaration in {path}")


def env() -> dict[str, str]:
    e = os.environ.copy()
    e["PATH"] = ":".join(["/home/ron/.cargo/bin", "/root/.cargo/bin", "/opt/LLVM/bin", e.get("PATH", "")])
    return e


def compile_osdi(root: Path, verilog: Path, osdi: Path) -> None:
    workspace = root.parent.parent
    rustup = shutil.which("rustup", path=env()["PATH"]) or "/home/ron/.cargo/bin/rustup"
    run(
        [
            rustup,
            "run",
            "stable-aarch64-unknown-linux-gnu",
            "cargo",
            "run",
            "--manifest-path",
            str(workspace / "Cargo.toml"),
            "-p",
            "openvaf-driver",
            "--",
            "-O",
            "0",
            "-o",
            str(osdi),
            str(verilog),
        ],
        cwd=workspace,
    )


def lift_python(root: Path, verilog: Path, py: Path) -> None:
    run([sys.executable, str(root / "mir_lift_runner.py"), str(verilog), "-o", str(py)], cwd=root)


def run(cmd: list[str], cwd: Path) -> None:
    res = subprocess.run(cmd, cwd=cwd, env=env(), text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if res.returncode != 0:
        if res.stdout:
            print(res.stdout, end="")
        if res.stderr:
            print(res.stderr, end="", file=sys.stderr)
        raise SystemExit(res.returncode)


if __name__ == "__main__":
    raise SystemExit(main())
