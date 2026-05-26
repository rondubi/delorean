#!/usr/bin/env python3
from __future__ import annotations

import argparse
import ctypes
import importlib.util
import inspect
import math
import os
import random
import shutil
import subprocess
import sys
import tempfile
import traceback
from dataclasses import dataclass
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
ACCESS = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32)

PARA_TY_MASK = 3
PARA_TY_REAL = 0
PARA_TY_INT = 1
PARA_TY_STR = 2
PARA_KIND_MASK = 3 << 30
PARA_KIND_MODEL = 0 << 30
PARA_KIND_INST = 1 << 30
PARA_KIND_OPVAR = 2 << 30
ACCESS_FLAG_SET = 1
ACCESS_FLAG_INSTANCE = 4
MAX_INIT_ATTEMPTS = 8
CALC_RESIST_RESIDUAL = 1
CALC_REACT_RESIDUAL = 2
CALC_RESIST_JACOBIAN = 4
CALC_REACT_JACOBIAN = 8
CALC_OP = 32
CALC_RESIST_LIM_RHS = 64
CALC_REACT_LIM_RHS = 128
ENABLE_LIM = 256
INIT_LIM = 512
ANALYSIS_DC = 2048
ANALYSIS_TRAN = 8192
ANALYSIS_IC = 16384
JACOBIAN_ENTRY_RESIST = 4
JACOBIAN_ENTRY_REACT = 8

FULL_CALC_FLAGS = (
    CALC_RESIST_RESIDUAL
    | CALC_REACT_RESIDUAL
    | CALC_RESIST_JACOBIAN
    | CALC_REACT_JACOBIAN
    | CALC_RESIST_LIM_RHS
    | CALC_REACT_LIM_RHS
)

EVAL_FLAG_COMBOS = [
    ANALYSIS_DC | FULL_CALC_FLAGS | INIT_LIM,
    ANALYSIS_DC | FULL_CALC_FLAGS | ENABLE_LIM | CALC_OP,
    ANALYSIS_TRAN | FULL_CALC_FLAGS | ENABLE_LIM,
    ANALYSIS_TRAN | FULL_CALC_FLAGS,
    ANALYSIS_IC | FULL_CALC_FLAGS | INIT_LIM,
]

@dataclass(frozen=True)
class ParamSpec:
    name: str
    strategy: str
    scope: str = "any"
    modules: tuple[str, ...] = ()
    value_type: str = "real"
    base: int | float | None = None
    default_source: str = "Verilog-A default"
    minimum: float | None = None
    maximum: float | None = None
    positive: bool = False
    finite: bool = True
    choices: tuple[int | float, ...] = ()
    given: bool = True
    builtin: bool = False
    category: str = ""
    note: str = ""

    def sample(self) -> int | float:
        if self.strategy == "choice":
            if not self.choices:
                raise SystemExit(f"ParamSpec {self.name!r} has no choices")
            return self.validate(random.choice(self.choices))
        if self.minimum is None or self.maximum is None:
            raise SystemExit(f"ParamSpec {self.name!r} needs minimum/maximum")
        if self.strategy == "uniform":
            return self.validate(random.uniform(self.minimum, self.maximum))
        if self.strategy == "log_uniform":
            return self.validate(random_log_uniform(self.minimum, self.maximum))
        raise SystemExit(f"unknown ParamSpec strategy {self.strategy!r} for {self.name!r}")

    def validate(self, value: int | float) -> int | float:
        if self.finite and not math.isfinite(float(value)):
            raise SystemExit(f"ParamSpec {self.name!r} produced non-finite value {value!r}")
        if self.positive and float(value) <= 0.0:
            raise SystemExit(f"ParamSpec {self.name!r} produced non-positive value {value!r}")
        if self.minimum is not None and float(value) < self.minimum:
            raise SystemExit(f"ParamSpec {self.name!r} produced value below minimum: {value!r}")
        if self.maximum is not None and float(value) > self.maximum:
            raise SystemExit(f"ParamSpec {self.name!r} produced value above maximum: {value!r}")
        if self.value_type == "int":
            return int(value)
        return float(value)


PARAM_SPECS: tuple[ParamSpec, ...] = (
    ParamSpec(
        "is",
        "log_uniform",
        modules=("diode_va",),
        base=1e-14,
        minimum=1e-15,
        maximum=1e-12,
        positive=True,
        category="diode iv",
        note="saturation current",
    ),
    ParamSpec(
        "rs",
        "log_uniform",
        modules=("diode_va",),
        base=0.0,
        minimum=1e-3,
        maximum=10.0,
        positive=True,
        category="diode parasitic",
        note="ohmic resistance",
    ),
    ParamSpec("zetars", "uniform", modules=("diode_va",), base=0.0, minimum=-1.0, maximum=1.0, category="diode temperature"),
    ParamSpec("n", "uniform", modules=("diode_va",), base=1.0, minimum=1.0, maximum=2.0, positive=True, category="diode iv"),
    ParamSpec("cj0", "log_uniform", modules=("diode_va",), base=0.0, minimum=1e-15, maximum=1e-11, positive=True, category="diode capacitance"),
    ParamSpec("vj", "uniform", modules=("diode_va",), base=1.0, minimum=0.3, maximum=1.2, positive=True, category="diode capacitance"),
    ParamSpec("m", "uniform", modules=("diode_va",), base=0.5, minimum=0.2, maximum=0.8, positive=True, category="diode capacitance"),
    ParamSpec("rth", "choice", modules=("diode_va",), base=0.0, choices=(0.0, 10.0, 100.0, 1000.0), category="diode thermal"),
    ParamSpec("zetarth", "uniform", modules=("diode_va",), base=0.0, minimum=-1.0, maximum=1.0, category="diode thermal"),
    ParamSpec("zetais", "uniform", modules=("diode_va",), base=3.0, minimum=2.0, maximum=4.0, category="diode temperature"),
    ParamSpec("ea", "uniform", modules=("diode_va",), base=1.11, minimum=0.8, maximum=1.3, positive=True, category="diode temperature"),
    ParamSpec("tnom", "uniform", modules=("diode_va",), base=300.0, minimum=290.0, maximum=330.0, positive=True, category="diode temperature"),
    ParamSpec(
        "minr",
        "log_uniform",
        modules=("diode_va",),
        base=1e-3,
        default_source="$simparam('minr', 1m)",
        minimum=1e-4,
        maximum=1e-2,
        positive=True,
        category="diode simulator",
    ),
    ParamSpec(
        "type",
        "choice",
        modules=("bsim4va",),
        value_type="int",
        base=1,
        default_source="BSIM4 setup default when not given",
        choices=(-1, 1),
        category="bsim4 polarity",
    ),
    ParamSpec(
        "l",
        "log_uniform",
        modules=("bsim4va",),
        base=1e-6,
        default_source="BSIM4 setup default when not given",
        minimum=1e-7,
        maximum=2e-6,
        positive=True,
        category="bsim4 geometry",
    ),
    ParamSpec(
        "w",
        "log_uniform",
        modules=("bsim4va",),
        base=1e-6,
        default_source="BSIM4 setup default when not given",
        minimum=2e-7,
        maximum=1e-5,
        positive=True,
        category="bsim4 geometry",
    ),
    ParamSpec(
        "nf",
        "choice",
        modules=("bsim4va",),
        value_type="int",
        base=1,
        default_source="BSIM4 setup default when not given",
        choices=(1, 2, 4),
        category="bsim4 geometry",
    ),
    ParamSpec("ad", "log_uniform", modules=("bsim4va",), base=1e-12, minimum=1e-14, maximum=1e-11, positive=True, category="bsim4 geometry"),
    ParamSpec("as", "log_uniform", modules=("bsim4va",), base=1e-12, minimum=1e-14, maximum=1e-11, positive=True, category="bsim4 geometry"),
    ParamSpec("pd", "log_uniform", modules=("bsim4va",), base=1e-6, minimum=1e-7, maximum=1e-5, positive=True, category="bsim4 geometry"),
    ParamSpec("ps", "log_uniform", modules=("bsim4va",), base=1e-6, minimum=1e-7, maximum=1e-5, positive=True, category="bsim4 geometry"),
    ParamSpec("nrd", "uniform", modules=("bsim4va",), base=1.0, minimum=0.0, maximum=2.0, category="bsim4 resistance"),
    ParamSpec("nrs", "uniform", modules=("bsim4va",), base=1.0, minimum=0.0, maximum=2.0, category="bsim4 resistance"),
    ParamSpec("off", "choice", modules=("bsim4va",), value_type="int", base=0, choices=(0, 1), category="bsim4 operating point"),
    ParamSpec("toxe", "log_uniform", modules=("bsim4va",), base=3e-9, minimum=1.5e-9, maximum=8e-9, positive=True, category="bsim4 oxide"),
    ParamSpec("toxp", "log_uniform", modules=("bsim4va",), base=3e-9, minimum=1.5e-9, maximum=8e-9, positive=True, category="bsim4 oxide"),
    ParamSpec("epsrox", "uniform", modules=("bsim4va",), base=3.9, minimum=3.5, maximum=4.2, positive=True, category="bsim4 oxide"),
    ParamSpec("vth0", "uniform", modules=("bsim4va",), base=0.7, minimum=0.35, maximum=0.9, category="bsim4 threshold"),
    ParamSpec("u0", "uniform", modules=("bsim4va",), base=0.067, minimum=0.02, maximum=0.12, positive=True, category="bsim4 mobility"),
    ParamSpec("vsat", "uniform", modules=("bsim4va",), base=8e4, minimum=5e4, maximum=1.5e5, positive=True, category="bsim4 mobility"),
    ParamSpec("rsh", "uniform", modules=("bsim4va",), base=0.0, minimum=0.0, maximum=20.0, category="bsim4 resistance"),
    ParamSpec("cgso", "log_uniform", modules=("bsim4va",), base=1e-10, minimum=1e-12, maximum=1e-9, positive=True, category="bsim4 capacitance"),
    ParamSpec("cgdo", "log_uniform", modules=("bsim4va",), base=1e-10, minimum=1e-12, maximum=1e-9, positive=True, category="bsim4 capacitance"),
    ParamSpec("tnom", "uniform", modules=("bsim4va",), base=27.0, minimum=0.0, maximum=80.0, category="bsim4 temperature"),
    ParamSpec(
        "mfactor",
        "choice",
        scope="instance",
        modules=("bsim4va",),
        base=1.0,
        choices=(0.5, 1.0, 2.0, 4.0),
        builtin=True,
        category="builtin",
    ),
)


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


class OsdiNodePair(ctypes.Structure):
    _fields_ = [("node_1", ctypes.c_uint32), ("node_2", ctypes.c_uint32)]


class OsdiJacobianEntry(ctypes.Structure):
    _fields_ = [
        ("nodes", OsdiNodePair),
        ("react_ptr_off", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
    ]


class OsdiParamOpvar(ctypes.Structure):
    _fields_ = [
        ("name", ctypes.POINTER(ctypes.c_char_p)),
        ("num_alias", ctypes.c_uint32),
        ("description", ctypes.c_char_p),
        ("units", ctypes.c_char_p),
        ("flags", ctypes.c_uint32),
        ("len", ctypes.c_uint32),
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


class OsdiInitFailed(Exception):
    pass


def main() -> int:
    ap = argparse.ArgumentParser(description="Compare random OSDI and lifted-Python evals.")
    ap.add_argument("target", nargs="?", default="diode")
    ap.add_argument("cases", nargs="?", type=int, default=8)
    ap.add_argument("seed", nargs="?", type=int, default=1)
    ap.add_argument(
        "--mode",
        choices=("realistic", "conservative"),
        default="realistic",
        help="realistic broadens Verilog-A-visible inputs; conservative keeps the previous narrow setup",
    )
    args = ap.parse_args()
    if args.cases <= 0:
        raise SystemExit("cases must be positive")

    random.seed(args.seed)
    root = Path(__file__).resolve().parent
    verilog = resolve_target(root, args.target)
    module = parse_module(verilog)
    work = Path(tempfile.gettempdir()) / f"mir_lift_compare_{module}"
    work.mkdir(parents=True, exist_ok=True)
    osdi = work / f"{module}.osdi"
    py = work / f"{module}.py"

    compile_osdi(root, verilog, osdi)
    lift_python(root, verilog, py)

    print(f"osdi: {osdi}")
    print(f"python: {py}")
    osdi_results, python_cases, shape, report = run_osdi(osdi, args.cases, args.mode)
    print(f"osdi cases: {len(osdi_results)}")
    print_report(args.mode, report)
    python_results = run_python(py, module, python_cases, shape)
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


def run_osdi(
    path: Path, cases: int, mode: str
) -> tuple[list[dict[str, object]], list[dict[str, object]], dict[str, list[int]], dict[str, object]]:
    lib = ctypes.CDLL(str(path))
    patch_osdi_log(lib)
    require_resolved_lim_table(lib)
    ndesc = ctypes.c_uint32.in_dll(lib, "OSDI_NUM_DESCRIPTORS").value
    if ndesc < 1:
        raise SystemExit("OSDI_NUM_DESCRIPTORS is zero")
    desc = OsdiDescriptor.in_dll(lib, "OSDI_DESCRIPTORS")
    handle = ctypes.c_char_p(b"direct_compare")
    handle_ptr = ctypes.cast(handle, ctypes.c_void_p)
    module_name = desc.name.decode(errors="replace") if desc.name else ""
    shape = osdi_output_shape(desc)
    access = ACCESS(desc.access) if desc.access else None
    params = osdi_params(desc)
    report: dict[str, object] = {
        "model_params_set": 0,
        "instance_params_set": 0,
        "builtin_params_set": 0,
        "builtin_params_skipped": sum(
            1
            for p in params[: int(desc.num_params)]
            if is_builtin_param(p)
            and int(p["kind"]) == PARA_KIND_INST
            and param_spec_for_param(p, module_name) is None
        ),
        "sim_params_set": 0,
        "scalar_params_available": sum(1 for p in params[: int(desc.num_params)] if is_scalar_writable_param(p)),
        "scalar_params_without_spec": sum(
            1
            for p in params[: int(desc.num_params)]
            if is_scalar_writable_param(p) and param_spec_for_param(p, module_name) is None
        ),
        "string_params_skipped": sum(1 for p in params[: int(desc.num_params)] if int(p["ty"]) == PARA_TY_STR),
        "array_params_skipped": sum(1 for p in params[: int(desc.num_params)] if int(p["len"]) != 0),
        "temperatures": [],
        "connected_terminals": [],
        "flag_combos": set(),
        "eval_repeats": [],
        "init_retry_failures": 0,
        "init_attempts": [],
        "param_specs_available": count_matching_param_specs(module_name),
        "param_specs_used": set(),
        "partial": [],
    }
    if any(p["ty"] == PARA_TY_STR for p in params[: int(desc.num_params)]):
        report["partial"].append("string parameters are decoded but not written by this ctypes harness")
    if any(int(p["len"]) for p in params[: int(desc.num_params)]):
        report["partial"].append("array/vector parameters are skipped because descriptor len needs element storage mapping")
    if report["scalar_params_without_spec"]:
        report["partial"].append("numeric parameters without explicit ParamSpec are intentionally left at model defaults")
    if mode == "realistic":
        report["partial"].append("simulator params are supplied to native OSDI and recorded in sim_info; lifted Python currently ignores sim_params")
        if report["builtin_params_skipped"]:
            report["partial"].append("instance builtin params without ParamSpec are left at generated defaults")

    nsolve = max(1, desc.num_nodes)
    out = []
    python_cases = []
    for case_idx in range(cases):
        max_attempts = MAX_INIT_ATTEMPTS if mode == "realistic" else 1
        last_init_error = None
        last_context = None
        for attempt_idx in range(max_attempts):
            if mode == "conservative":
                temperature = random.uniform(275.0, 350.0)
                connected_terminals = random.randint(0, int(desc.num_terminals))
                sim_param_values: dict[str, float] = {}
                model_values: dict[str, object] = {}
                model_given: dict[str, bool] = {}
                instance_values: dict[str, object] = {}
                instance_given: dict[str, bool] = {}
                model_builtin_values: dict[str, float] = {}
                instance_builtin_values: dict[str, float] = {}
                repeats = 1
            else:
                temperature = random_temperature(case_idx)
                connected_terminals = random_connected_terminals(int(desc.num_terminals), case_idx)
                sim_param_values = random_sim_params()
                model_values, model_given, model_builtin_values = random_model_params(
                    desc, params, module_name
                )
                instance_values, instance_given, instance_builtin_values = random_instance_params(
                    desc, params, module_name
                )
                repeats = random.choice([1, 1, 2, 3])
            model = ctypes.create_string_buffer(desc.model_size)
            inst = ctypes.create_string_buffer(desc.instance_size)
            init = OsdiInitInfo()
            sim_paras, _sim_paras_keepalive = make_sim_paras(sim_param_values)
            attempt_report = {
                "model_params_set": 0,
                "instance_params_set": 0,
                "builtin_params_set": 0,
            }
            last_context = {
                "case_index": case_idx,
                "attempt": attempt_idx + 1,
                "temperature": temperature,
                "connected_terminals": connected_terminals,
                "model_params": model_values,
                "model_given": model_given,
                "instance_params": instance_values,
                "instance_given": instance_given,
                "model_builtin_params": model_builtin_values,
                "instance_builtin_params": instance_builtin_values,
                "sim_params": sim_param_values,
            }
            try:
                if access is not None:
                    set_osdi_params(
                        access,
                        desc,
                        ctypes.byref(inst),
                        ctypes.byref(model),
                        params,
                        model_values,
                        model_given,
                        model_builtin_values,
                        {},
                        {},
                        {},
                        attempt_report,
                    )
                desc.setup_model(handle_ptr, ctypes.byref(model), ctypes.byref(sim_paras), ctypes.byref(init))
                check_init(init)
                if access is not None and mode == "realistic":
                    set_osdi_params(
                        access,
                        desc,
                        ctypes.byref(inst),
                        ctypes.byref(model),
                        params,
                        {},
                        {},
                        {},
                        instance_values,
                        instance_given,
                        instance_builtin_values,
                        attempt_report,
                    )
                desc.setup_instance(
                    handle_ptr,
                    ctypes.byref(inst),
                    ctypes.byref(model),
                    temperature,
                    connected_terminals,
                    ctypes.byref(sim_paras),
                    ctypes.byref(init),
                )
                check_init(init)
            except OsdiInitFailed as err:
                last_init_error = err
                report["init_retry_failures"] = int(report["init_retry_failures"]) + 1
                continue
            initialize_instance_layout(desc, inst)
            for key in ("model_params_set", "instance_params_set", "builtin_params_set"):
                report[key] = int(report[key]) + int(attempt_report[key])
            report["param_specs_used"].update(model_values)
            report["param_specs_used"].update(instance_values)
            report["param_specs_used"].update(model_builtin_values)
            report["param_specs_used"].update(instance_builtin_values)
            report["init_attempts"].append(attempt_idx + 1)
            break
        else:
            raise SystemExit(
                f"OSDI init failed after {max_attempts} setup attempts: {last_init_error}\n"
                f"context: {format_case_context(last_context or {})}"
            ) from None

        case_bundle: dict[str, object] = {
            "case_index": case_idx,
            "temperature": temperature,
            "connected_terminals": connected_terminals,
            "num_terminals": int(desc.num_terminals),
            "model_params": model_values,
            "model_given": model_given,
            "model_builtin_params": model_builtin_values,
            "instance_params": instance_values,
            "instance_given": instance_given,
            "instance_builtin_params": instance_builtin_values,
            "sim_params": sim_param_values,
            "evals": [],
        }
        report["temperatures"].append(temperature)
        report["connected_terminals"].append(connected_terminals)
        report["sim_params_set"] = int(report["sim_params_set"]) + len(sim_param_values)
        report["eval_repeats"].append(repeats)

        prev_state_values = [random_state_value(i) for i in range(int(desc.num_states))]
        for step_idx in range(repeats):
            solve_values = random_prev_solve(nsolve, step_idx)
            next_state_values = evolve_next_state(prev_state_values)
            flags = conservative_flags() if mode == "conservative" else random.choice(EVAL_FLAG_COMBOS)
            sim_case = {
                "case_index": case_idx,
                "eval_index": step_idx,
                "abstime": 0.0 if mode == "conservative" else random_abstime(case_idx, step_idx),
                "prev_solve": solve_values,
                "prev_state": prev_state_values,
                "next_state": next_state_values,
                "flags": flags,
                "connected_terminals": connected_terminals,
                "num_terminals": int(desc.num_terminals),
                "sim_params": sim_param_values,
            }
            solve = doubles(solve_values, nsolve)
            prev_state = doubles(prev_state_values, max(1, desc.num_states))
            next_state = doubles(next_state_values, max(1, desc.num_states))
            info = OsdiSimInfo(sim_paras, sim_case["abstime"], solve, prev_state, next_state, flags)
            ret_flags = desc.eval(handle_ptr, ctypes.byref(inst), ctypes.byref(model), ctypes.byref(info))
            case_bundle["evals"].append(sim_case)
            report["flag_combos"].add(flags)
            residual_len = max(1, desc.num_nodes)
            jacobian_resist_len = int(desc.num_resistive_jacobian_entries)
            jacobian_react_len = int(desc.num_reactive_jacobian_entries)
            residual_resist = (ctypes.c_double * residual_len)()
            residual_react = (ctypes.c_double * residual_len)()
            limit_rhs_resist = (ctypes.c_double * residual_len)()
            limit_rhs_react = (ctypes.c_double * residual_len)()
            jacobian_resist = (ctypes.c_double * max(1, jacobian_resist_len))()
            jacobian_react = (ctypes.c_double * max(1, jacobian_react_len))()
            desc.load_residual_resist(ctypes.byref(inst), ctypes.byref(model), residual_resist)
            desc.load_residual_react(ctypes.byref(inst), ctypes.byref(model), residual_react)
            desc.load_limit_rhs_resist(ctypes.byref(inst), ctypes.byref(model), limit_rhs_resist)
            desc.load_limit_rhs_react(ctypes.byref(inst), ctypes.byref(model), limit_rhs_react)
            desc.write_jacobian_array_resist(ctypes.byref(inst), ctypes.byref(model), jacobian_resist)
            desc.write_jacobian_array_react(ctypes.byref(inst), ctypes.byref(model), jacobian_react)
            out.append(
                {
                    "flags": ret_flags,
                    "residual_resist": list(residual_resist),
                    "residual_react": list(residual_react),
                    "limit_rhs_resist": list(limit_rhs_resist),
                    "limit_rhs_react": list(limit_rhs_react),
                    "jacobian_resist": list(jacobian_resist)[:jacobian_resist_len],
                    "jacobian_react": list(jacobian_react)[:jacobian_react_len],
                }
            )
            prev_state_values = next_state_values
        python_cases.append(case_bundle)
    report["flag_combos"] = sorted(report["flag_combos"])
    report["param_specs_used"] = sorted(report["param_specs_used"])
    return out, python_cases, shape, report


def conservative_flags() -> int:
    return ANALYSIS_DC | FULL_CALC_FLAGS | INIT_LIM


def doubles(values: list[float], min_len: int) -> ctypes.Array[ctypes.c_double]:
    arr = (ctypes.c_double * max(1, min_len))()
    for idx, value in enumerate(values[: len(arr)]):
        arr[idx] = value
    return arr


def osdi_params(desc: OsdiDescriptor) -> list[dict[str, object]]:
    count = int(desc.num_params) + int(desc.num_opvars)
    if not desc.param_opvar or count <= 0:
        return []
    entries = ctypes.cast(desc.param_opvar, ctypes.POINTER(OsdiParamOpvar))
    out = []
    for idx in range(count):
        item = entries[idx]
        name = f"param_{idx}"
        if item.name:
            first = item.name[0]
            if first:
                name = first.decode(errors="replace")
        out.append(
            {
                "id": idx,
                "name": name,
                "py_name": python_param_name(name),
                "flags": int(item.flags),
                "ty": int(item.flags) & PARA_TY_MASK,
                "kind": int(item.flags) & PARA_KIND_MASK,
                "len": int(item.len),
            }
        )
    return out


def python_param_name(name: str) -> str:
    return name[1:] if name.startswith("$") else name


def is_builtin_param(param: dict[str, object]) -> bool:
    return str(param["name"]).startswith("$")


def is_scalar_writable_param(param: dict[str, object]) -> bool:
    return int(param["len"]) == 0 and int(param["ty"]) in (PARA_TY_REAL, PARA_TY_INT)


def random_model_params(
    desc: OsdiDescriptor, params: list[dict[str, object]], module_name: str
) -> tuple[dict[str, object], dict[str, bool], dict[str, float]]:
    selected = select_params(params[int(desc.num_instance_params) : int(desc.num_params)], module_name)
    values: dict[str, object] = {}
    given: dict[str, bool] = {}
    builtin_values: dict[str, float] = {}
    for param, spec in selected:
        key = str(param["py_name"])
        value = spec.sample()
        if is_builtin_param(param):
            if int(param["kind"]) == PARA_KIND_MODEL:
                builtin_values[key] = float(value)
        else:
            values[key] = value
            given[key] = spec.given
    return values, given, builtin_values


def random_instance_params(
    desc: OsdiDescriptor, params: list[dict[str, object]], module_name: str
) -> tuple[dict[str, object], dict[str, bool], dict[str, float]]:
    selected = select_params(params[: int(desc.num_instance_params)], module_name)
    values: dict[str, object] = {}
    given: dict[str, bool] = {}
    builtin_values: dict[str, float] = {}
    for param, spec in selected:
        key = str(param["py_name"])
        value = spec.sample()
        if is_builtin_param(param):
            if spec.builtin:
                builtin_values[key] = float(value)
        else:
            values[key] = value
            given[key] = spec.given
    return values, given, builtin_values


def select_params(params: list[dict[str, object]], module_name: str) -> list[tuple[dict[str, object], ParamSpec]]:
    selected = []
    for param in params:
        # Strings, arrays, opvars, and unknown numeric parameters need explicit ABI/storage handling
        # or a named ParamSpec before this harness should perturb them.
        if not is_scalar_writable_param(param):
            continue
        spec = param_spec_for_param(param, module_name)
        if spec is not None:
            selected.append((param, spec))
    return selected


def param_spec_for_param(param: dict[str, object], module_name: str) -> ParamSpec | None:
    name = str(param["py_name"]).lower()
    scope = param_scope(param)
    param_ty = "int" if int(param["ty"]) == PARA_TY_INT else "real"
    for spec in PARAM_SPECS:
        if spec.name != name:
            continue
        if spec.modules and module_name not in spec.modules:
            continue
        if spec.scope != "any" and spec.scope != scope:
            continue
        if spec.value_type != param_ty:
            continue
        if spec.builtin != is_builtin_param(param):
            continue
        return spec
    return None


def param_scope(param: dict[str, object]) -> str:
    kind = int(param["kind"])
    if kind == PARA_KIND_MODEL:
        return "model"
    if kind == PARA_KIND_INST:
        return "instance"
    if kind == PARA_KIND_OPVAR:
        return "opvar"
    return "unknown"


def count_matching_param_specs(module_name: str) -> int:
    return sum(1 for spec in PARAM_SPECS if not spec.modules or module_name in spec.modules)


def random_log_uniform(low: float, high: float) -> float:
    return math.exp(random.uniform(math.log(low), math.log(high)))


def random_temperature(case_idx: int) -> float:
    edge = [233.15, 273.15, 300.0, 350.0, 423.15]
    if case_idx < len(edge):
        return edge[case_idx]
    return random.uniform(230.0, 450.0)


def random_connected_terminals(num_terminals: int, case_idx: int) -> int:
    if num_terminals <= 0:
        return 0
    edge = [0, num_terminals, max(0, num_terminals - 1)]
    if case_idx < len(edge):
        return edge[case_idx]
    return random.randint(0, num_terminals)


def random_sim_params() -> dict[str, float]:
    return {
        "gmin": random_log_uniform(1e-15, 1e-9),
        "minr": random_log_uniform(1e-5, 1e-1),
        "reltol": random_log_uniform(1e-6, 1e-2),
        "abstol": random_log_uniform(1e-15, 1e-9),
        "chgtol": random_log_uniform(1e-18, 1e-12),
        "tnom": random.uniform(250.0, 350.0),
    }


def random_prev_solve(nsolve: int, step_idx: int) -> list[float]:
    edge = [0.0, 1e-12, -1e-12, 1e-3, -1e-3, 0.7, -0.7, 5.0, -5.0]
    out = []
    for idx in range(nsolve):
        if idx + step_idx < len(edge):
            out.append(edge[idx + step_idx])
        else:
            out.append(random.uniform(-8.0, 8.0))
    return out


def random_state_value(idx: int) -> float:
    edge = [0.0, 1e-15, -1e-15, 1e-6, -1e-6, 0.25, -0.25]
    if idx < len(edge):
        return edge[idx]
    return random.uniform(-1.0, 1.0)


def evolve_next_state(prev_state: list[float]) -> list[float]:
    return [value + random.uniform(-0.2, 0.2) for value in prev_state]


def random_abstime(case_idx: int, step_idx: int) -> float:
    if case_idx == 0 and step_idx == 0:
        return 0.0
    return random.choice([1e-15, 1e-9, 1e-6, 1e-3, random.uniform(0.0, 10.0)])


def set_osdi_params(
    access: ACCESS,
    desc: OsdiDescriptor,
    inst_ptr: object,
    model_ptr: object,
    params: list[dict[str, object]],
    model_values: dict[str, object],
    model_given: dict[str, bool],
    model_builtin_values: dict[str, float],
    instance_values: dict[str, object],
    instance_given: dict[str, bool],
    instance_builtin_values: dict[str, float],
    report: dict[str, object],
) -> None:
    by_name = {str(param["py_name"]): param for param in params[: int(desc.num_params)]}
    for name, value in model_values.items():
        param = by_name.get(name)
        if param is not None and bool(model_given.get(name, False)):
            write_osdi_param(desc, access, inst_ptr, model_ptr, param, value, instance=False)
            report["model_params_set"] = int(report["model_params_set"]) + 1
    for name, value in model_builtin_values.items():
        param = by_name.get(name)
        if param is not None:
            write_osdi_param(desc, access, inst_ptr, model_ptr, param, value, instance=False)
            report["builtin_params_set"] = int(report["builtin_params_set"]) + 1
    for name, value in instance_values.items():
        param = by_name.get(name)
        if param is not None and bool(instance_given.get(name, False)):
            write_osdi_param(desc, access, inst_ptr, model_ptr, param, value, instance=True)
            report["instance_params_set"] = int(report["instance_params_set"]) + 1
    for name, value in instance_builtin_values.items():
        param = by_name.get(name)
        if param is not None:
            write_osdi_param(desc, access, inst_ptr, model_ptr, param, value, instance=True)
            report["builtin_params_set"] = int(report["builtin_params_set"]) + 1


def write_osdi_param(
    desc: OsdiDescriptor,
    access: ACCESS,
    inst_ptr: object,
    model_ptr: object,
    param: dict[str, object],
    value: object,
    instance: bool,
) -> None:
    flags = ACCESS_FLAG_SET | (ACCESS_FLAG_INSTANCE if instance else 0)
    ptr = access(inst_ptr, model_ptr, int(param["id"]), flags)
    if not ptr:
        raise SystemExit(f"OSDI access returned NULL for parameter {param['name']!r} id={param['id']}")
    if int(param["ty"]) == PARA_TY_INT:
        ctypes.c_int32.from_address(ptr).value = int(value)
    elif int(param["ty"]) == PARA_TY_REAL:
        ctypes.c_double.from_address(ptr).value = float(value)
    else:
        raise SystemExit(f"unsupported OSDI parameter type for {param['name']!r}")
    given = desc.given_flag_instance(inst_ptr, int(param["id"])) if instance else desc.given_flag_model(model_ptr, int(param["id"]))
    if not given:
        scope = "instance" if instance else "model"
        raise SystemExit(f"OSDI {scope} given flag was not set for parameter {param['name']!r} id={param['id']}")


def make_sim_paras(values: dict[str, float]) -> tuple[OsdiSimParas, tuple[object, ...]]:
    if not values:
        return empty_sim_paras()
    keys = list(values)
    name_bytes = [key.encode() for key in keys]
    names = (ctypes.c_char_p * (len(keys) + 1))()
    vals = (ctypes.c_double * len(keys))()
    for idx, key in enumerate(keys):
        names[idx] = name_bytes[idx]
        vals[idx] = values[key]
    names[len(keys)] = None
    names_str = (ctypes.c_char_p * 1)()
    vals_str = (ctypes.c_char_p * 1)()
    names_str[0] = None
    vals_str[0] = None
    return (
        OsdiSimParas(names, vals, names_str, vals_str),
        (names, vals, names_str, vals_str, *name_bytes),
    )


def initialize_instance_layout(desc: OsdiDescriptor, inst: ctypes.Array[ctypes.c_char]) -> None:
    base = ctypes.addressof(inst)
    node_mapping = (ctypes.c_uint32 * int(desc.num_nodes)).from_address(
        base + int(desc.node_mapping_offset)
    )
    for idx in range(int(desc.num_nodes)):
        node_mapping[idx] = idx

    state_idx = (ctypes.c_uint32 * int(desc.num_states)).from_address(
        base + int(desc.state_idx_off)
    )
    for idx in range(int(desc.num_states)):
        state_idx[idx] = idx


def osdi_output_shape(desc: OsdiDescriptor) -> dict[str, list[int]]:
    entries = ctypes.cast(
        desc.jacobian_entries,
        ctypes.POINTER(OsdiJacobianEntry),
    )
    resist = []
    react = []
    for idx in range(int(desc.num_jacobian_entries)):
        flags = entries[idx].flags
        if flags & JACOBIAN_ENTRY_RESIST:
            resist.append(idx)
        if flags & JACOBIAN_ENTRY_REACT:
            react.append(idx)
    if len(resist) != int(desc.num_resistive_jacobian_entries):
        raise SystemExit(
            "OSDI descriptor resistive jacobian metadata mismatch: "
            f"flags describe {len(resist)}, descriptor says {desc.num_resistive_jacobian_entries}"
        )
    if len(react) != int(desc.num_reactive_jacobian_entries):
        raise SystemExit(
            "OSDI descriptor reactive jacobian metadata mismatch: "
            f"flags describe {len(react)}, descriptor says {desc.num_reactive_jacobian_entries}"
        )
    return {"jacobian_resist": resist, "jacobian_react": react}


def run_python(
    path: Path, module: str, cases: list[dict[str, object]], shape: dict[str, list[int]]
) -> list[object]:
    mod = load_lifted_module(path)
    func = getattr(mod, f"{module}_eval")
    setup_model = getattr(mod, f"{module}_setup_model")
    setup_instance = getattr(mod, f"{module}_setup_instance")
    out = []
    for bundle in cases:
        model_kwargs = {
            "params": bundle.get("model_params", {}),
            "given": bundle.get("model_given", {}),
            "builtin_params": bundle.get("model_builtin_params", {}),
        }
        try:
            model = call_with_supported_kwargs(setup_model, model_kwargs, f"{module}_setup_model")
        except Exception as err:
            last = traceback.extract_tb(err.__traceback__)[-1]
            raise SystemExit(
                f"lifted Python setup_model failed: {type(err).__name__}: {err} at {last.filename}:{last.lineno}\n"
                f"context: {format_case_context(bundle)}"
            ) from None
        connected_terminals = bundle.get("connected_terminals", bundle.get("num_terminals", 0))
        instance_kwargs = {
            "model": model,
            "temperature": bundle.get("temperature", 300.0),
            "params": bundle.get("instance_params", {}),
            "given": bundle.get("instance_given", {}),
            "builtin_params": bundle.get("instance_builtin_params", {}),
            "connected_terminals": connected_terminals,
        }
        try:
            instance = call_with_supported_kwargs(setup_instance, instance_kwargs, f"{module}_setup_instance")
        except Exception as err:
            last = traceback.extract_tb(err.__traceback__)[-1]
            raise SystemExit(
                f"lifted Python setup_instance failed: {type(err).__name__}: {err} at {last.filename}:{last.lineno}\n"
                f"context: {format_case_context(bundle)}"
            ) from None
        for sim_case in bundle.get("evals", []):
            try:
                result = func(instance, model, sim_case)
            except Exception as err:
                last = traceback.extract_tb(err.__traceback__)[-1]
                raise SystemExit(
                    f"lifted Python failed: {type(err).__name__}: {err} at {last.filename}:{last.lineno}\n"
                    f"context: {format_case_context(bundle, sim_case)}"
                ) from None
            if result is None:
                result = {}
            if not isinstance(result, dict):
                raise SystemExit(f"lifted Python eval returned {type(result).__name__}, expected dict")
            none_paths = find_none_values(result)
            if none_paths:
                raise SystemExit(
                    "lifted Python returned None at ABI output path(s): "
                    + ", ".join(none_paths[:16])
                    + (" ..." if len(none_paths) > 16 else "")
                )
            out.append(project_python_result(result, shape))
    return out


def load_lifted_module(path: Path) -> object:
    spec = importlib.util.spec_from_file_location("lifted", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def call_with_supported_kwargs(func: object, kwargs: dict[str, object], func_name: str) -> object:
    sig = inspect.signature(func)
    if any(param.kind == inspect.Parameter.VAR_KEYWORD for param in sig.parameters.values()):
        return func(**kwargs)
    supported = {key: value for key, value in kwargs.items() if key in sig.parameters}
    missing = {
        key: value
        for key, value in kwargs.items()
        if key not in sig.parameters and setup_input_required(value)
    }
    if missing:
        missing_names = ", ".join(sorted(missing))
        supported_names = ", ".join(sig.parameters) or "<none>"
        raise TypeError(
            f"{func_name} missing setup interface for required input(s): {missing_names}; "
            f"signature supports: {supported_names}"
        )
    try:
        return func(**supported)
    except TypeError:
        positional = []
        for name in sig.parameters:
            if name in supported:
                positional.append(supported[name])
        return func(*positional)


def setup_input_required(value: object) -> bool:
    if value is None:
        return False
    if isinstance(value, (dict, list, tuple, set)):
        return bool(value)
    return True


def format_case_context(bundle: dict[str, object], sim_case: object = None) -> str:
    parts = [
        f"case={bundle.get('case_index', '?')}",
        f"temperature={bundle.get('temperature', '?')!r}",
        f"connected_terminals={bundle.get('connected_terminals', '?')!r}",
        f"model_params={short_mapping(bundle.get('model_params', {}))}",
        f"model_given={short_mapping(bundle.get('model_given', {}))}",
        f"instance_params={short_mapping(bundle.get('instance_params', {}))}",
        f"instance_given={short_mapping(bundle.get('instance_given', {}))}",
        f"model_builtin_params={short_mapping(bundle.get('model_builtin_params', {}))}",
        f"instance_builtin_params={short_mapping(bundle.get('instance_builtin_params', {}))}",
        f"sim_params={short_mapping(bundle.get('sim_params', {}))}",
    ]
    if isinstance(sim_case, dict):
        parts.extend(
            [
                f"eval={sim_case.get('eval_index', '?')}",
                f"flags={sim_case.get('flags', '?')!r}",
                f"abstime={sim_case.get('abstime', '?')!r}",
                f"prev_solve={short_sequence(sim_case.get('prev_solve', []))}",
                f"prev_state={short_sequence(sim_case.get('prev_state', []))}",
                f"next_state={short_sequence(sim_case.get('next_state', []))}",
            ]
        )
    return "; ".join(parts)


def short_mapping(value: object, limit: int = 10) -> str:
    if not isinstance(value, dict):
        return repr(value)
    items = list(value.items())
    shown = ", ".join(f"{key!r}: {val!r}" for key, val in items[:limit])
    if len(items) > limit:
        shown += f", ... +{len(items) - limit}"
    return "{" + shown + "}"


def short_sequence(value: object, limit: int = 8) -> str:
    if not isinstance(value, list):
        return repr(value)
    shown = ", ".join(repr(item) for item in value[:limit])
    if len(value) > limit:
        shown += f", ... +{len(value) - limit}"
    return "[" + shown + "]"


def project_python_result(
    result: dict[str, object], shape: dict[str, list[int]]
) -> dict[str, object]:
    projected = dict(result)
    for key, indices in shape.items():
        values = result.get(key, [])
        if not isinstance(values, list):
            raise SystemExit(f"lifted Python {key} is {type(values).__name__}, expected list")
        try:
            projected[key] = [values[idx] for idx in indices]
        except IndexError as err:
            raise SystemExit(
                f"lifted Python {key} has length {len(values)}, cannot project OSDI index {err}"
            ) from None
    return projected


def find_none_values(value: object, prefix: str = "") -> list[str]:
    if value is None:
        return [prefix or "<root>"]
    if isinstance(value, dict):
        out = []
        for key, item in value.items():
            out.extend(find_none_values(item, f"{prefix}.{key}" if prefix else str(key)))
        return out
    if isinstance(value, list):
        out = []
        for idx, item in enumerate(value):
            out.extend(find_none_values(item, f"{prefix}[{idx}]"))
        return out
    return []


def print_report(mode: str, report: dict[str, object]) -> None:
    temps = report.get("temperatures", [])
    connected = report.get("connected_terminals", [])
    repeats = report.get("eval_repeats", [])
    print(f"perturbation mode: {mode}")
    print(
        "perturbed: "
        f"model_params={report.get('model_params_set', 0)}, "
        f"instance_params={report.get('instance_params_set', 0)}, "
        f"builtin_params={report.get('builtin_params_set', 0)}, "
        f"sim_params={report.get('sim_params_set', 0)}, "
        f"eval_flag_combos={len(report.get('flag_combos', []))}"
    )
    if temps:
        print(f"temperature range: {min(temps):.6g}..{max(temps):.6g}")
    if connected:
        print(f"connected terminal counts: {sorted(set(connected))}")
    if repeats:
        print(f"eval repeats per setup: {repeats}")
    skipped = []
    if report.get("scalar_params_without_spec", 0):
        skipped.append(f"{report['scalar_params_without_spec']} numeric-without-spec")
    if report.get("string_params_skipped", 0):
        skipped.append(f"{report['string_params_skipped']} string")
    if report.get("array_params_skipped", 0):
        skipped.append(f"{report['array_params_skipped']} array/vector")
    if skipped:
        print("unperturbed params: " + ", ".join(skipped))
    used_specs = report.get("param_specs_used", [])
    if used_specs:
        print("param specs used: " + ", ".join(str(item) for item in used_specs))
    for item in report.get("partial", []):
        print(f"partial: {item}")


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


def require_resolved_lim_table(lib: ctypes.CDLL) -> None:
    try:
        table_len = ctypes.c_uint32.in_dll(lib, "OSDI_LIM_TABLE_LEN").value
        table_ptr = ctypes.c_void_p.in_dll(lib, "OSDI_LIM_TABLE").value
    except ValueError:
        return
    if not table_ptr or table_len == 0:
        return

    class Lim(ctypes.Structure):
        _fields_ = [("name", ctypes.c_char_p), ("num_args", ctypes.c_uint32), ("func_ptr", ctypes.c_void_p)]

    table = (Lim * table_len).from_address(table_ptr)
    missing = []
    for item in table:
        if item.func_ptr:
            continue
        name = item.name.decode(errors="replace") if item.name else "<unnamed>"
        missing.append(f"{name}/{int(item.num_args)}")
    if missing:
        raise SystemExit(
            "native OSDI requires unresolved limiter callback(s) in OSDI_LIM_TABLE: "
            + ", ".join(missing)
        )


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
        raise OsdiInitFailed(f"flags={info.flags} errors={info.num_errors}")


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
