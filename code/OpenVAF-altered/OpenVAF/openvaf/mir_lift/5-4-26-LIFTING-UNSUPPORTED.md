# Unsupported Items In MIR/LIR Python Lifting

As of 2026-05-05, the MIR-to-LIR and LIR-to-Python path is intentionally
fail-fast for semantic gaps. The remaining unsupported items are mostly not
ordinary arithmetic or control-flow lowering problems. They are places where the
lifted Python must reproduce OSDI ABI state, simulator callbacks, or native
numeric semantics.

## Supported Baseline

These items are not currently unsupported:

- Basic MIR control flow: `Jump`, `Branch`, and `Exit`.
- MIR phi nodes: lowered as edge copies before entering the destination block.
  The generated Python should not contain `PhiNode` placeholders.
- Unary MIR operations currently present in the generated opcode set:
  boolean/integer not, integer/float negation, scalar casts, optimization
  barrier passthrough, and one-argument math functions such as `sqrt`, `exp`,
  `ln`, `log10`, `floor`, trigonometric, and hyperbolic operations.
- Binary MIR operations currently present in the generated opcode set:
  integer/float arithmetic, integer bit operations, integer shifts,
  comparisons, `hypot`, `atan2`, and `pow`.
- Simple MIR call shape: call arguments and single results can be represented
  in LIR and emitted to Python as `mir_call(...)`.

The important caveat is the last item: a MIR call can be structurally lowered
while still lacking a correct Python implementation for the callee semantics.

## Unsupported Or Semantically Unsafe Items

### 1. Hidden State ABI Values

Hidden state is no longer treated as an entirely unsupported category. The
Python OSDI wrapper collects live hidden-state parameter references with the MIR
forward-pass framework, takes the union with hidden-state outputs, preallocates
slots on the Python instance, reads those slots for setup/eval inputs, and copies
lifted raw hidden outputs back into the instance.

The remaining risk is semantic initialization and lifetime matching. Lazy slot
creation gives each referenced state a storage location, but exact equivalence
still requires those slots to be initialized and updated in the same phase as the
native OSDI path. This is especially important for states whose initial value is
not semantically zero.

Model-specific examples include diode hidden values such as `tdev`, `vt`,
`is_t`, `rs_t`, `vd`, `id`, `qd`, `cd`, and `gd`. BSIM4 contains many more; the
exhaustive set is derived from the model-specific `ParamKind::HiddenState`
entries referenced by the compiled MIR and the hidden-state outputs in the salsa
compilation database.

### 2. MIR Runtime Calls Without Semantic Implementations

The Python runtime shim currently only handles:

- `simparam_opt`: returns the MIR-provided default argument.
- `Display...`: treated as a diagnostic no-op.

All other `mir_call(...)` targets are unsupported unless a semantic
implementation is added. Calls seen in generated BSIM4/diode lifted output
include:

- `set_Invalid(Parameter { id: ParamId(0) })` through
  `set_Invalid(Parameter { id: ParamId(12) })`.
- `collapse_node0_Some(node4)`.
- `collapse_node1_Some(node7)`.
- `collapse_node2_None`.
- `collapse_node2_Some(node5)`.
- `collapse_node3_Some(node1)`.
- `collapse_node3_Some(node8)`.
- `collapse_node3_Some(node10)`.
- `collapse_node7_Some(node6)`.
- `collapse_node9_Some(node3)`.

These cannot be simply supported as pure scalar functions. `set_Invalid(...)`
mutates parameter-validity state used by the OSDI initialization path.
`collapse_node...` calls alter topology/node mapping state. Correct support
must update the same ABI-visible state that the native backend updates, not just
return a number.

### 3. Setup-Phase Parameters With No Python ABI Source

Some setup-phase parameter kinds are still emitted as `0.0` when the wrapper
does not know how to source them:

- `Voltage`.
- `Current`.
- `ImplicitUnknown`.
- `Abstime`.
- `EnableIntegration`.
- `EnableLim`.
- `PrevState`.
- `NewState`.
- Any parameter not found in the interned parameter map.

This is not simply supportable because setup code does not receive the same
evaluation context as eval code. Several of these values are simulator state,
solver state, or integration-mode state. Correct support requires threading the
same setup ABI inputs that native OSDI receives, or proving that a given
parameter kind cannot occur in setup for the specific function being lifted.

### 4. Eval Port Currents And Missing DAE Unknowns

Eval lowering still has unsafe zero fallbacks for:

- `Current(Port(_))`.
- Any DAE unknown that `python_osdi_solve_expr` cannot map to a solve-vector
  index.

These cannot be simply supported by returning zero. A missing DAE unknown may
mean either "this value is physically absent" or "the Python ABI mapping is
wrong." Those cases must be distinguished using the same topology, unknown
ordering, and solve-vector metadata used by the native OSDI backend.

### 5. Multi-Result MIR Instructions

The current LIR expression model is single-result. A MIR instruction with more
than one result will fail if it lowers to an expression.

This is not simply supportable by picking the first result. Correct lowering
would need tuple-like LIR values, destructuring assignments, per-result liveness,
and return/output mapping for each produced value. The current generated opcode
set used by diode and BSIM4 has not required this for ordinary arithmetic.

### 6. Unsupported LIR Nodes

`Stmt::Unsupported` and `Expr::Unsupported` remain in the LIR type as explicit
escape hatches, but Python emission now treats them as hard errors.

This is intentional. If an optimization or lowering pass creates one of these
nodes, the compiler has lost semantic information. Emitting a placeholder would
hide the bug and make equivalence failures harder to diagnose.

### 7. Exact Native Numeric Semantics

The generated Python uses Python arithmetic and `math` library calls. This is
not guaranteed to match native LLVM/OSDI behavior for all cases, including:

- Floating-point division by zero.
- NaN payload/sign behavior.
- Signed zero behavior.
- Integer width and overflow.
- Integer division and remainder sign rules.
- libm domain and errno/exception behavior.

This is not simply supportable with local expression rewrites. Exact equivalence
requires choosing and implementing the same numeric contract as the native
backend for every operation, especially around undefined, implementation-defined,
or simulator-defined edge cases.

## Practical Interpretation

For ordinary straight-line MIR arithmetic and control flow, unsupported lowering
should now surface as a compiler error rather than as generated Python
placeholders. For OSDI equivalence, the remaining hard work is ABI behavior:
topology-changing callbacks, validity side effects, solve-vector/unknown
mapping, and exact native numeric edge cases. Those must come from the same
compilation metadata and OSDI ABI construction used by the native backend; they
cannot be repaired by hardcoded defaults in lifted Python.
