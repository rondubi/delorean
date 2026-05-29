# LIR Format And Lifting Design

This document describes the current `mir_lift` LIR layer and the two main
translations around it: MIR-to-LIR lowering and LIR-to-Python emission. It is
based on the current source in `src/lir.rs`, `src/mir_to_lir.rs`,
`src/lir_to_python.rs`, `src/lir_backward.rs`, `src/lir_forward.rs`,
`src/lir_structure.rs`, and `src/lib.rs`, plus the 2026-05-08 and 2026-05-09
status notes.

## Purpose

LIR is a small imperative IR used as a boundary between OpenVAF MIR and emitted
Python. MIR is SSA-like and compiler-internal; the Python output needs ordinary
assignments, explicit control-flow transfers, and dictionaries of observable
outputs. LIR is the place where those two worlds meet.

The intended split is:

- MIR-to-LIR handles MIR-specific details: SSA values, params, constants, phi
  nodes, MIR opcodes, block slicing, selected return values, and capture points.
- LIR passes simplify and structure that imperative graph without knowing OSDI
  ABI details.
- LIR-to-Python mechanically emits Python from LIR plus pass facts. It should
  not rediscover MIR semantics.
- OSDI-shaped wrapper generation, in the `openvaf` backend integration, maps
  compiler metadata to model/setup/eval functions around the raw lifted Python.

LIR is not a full replacement for MIR. It is intentionally smaller and currently
oriented toward scalar expression lifting and OSDI comparison.

## Core Data Model

The LIR AST is defined in `src/lir.rs`.

```text
Program
  functions: Vec<Function>

Function
  name: String
  params: Vec<LocalId>
  locals: Vec<Local>
  entry: Label
  blocks: Vec<Block>
  returns: Vec<ReturnSlot>

Block
  label: Label
  stmts: Vec<Stmt>
  term: Terminator
```

A function owns a flat local table. `LocalId` is an index into that table, and
each `Local` has a stable id, a `name_hint`, and a best-effort `LirType`
(`Bool`, `Int`, `Real`, `Str`, or `Unknown`). `params` is a list of local ids
whose values are supplied by the raw function caller. `entry` names the first
block. `blocks` form a CFG through block terminators.

`Function::returns` records named return slots for selected materialized locals.
The executable return behavior is carried by `Terminator::Return`, whose values
are expressions. In practice, the lowering path builds terminator returns from
the requested `FunctionUnit.return_values`, while `returns` is metadata about
local-backed return slots.

## Statements

LIR statements are deliberately small:

```text
Assign { dst, value }
Capture { key, value }
Expr(value)
Unsupported { dsts, text }
```

`Assign` writes a local. LIR is no longer pure SSA at this point; phi lowering
and helper transformations use ordinary assignment semantics.

`Capture` records an observable value at the point where it is defined. This is
important for setup/cache behavior: native OSDI setup stores some values when
the defining MIR instruction executes, not only at final function return. In
Python emission, captures write into `_lir_outputs`, keyed by the MIR-style
value name such as `"v10339"`.

`Expr` is used for side-effecting expressions, especially calls whose return is
not materialized.

`Unsupported` is an explicit escape hatch in the LIR type, but the Python emitter
currently treats unsupported LIR statements and expressions as hard errors.
That is intentional: emitting placeholders would hide semantic gaps.

## Terminators

Blocks end in exactly one terminator:

```text
Goto(label)
Branch { cond, then_label, else_label }
Return(Vec<ReturnValue>)
Unreachable
```

`Goto` and `Branch` define the LIR CFG. `Return` produces a dictionary-shaped
raw result through named return values. `Unreachable` lowers to a runtime error
after structuring.

When MIR lowering sees a jump or branch that leaves the selected `FunctionUnit`
slice, it returns the current selected outputs instead of trying to follow
outside blocks.

## Expressions

Expressions are scalar:

```text
Local(local)
Const(Bool | Int | Real | Str | None)
Unary { op, arg }
Binary { op, lhs, rhs }
Call { target, args }
Unsupported { text, args }
```

The supported unary and binary operations cover the arithmetic, comparisons,
casts, bit operations, shifts, and math functions currently lowered by
`src/mir_to_lir.rs`. Calls are represented by target name plus arguments and are
emitted through the Python runtime shim `mir_call(name, *args)`.

The expression model is single-result. Multi-result MIR instructions are not
represented as tuple assignments today; lowering fails if such an instruction
would need expression lowering.

## Params, Locals, Returns, And Captures

MIR params become LIR locals first. Raw lifted Python signatures are derived
from the LIR function params, after name sanitation.

MIR instruction results normally become locals unless the MIR forward pass
proves they can be represented as aliases. Copy aliases, especially
`OptBarrier`, canonicalize multiple MIR values to one LIR local where safe.
Single-use simple expressions may be inlined instead of materialized.

Return values are selected by the caller of the lifting API:

- `lift_text` without metadata defaults to returning params plus SSA results in
  the selected unit.
- `lift_function_with_returns` returns only explicit MIR values requested by the
  integration caller.
- `lift_function_with_returns_and_captures` also asks lowering to emit
  `Capture` statements for explicit MIR values.

Captures are preserved as observable side effects. The emitter guards captures
whose expression reads optional undefined helper inputs. If any input is
`_LIR_UNDEF`, the generated code removes the key from `_lir_outputs` with
`pop(key, None)` so stale values from another path do not survive.

## MIR-To-LIR Responsibilities

`src/mir_to_lir.rs` lowers one `FunctionUnit` into one LIR function.

The lowering context is responsible for:

- building labels for selected MIR blocks;
- creating locals for params and materialized instruction results;
- computing MIR forward facts for copy aliases and inlineable expressions;
- lowering constants, params, results, and invalid values into LIR expressions;
- translating supported unary and binary opcodes into LIR ops;
- translating MIR calls into `Expr::Call`;
- converting phi nodes into explicit edge-copy blocks before the destination;
- using temporary locals for parallel phi copies when the incoming value would
  otherwise overwrite another destination;
- emitting entry captures for capture values that are params or constants;
- emitting captures immediately after the captured value is assigned or copied;
- converting MIR `Jump`, `Branch`, and `Exit` into LIR terminators;
- failing on unsupported instructions, unsupported terminators, or unsupported
  multi-result lowering.

The phi handling is one of the main semantic jobs. LIR blocks do not contain phi
nodes. Instead, an edge from predecessor `P` to destination `D` may be redirected
through a fresh LIR block that assigns each destination phi local from the
incoming edge value and then jumps to `D`.

## Function Units And Text Lifting

`src/lib.rs` builds `FunctionUnit`s before lowering. A unit may be an entire MIR
function or a metadata-selected block slice. Whole-function params are the MIR
params sorted by MIR param index. Slice params are external SSA inputs used by
the selected blocks.

The public text path is:

```text
normalize MIR text
parse MIR functions
parse optional compilation metadata
build FunctionUnit list
lower each unit to LIR
simplify LIR
emit Python
```

The text normalizer strips comments and some address/signature decoration so
`mir_reader` can parse dumped MIR text.

## Pass Pipeline

There are three relevant pass stages.

First, MIR forward facts run during MIR-to-LIR lowering. Today these facts cover
copy aliases, simple expression aliases, and live param collection used by the
OSDI hidden-state discovery path.

Second, `lir_simplify::simplify` runs after lowering. Unless
`MIR_LIFT_DISABLE_LIR_OPTS` is set, it runs the LIR forward pass framework.
The current forward LIR pass is constant propagation plus constant folding and
branch folding.

Third, Python emission structures LIR control flow and runs backward structured
passes. `lir_structure::structure` chooses helper blocks for labels with
multiple incoming transfers and cycle targets, builds a tree of
`StructuredStmt`s, and leaves helper calls where direct inlining would duplicate
joins or cycles. `lir_backward::run_backward_passes` then runs bounded cleanup
rounds: helper forwarding, common-tail helper sinking, helper signature/live-in
recomputation and pruning, cost-based helper inlining, structured simplification,
helper computation push-up, dead assignment removal, and finally optional
helper-live-in analysis.

The pass pipeline is intentionally bounded. It is not an unbounded optimizer
fixed point. Timing and helper stats are available through `MIR_LIFT_TIMING`.

## Structured Control Flow And Helpers

Structured LIR is an emission-only representation:

```text
Stmt(...)
If { cond, then_body, else_body }
CallHelper(label)
Return(values)
Raise(message)
```

Helpers represent control-flow joins and cycles that should not be blindly
duplicated into every predecessor. Backward liveness computes each helper's
parameters from locals used before definition in that helper body. The emitter
then generates Python helper functions with exactly those live-ins.

The public raw Python function does not call helpers recursively forever. Helper
transfers are emitted as return tuples:

```text
("call", helper_function, args_tuple)
```

The outer function runs a small dispatch loop until a helper returns:

```text
("return", result_dict)
```

This preserves tail-call-shaped control flow without reintroducing a large
program-counter switch.

## Optional Live-Ins And `_LIR_UNDEF`

Some helper live-ins are only defined on some incoming paths. The backward pass
records these as `optional_helper_live_ins`. When a helper call needs such a
local and it is not currently defined, the emitter passes `_LIR_UNDEF`.

`_LIR_UNDEF` is a Python sentinel, not a semantic value from MIR. It exists so
the generated helper signatures can stay explicit while still representing
path-dependent definedness. The emitter uses it in two places:

- guarded captures skip and clear outputs when an input is undefined;
- guarded returns avoid writing a return key whose expression depends on an
  undefined local.

For future work, `_LIR_UNDEF` should be viewed as an implementation artifact of
Python emission. Analysis backends should prefer path predicates and liveness
facts over treating `_LIR_UNDEF` as a normal value.

## LIR-To-Python Responsibilities

`src/lir_to_python.rs` emits raw lifted Python from one LIR function.

The emitter is responsible for:

- sanitizing function and local names into Python identifiers;
- invoking LIR structuring and consuming helper live-in facts;
- emitting helper functions, an entry helper, and the trampoline loop;
- emitting assignments, side-effect expressions, branches, helper calls, and
  returns;
- mapping LIR operations to Python operators and `math` functions;
- emitting `mir_call(...)` for MIR calls;
- managing `_lir_outputs` when captures are present;
- guarding optional undefined captures and returns;
- failing on unsupported LIR nodes.

The Python prelude only implements a small runtime surface. `simparam_opt`
returns its default argument, `Display...` calls are no-ops, and names in
`MIR_IGNORED_CALLS` are no-ops. Other calls raise at runtime unless the OSDI
wrapper adds the target to the ignored set or a real implementation is added.

The emitter uses Python arithmetic and Python `math`. That is pragmatic for
comparison work, but it is not a proof of exact LLVM/native numeric equivalence.

## OSDI-Shaped Wrapper Generation

Raw lifted functions have MIR-shaped signatures and return dictionaries keyed by
MIR value names. The OSDI-shaped API is generated around those raw functions by
the `openvaf` backend integration, not by `src/lir_to_python.rs`.

The wrapper generator builds a `PythonOsdiModule` from `sim_back::CompiledModule`
and emits functions such as:

```text
{module}_setup_model(...)
{module}_setup_instance(...)
{module}_eval(...)
{module}_load_residual_resist(...)
{module}_load_residual_react(...)
{module}_load_limit_rhs_resist(...)
{module}_load_limit_rhs_react(...)
{module}_load_jacobian_resist(...)
{module}_load_jacobian_react(...)
```

The core idea is that the ABI bridge is metadata-driven:

- raw model/init/eval function names are assigned before lifting;
- model setup arguments come from the model setup interner;
- instance setup arguments come from the init interner;
- eval arguments come from eval MIR params and `compiled.intern.params`;
- appended eval params beyond the interned parameter list are read from init
  cache slots;
- model and instance parameter outputs come from `PlaceKind::Param`;
- init cache storage comes from `compiled.init.cached_vals`;
- hidden-state slots and hidden outputs are discovered from live hidden-state
  params and `PlaceKind::Var` outputs;
- eval output arrays come from DAE residual and jacobian metadata.

For cache initialization, the important LIR-level design is that cached init
values can be requested as captures. Captures model "store this value when this
MIR value is defined", which is closer to native OSDI setup than forcing every
cache value through the final raw return dictionary.

The wrapper layer is intentionally a data-movement layer. It should not become a
hand-written model implementation.

## Comparison Harness Relationship

`compare_random.sh` runs `direct_compare.py`. The harness builds a native OSDI
artifact and a lifted Python artifact for the same Verilog-A target, then runs
seeded random eval cases through both.

At a high level, the harness:

- compiles native OSDI with `openvaf-driver`;
- generates lifted Python through `mir_lift_runner.py`;
- loads native OSDI with `ctypes`;
- calls native setup and initializes simple node/state layout;
- records random `sim_info` cases;
- runs native eval and reads residual, limit RHS, and jacobian arrays;
- imports the lifted Python module;
- calls the OSDI-shaped Python setup/eval wrappers;
- rejects `None` in ABI output paths;
- projects lifted jacobian arrays into the compact native resistive/reactive
  shapes described by the OSDI descriptor;
- compares dictionaries with numeric tolerance.

This makes the harness a consumer of the OSDI-shaped wrapper surface, not of raw
LIR. When it fails, the failure can be in MIR-to-LIR lowering, LIR-to-Python
emission, wrapper mapping, native-vs-Python numeric semantics, or the harness's
view of OSDI storage. The 2026-05-09 notes describe a fixed harness bug where
native compact jacobian arrays were previously compared against unprojected
lifted arrays.

## SMT Translation Boundaries

LIR is a better starting point for SMT work than generated Python because it has
explicit blocks, locals, expressions, and observable return/capture events. The
core LIR graph before Python structuring is the most useful boundary.

Important boundaries for an SMT backend:

- Treat `Capture` and `Return` as observable events, not ordinary local writes.
- Model block terminators as path constraints and transitions.
- Preserve assignment ordering, especially around phi edge-copy blocks and
  parallel-copy temporaries.
- Do not rely on Python helper structure as semantic input; helpers are an
  emission strategy.
- Replace `_LIR_UNDEF` with definedness/path predicates.
- Decide a numeric theory up front: mathematical reals, bit-vectors, IEEE-754,
  or a mixed abstraction. Current Python emission does not settle that contract.
- Treat `Expr::Call` as unsupported, uninterpreted, or axiomatized per target.
  Many OSDI callbacks are stateful and cannot be reduced to pure scalar calls
  without additional ABI modeling.
- Avoid using OSDI wrapper defaults such as `0.0` fallbacks as semantic facts
  unless they are proven equivalent to the native backend.
- Keep unsupported LIR nodes fail-fast. SMT translation should not silently
  invent meanings for `Unsupported`.

In short, the SMT boundary should be core LIR plus explicit ABI assumptions, not
generated Python text and not the comparison harness's dictionary projection.

## Known Limitations And Risks

The current design is useful but not a full correctness proof.

- Unsupported MIR instructions, unsupported terminators, and multi-result
  expression lowering fail rather than producing partial Python.
- MIR calls are structurally represented, but most call targets do not have
  Python semantics. Some OSDI callbacks mutate validity or topology state.
- Python arithmetic may differ from native LLVM/OSDI behavior for division by
  zero, NaNs, signed zero, integer width/overflow, remainder rules, and libm
  edge cases.
- Setup-phase ABI values such as solve-vector data, integration flags, and some
  state values may still have simplified sources depending on the wrapper path.
- Hidden state is represented with Python instance storage, but exact
  initialization and lifetime equivalence remains a risk.
- Eval flags are currently simplified in the Python wrapper.
- Cache initialization is sensitive to capture reachability and wrapper cache
  mapping. The 2026-05-08 note specifically calls out BSIM4 cache initialization
  as an area that had not yet established semantic equivalence.
- The optimizer is bounded and heuristic in places. Helper inlining and cleanup
  are intended to improve emitted Python shape, not to prove program equivalence.
- `Function::returns` metadata and terminator return expressions should not be
  confused. The actual emitted raw result comes from structured `Return`
  statements.

Future changes should keep the main separation intact: MIR semantics belong in
MIR-to-LIR, control/data-flow cleanup belongs in LIR passes, Python emission
should stay mechanical, and OSDI ABI projection should be generated from
compiler metadata.
