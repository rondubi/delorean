#!/usr/bin/env python3
"""First-pass Sympy symbolic execution for the generated raw BSIM4 eval core."""

from __future__ import annotations

import argparse
import ast
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Tuple

try:
    import sympy as sp
except ImportError as exc:
    raise SystemExit(
        "sympy is required for symex_raw_eval.py. Install it in this Python environment."
    ) from exc


class SymexError(Exception):
    pass


@dataclass
class ReturnValue:
    fields: Dict[str, object]


@dataclass
class Stats:
    assignments: int = 0
    attr_assignments: int = 0
    ifs: int = 0
    helper_calls: int = 0
    math_calls: int = 0
    cast_calls: int = 0
    max_depth: int = 0


class Symex:
    def __init__(
        self,
        module: ast.Module,
        functions: Dict[str, ast.FunctionDef],
        *,
        max_ifs: int,
        max_helper_calls: int,
        max_depth: int,
    ) -> None:
        self.module = module
        self.functions = functions
        self.max_ifs = max_ifs
        self.max_helper_calls = max_helper_calls
        self.max_depth = max_depth
        self.stats = Stats()

    def run(self, func: ast.FunctionDef) -> ReturnValue:
        env = {arg.arg: sp.Symbol(arg.arg, real=True) for arg in func.args.args}
        ret = self.exec_body(func.body, env, depth=0)
        if ret is None:
            self.fail(func, f"{func.name} completed without a return")
        return ret

    def exec_body(
        self, body: Iterable[ast.stmt], env: Dict[str, object], *, depth: int
    ) -> Optional[ReturnValue]:
        self.stats.max_depth = max(self.stats.max_depth, depth)
        if depth > self.max_depth:
            raise SymexError(f"helper recursion depth exceeded --max-depth={self.max_depth}")

        for stmt in body:
            if isinstance(stmt, ast.Assign):
                self.assign(stmt, env)
            elif isinstance(stmt, ast.AnnAssign):
                self.assign_ann(stmt, env)
            elif isinstance(stmt, ast.If):
                ret = self.exec_if(stmt, env, depth=depth)
                if ret is not None:
                    return ret
            elif isinstance(stmt, ast.Return):
                return self.eval_return(stmt, env, depth=depth)
            elif isinstance(stmt, ast.Pass):
                continue
            else:
                self.fail(stmt, f"unsupported statement {type(stmt).__name__}")
        return None

    def assign(self, stmt: ast.Assign, env: Dict[str, object]) -> None:
        if len(stmt.targets) != 1:
            self.fail(stmt, "multi-target assignment is unsupported")
        value = self.expr(stmt.value, env)
        self.write_target(stmt.targets[0], value, env)

    def assign_ann(self, stmt: ast.AnnAssign, env: Dict[str, object]) -> None:
        if stmt.value is None:
            self.fail(stmt, "annotation without a value is unsupported")
        self.write_target(stmt.target, self.expr(stmt.value, env), env)

    def write_target(self, target: ast.expr, value: object, env: Dict[str, object]) -> None:
        if isinstance(target, ast.Name):
            env[target.id] = value
            self.stats.assignments += 1
            return
        if isinstance(target, ast.Attribute) and isinstance(target.value, ast.Name):
            if target.value.id != "_lir_result":
                self.fail(target, f"unsupported attribute target {target.value.id}.{target.attr}")
            result = env.setdefault("_lir_result", {})
            if not isinstance(result, dict):
                self.fail(target, "_lir_result is not a symbolic result record")
            result[target.attr] = value
            self.stats.attr_assignments += 1
            return
        self.fail(target, f"unsupported assignment target {type(target).__name__}")

    def exec_if(
        self, stmt: ast.If, env: Dict[str, object], *, depth: int
    ) -> Optional[ReturnValue]:
        self.stats.ifs += 1
        if self.stats.ifs > self.max_ifs:
            self.fail(stmt, f"branch budget exceeded --max-ifs={self.max_ifs}")
        cond = self.expr(stmt.test, env)

        then_env = clone_env(env)
        else_env = clone_env(env)
        then_ret = self.exec_body(stmt.body, then_env, depth=depth)
        else_ret = self.exec_body(stmt.orelse, else_env, depth=depth)

        if then_ret is not None or else_ret is not None:
            if then_ret is None or else_ret is None:
                self.fail(stmt, "one branch returns and the other falls through")
            return merge_returns(then_ret, else_ret, cond)

        for name in set(then_env) | set(else_env):
            if name == "_lir_result":
                env[name] = merge_result_records(
                    env.get(name), then_env.get(name), else_env.get(name), cond, stmt
                )
                continue
            original_present = name in env
            then_value = then_env.get(name, env.get(name))
            else_value = else_env.get(name, env.get(name))
            if not original_present and (then_value is None or else_value is None):
                continue
            if then_value == else_value:
                env[name] = then_value
            else:
                env[name] = sp.Piecewise((then_value, cond), (else_value, True))
        return None

    def eval_return(
        self, stmt: ast.Return, env: Dict[str, object], *, depth: int
    ) -> ReturnValue:
        if stmt.value is None:
            self.fail(stmt, "bare return is unsupported")
        value = stmt.value
        if isinstance(value, ast.Name):
            result = env.get(value.id)
            if value.id != "_lir_result" or not isinstance(result, dict):
                self.fail(value, f"unsupported return name {value.id}")
            return ReturnValue(dict(result))
        if isinstance(value, ast.Call) and isinstance(value.func, ast.Name):
            callee = value.func.id
            if callee in self.functions:
                return self.call_helper(callee, value.args, env, depth=depth)
        self.fail(stmt, f"unsupported return value {ast.dump(value, include_attributes=False)}")

    def call_helper(
        self, name: str, args: List[ast.expr], env: Dict[str, object], *, depth: int
    ) -> ReturnValue:
        self.stats.helper_calls += 1
        if self.stats.helper_calls > self.max_helper_calls:
            raise SymexError(f"helper-call budget exceeded --max-helper-calls={self.max_helper_calls}")
        func = self.functions[name]
        params = [arg.arg for arg in func.args.args]
        if len(args) != len(params):
            self.fail(func, f"{name} arity mismatch: {len(args)} args for {len(params)} params")
        helper_env = {param: self.expr(arg, env) for param, arg in zip(params, args)}
        ret = self.exec_body(func.body, helper_env, depth=depth + 1)
        if ret is None:
            self.fail(func, f"{name} completed without a return")
        return ret

    def expr(self, node: ast.expr, env: Dict[str, object]) -> object:
        if isinstance(node, ast.Constant):
            if isinstance(node.value, (int, float, bool)):
                return sp.sympify(node.value)
            self.fail(node, f"unsupported constant {node.value!r}")
        if isinstance(node, ast.Name):
            if node.id in env:
                return env[node.id]
            self.fail(node, f"use of unknown symbol {node.id}")
        if isinstance(node, ast.UnaryOp):
            value = self.expr(node.operand, env)
            if isinstance(node.op, ast.USub):
                return -value
            if isinstance(node.op, ast.UAdd):
                return value
            if isinstance(node.op, ast.Not):
                return sp.Not(value)
            self.fail(node, f"unsupported unary operator {type(node.op).__name__}")
        if isinstance(node, ast.BinOp):
            left = self.expr(node.left, env)
            right = self.expr(node.right, env)
            return self.binop(node, left, right)
        if isinstance(node, ast.BoolOp):
            values = [self.expr(value, env) for value in node.values]
            if isinstance(node.op, ast.And):
                return sp.And(*values)
            if isinstance(node.op, ast.Or):
                return sp.Or(*values)
            self.fail(node, f"unsupported bool operator {type(node.op).__name__}")
        if isinstance(node, ast.Compare):
            return self.compare(node, env)
        if isinstance(node, ast.Call):
            return self.call_expr(node, env)
        self.fail(node, f"unsupported expression {type(node).__name__}")

    def binop(self, node: ast.BinOp, left: object, right: object) -> object:
        if isinstance(node.op, ast.Add):
            return left + right
        if isinstance(node.op, ast.Sub):
            return left - right
        if isinstance(node.op, ast.Mult):
            return left * right
        if isinstance(node.op, ast.Div):
            return left / right
        if isinstance(node.op, ast.Pow):
            return left**right
        if isinstance(node.op, ast.Mod):
            return sp.Mod(left, right)
        self.fail(node, f"unsupported binary operator {type(node.op).__name__}")

    def compare(self, node: ast.Compare, env: Dict[str, object]) -> object:
        left = self.expr(node.left, env)
        pieces = []
        for op, comparator in zip(node.ops, node.comparators):
            right = self.expr(comparator, env)
            if isinstance(op, ast.Eq):
                pieces.append(sp.Eq(left, right))
            elif isinstance(op, ast.NotEq):
                pieces.append(sp.Ne(left, right))
            elif isinstance(op, ast.Lt):
                pieces.append(left < right)
            elif isinstance(op, ast.LtE):
                pieces.append(left <= right)
            elif isinstance(op, ast.Gt):
                pieces.append(left > right)
            elif isinstance(op, ast.GtE):
                pieces.append(left >= right)
            else:
                self.fail(node, f"unsupported comparison {type(op).__name__}")
            left = right
        return pieces[0] if len(pieces) == 1 else sp.And(*pieces)

    def call_expr(self, node: ast.Call, env: Dict[str, object]) -> object:
        if isinstance(node.func, ast.Name):
            name = node.func.id
            args = [self.expr(arg, env) for arg in node.args]
            if name == "float":
                self.stats.cast_calls += 1
                return require_arity(name, args, 1, node)[0]
            if name == "bool":
                self.stats.cast_calls += 1
                return sp.Ne(require_arity(name, args, 1, node)[0], 0)
            if name == "int":
                self.stats.cast_calls += 1
                return sp.Function("int")(require_arity(name, args, 1, node)[0])
            if name == "abs":
                return sp.Abs(require_arity(name, args, 1, node)[0])
            if name == "min":
                return sp.Min(*args)
            if name == "max":
                return sp.Max(*args)
            if name == "pow":
                left, right = require_arity(name, args, 2, node)
                return left**right
            self.fail(node, f"unsupported call {name}()")
        if isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Name):
            if node.func.value.id != "math":
                self.fail(node, f"unsupported attribute call {node.func.value.id}.{node.func.attr}()")
            self.stats.math_calls += 1
            return self.math_call(node.func.attr, [self.expr(arg, env) for arg in node.args], node)
        self.fail(node, f"unsupported call form {ast.dump(node.func, include_attributes=False)}")

    def math_call(self, name: str, args: List[object], node: ast.AST) -> object:
        if name == "exp":
            return sp.exp(require_arity(name, args, 1, node)[0])
        if name in ("log", "ln"):
            return sp.log(require_arity(name, args, 1, node)[0])
        if name == "sqrt":
            return sp.sqrt(require_arity(name, args, 1, node)[0])
        if name == "sin":
            return sp.sin(require_arity(name, args, 1, node)[0])
        if name == "cos":
            return sp.cos(require_arity(name, args, 1, node)[0])
        if name == "tan":
            return sp.tan(require_arity(name, args, 1, node)[0])
        if name == "pow":
            left, right = require_arity(name, args, 2, node)
            return left**right
        self.fail(node, f"unsupported math.{name}()")

    def fail(self, node: ast.AST, message: str) -> None:
        location = f"line {getattr(node, 'lineno', '?')}, col {getattr(node, 'col_offset', '?')}"
        raise SymexError(f"{location}: {message}")


def require_arity(name: str, args: List[object], expected: int, node: ast.AST) -> List[object]:
    if len(args) != expected:
        location = f"line {getattr(node, 'lineno', '?')}, col {getattr(node, 'col_offset', '?')}"
        raise SymexError(f"{location}: {name} expects {expected} args, got {len(args)}")
    return args


def clone_env(env: Dict[str, object]) -> Dict[str, object]:
    cloned = dict(env)
    result = env.get("_lir_result")
    if isinstance(result, dict):
        cloned["_lir_result"] = dict(result)
    return cloned


def merge_result_records(
    original: object, then_value: object, else_value: object, cond: object, stmt: ast.AST
) -> Dict[str, object]:
    if then_value is None:
        then_value = original
    if else_value is None:
        else_value = original
    if not isinstance(then_value, dict) or not isinstance(else_value, dict):
        location = f"line {getattr(stmt, 'lineno', '?')}, col {getattr(stmt, 'col_offset', '?')}"
        raise SymexError(f"{location}: result record is not assigned on both branches")
    return {
        field: sp.Piecewise((then_value.get(field), cond), (else_value.get(field), True))
        if then_value.get(field) != else_value.get(field)
        else then_value.get(field)
        for field in set(then_value) | set(else_value)
    }


def merge_returns(then_ret: ReturnValue, else_ret: ReturnValue, cond: object) -> ReturnValue:
    fields = {}
    for field in set(then_ret.fields) | set(else_ret.fields):
        then_value = then_ret.fields.get(field)
        else_value = else_ret.fields.get(field)
        fields[field] = (
            then_value
            if then_value == else_value
            else sp.Piecewise((then_value, cond), (else_value, True))
        )
    return ReturnValue(fields)


def find_raw_eval(functions: Dict[str, ast.FunctionDef]) -> ast.FunctionDef:
    candidates = [
        func
        for name, func in functions.items()
        if name.endswith("_eval_raw") and not name.endswith("_eval_raw_entry")
    ]
    if len(candidates) != 1:
        names = ", ".join(sorted(func.name for func in candidates))
        raise SymexError(f"expected one public raw eval function, found {len(candidates)}: {names}")
    return candidates[0]


def maybe_unwrap_entry(func: ast.FunctionDef, functions: Dict[str, ast.FunctionDef]) -> ast.FunctionDef:
    if len(func.body) != 1 or not isinstance(func.body[0], ast.Return):
        return func
    value = func.body[0].value
    if not isinstance(value, ast.Call) or not isinstance(value.func, ast.Name):
        return func
    callee = value.func.id
    if callee.endswith("_eval_raw_entry") and callee in functions:
        return functions[callee]
    return func


def load_module(path: Path) -> Tuple[ast.Module, Dict[str, ast.FunctionDef]]:
    try:
        tree = ast.parse(path.read_text(), filename=str(path))
    except SyntaxError as exc:
        raise SymexError(f"could not parse {path}: {exc}") from exc
    functions = {node.name: node for node in tree.body if isinstance(node, ast.FunctionDef)}
    return tree, functions


def regenerate_bsim4(output: Path) -> None:
    lift_script = Path(__file__).resolve().parent / "lift.sh"
    cmd = [str(lift_script), "bsim4", "-o", str(output)]
    completed = subprocess.run(cmd)
    if completed.returncode != 0:
        raise SymexError(f"{' '.join(cmd)} failed with exit code {completed.returncode}")


def print_result(result: ReturnValue, outputs: List[str], limit_fields: int) -> None:
    fields = sorted(result.fields)
    selected = outputs or fields[:limit_fields]
    missing = [field for field in selected if field not in result.fields]
    if missing:
        raise SymexError("requested output field(s) not produced: " + ", ".join(missing))
    print(f"symbolic output fields: {len(fields)}")
    for field in selected:
        print(f"{field} = {sp.sstr(result.fields[field])}")
    if not outputs and len(fields) > limit_fields:
        print(f"... {len(fields) - limit_fields} more fields omitted; use --output FIELD")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run bounded Sympy symbolic execution over the generated raw BSIM4 eval core."
    )
    parser.add_argument(
        "--lifted",
        type=Path,
        help="use an existing lifted Python file instead of regenerating BSIM4",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("/tmp/mir_lift_current/bsim4.py"),
        help="where to write regenerated BSIM4 Python when --lifted is not supplied",
    )
    parser.add_argument(
        "--function",
        help="raw function name to execute; defaults to the discovered eval entry function",
    )
    parser.add_argument("--max-ifs", type=int, default=40)
    parser.add_argument("--max-helper-calls", type=int, default=16)
    parser.add_argument("--max-depth", type=int, default=16)
    parser.add_argument("--limit-fields", type=int, default=8)
    parser.add_argument(
        "--output",
        action="append",
        default=[],
        help="specific result field to print; may be repeated",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    lifted = args.lifted or args.out
    if args.lifted is None:
        regenerate_bsim4(lifted)

    module, functions = load_module(lifted)
    raw_eval = functions.get(args.function) if args.function else maybe_unwrap_entry(find_raw_eval(functions), functions)
    if raw_eval is None:
        raise SymexError(f"raw function not found: {args.function}")

    print(f"lifted file: {lifted}", flush=True)
    print(f"symex target: {raw_eval.name}", flush=True)
    print(f"args: {len(raw_eval.args.args)}", flush=True)

    symex = Symex(
        module,
        functions,
        max_ifs=args.max_ifs,
        max_helper_calls=args.max_helper_calls,
        max_depth=args.max_depth,
    )
    result = symex.run(raw_eval)
    print(
        "stats: "
        f"assignments={symex.stats.assignments}, "
        f"result_assignments={symex.stats.attr_assignments}, "
        f"ifs={symex.stats.ifs}, "
        f"helper_calls={symex.stats.helper_calls}, "
        f"math_calls={symex.stats.math_calls}, "
        f"casts={symex.stats.cast_calls}, "
        f"max_depth={symex.stats.max_depth}"
    )
    print_result(result, args.output, args.limit_fields)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SymexError as exc:
        print(f"symex failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
