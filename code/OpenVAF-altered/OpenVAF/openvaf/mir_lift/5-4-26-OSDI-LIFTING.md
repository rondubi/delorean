# OSDI-Shaped APIs for Lifted MIR

This note describes the current plan and implementation shape for connecting
OSDI-style entry points to lifted MIR/Python. The goal is to make lifted Python
look enough like the native OSDI backend that random equality checking compares
the same model interface rather than a hand-written adapter.

The important constraint is that the existing MIR-to-LLVM-to-OSDI backend
remains the control path. The lifting path should not change it. Instead, the
lifting backend builds a parallel OSDI-shaped Python surface from the same
compiler data structures that the LLVM OSDI backend uses.

## Shape

The lifted artifact contains two layers.

The first layer is raw lifted MIR:

```text
_{module}_model_raw(...)
_{module}_init_raw(...)
_{module}_eval_raw(...)
```

These functions keep the MIR-shaped calling convention. Their arguments are the
MIR function parameters in `mir::Param` order, and their return values are
dictionaries keyed by MIR value names such as `"v598"`.

The second layer is the OSDI-shaped Python ABI:

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

The OSDI-shaped functions are generated in `openvaf/src/lib.rs` from
`sim_back::CompiledModule`. They are intended to perform the same data movement
that the native OSDI backend performs around the lowered MIR.

## Eval Argument Mapping

The native OSDI eval backend builds `builder.params` by walking
`compiled.intern.params.raw`. Each entry is a `ParamKind`, and each `ParamKind`
is loaded from the OSDI model, instance, or simulation state.

The Python ABI generator follows the same structure:

1. Enumerate all raw eval MIR parameters from `compiled.eval.dfg`.
2. Keep only `ValueDef::Param(param)`.
3. Sort by `usize::from(param)`, matching the raw lifted Python signature.
4. For each param, look up `compiled.intern.params.get_index(param)`.
5. Emit a Python expression for that `ParamKind`.
6. If the param index is beyond `compiled.intern.params.len()`, treat it as an
   appended init cache slot, matching the native backend's `params.extend(cache_vals)`.

The mapping is:

| `ParamKind` | Python ABI source |
| --- | --- |
| `Param(param)` | instance parameter, falling back to model parameter |
| `ParamGiven { param }` | instance given flag, falling back to model given flag |
| `ParamSysFun(param)` | builtin instance parameter with the builtin default |
| `Temperature` | instance temperature |
| `Abstime` | `sim_info["abstime"]` |
| `Voltage { hi, lo }` | `prev_solve[KCL(hi)] - prev_solve[KCL(lo)]`, with missing unknowns as zero |
| `Current(kind)` | `prev_solve[Current(kind)]`, except port current is zero |
| `ImplicitUnknown(eq)` | `prev_solve[Implicit(eq)]` |
| `PortConnected { port }` | unknown index compared with connected terminal count |
| `PrevState(state)` | `sim_info["prev_state"][instance["state_idx"][state]]` |
| `NewState(state)` | `sim_info["next_state"][instance["state_idx"][state]]` |
| `EnableIntegration` | OSDI flags: reactive Jacobian requested and not initial condition |
| `EnableLim` | OSDI enable-limit flag |
| `HiddenState(var)` | currently `instance["hidden"].get(name, 0.0)` |

This is the main place where the Python ABI can be close to correct by
construction: it is not hand-mapping names or positional arguments. It is
walking the same interner and DAE unknown table as the OSDI backend.

## Setup Mapping

The native OSDI setup path does two relevant things:

1. It calls model setup and stores `PlaceKind::Param` outputs back into model
   parameter storage.
2. It calls instance setup and stores instance parameter outputs and init cache
   slots back into instance storage.

The Python ABI wrapper mirrors this at dictionary level:

```text
model = {
  "params": ...,
  "given": ...,
  "builtin_params": ...,
  "raw": ...
}

instance = {
  "model": model,
  "temperature": ...,
  "params": ...,
  "given": ...,
  "builtin_params": ...,
  "state_idx": ...,
  "cache": ...,
  "outputs": ...
}
```

The model wrapper generates raw model setup arguments from
`compiled.model_param_intern.params`, calls `_{module}_model_raw`, then maps
`compiled.model_param_intern.outputs[PlaceKind::Param(param)]` back into
`model["params"][param_name]`.

The instance wrapper generates raw init arguments from `compiled.init.intern`,
calls `_{module}_init_raw`, maps `PlaceKind::Param(param)` outputs back into
`instance["params"]`, and maps `compiled.init.cached_vals` into
`instance["cache"]`.

This is also partly correct by construction: the identity of parameter outputs
and cache slots is not guessed from Python names. It comes from the same
`PlaceKind` and cache-slot maps used by OSDI setup.

## Output Mapping

The eval wrapper does not return arbitrary raw MIR values. It constructs
OSDI-shaped output arrays from the DAE system:

```text
residual_resist
residual_react
limit_rhs_resist
limit_rhs_react
jacobian_resist
jacobian_react
```

For each output group, the generator walks the corresponding DAE residual or
jacobian entries in `compiled.dae_system`. The mapped MIR values are used as
explicit return values for raw eval lifting, and the wrapper reads those values
from the raw eval result dictionary.

This is preferable to returning all MIR intern outputs because the OSDI checker
compares OSDI storage-shaped data. The ordering comes from the DAE residual and
jacobian layout, not from incidental MIR value numbering.

## Why This Can Be Correct By Construction

The core correctness argument is that the bridge is generated from compiler
metadata already used by the trusted backend:

- Raw function argument order is derived from `mir::Param` indices.
- The meaning of each raw argument is derived from `HirInterner.params`.
- Voltage/current/implicit unknown locations are derived from
  `compiled.dae_system.unknowns`.
- Eval output arrays are derived from `compiled.dae_system.residual` and
  `compiled.dae_system.jacobian`.
- Model and instance parameter stores are derived from
  `PlaceKind::Param(param)` outputs.
- Init cache stores are derived from `compiled.init.cached_vals`.
- Builtin parameter defaults come from `ParamSysFun::default_value()`.

If those structures are the same structures consumed by the LLVM OSDI backend,
then the generated Python ABI should have the same high-level wiring: the same
MIR parameters receive the same kinds of OSDI data, and the same MIR outputs are
projected into the same OSDI result arrays.

This is exactly the kind of thing we want: there should be no hand-coded BSIM4
parameter table, no external JSON database, and no checker-side guessing about
which Python argument corresponds to which OSDI field.

## Why This Can Be Incorrect By Construction

There are also ways this approach can be structurally wrong even though it is
metadata-driven.

### Hidden State Is Not Modeled Like OSDI

The LLVM eval backend currently treats `ParamKind::HiddenState(_)` as
unreachable in eval. The Python path has a fallback:

```text
instance["hidden"].get(name, 0.0)
```

That is a pragmatic placeholder, not a proof of equivalence. If hidden state is
actually live in a lifted eval signature, the Python ABI may feed a value the
native backend would not feed, or may silently feed zero.

### Return Flags Are Not Complete

The Python eval wrapper currently returns `"flags": 0`. Native OSDI eval writes
and returns flags for events such as limiting checks, errors, and other simulator
status. If equivalence requires flags, the Python ABI is incomplete.

### Setup Side Effects Are Only Partially Modeled

The Python setup wrapper stores model params, instance params, and cache slots.
It does not fully model every OSDI setup side effect. Node collapse hints,
connectivity storage, invalid-parameter reporting, and error reporting are not
yet represented with the same fidelity as native OSDI.

For diode, this is enough to run a simple random comparison. For BSIM4, this is
not yet a proof.

### Undef And Unlifted Values Are Approximated

Native OSDI has an explicit `BuilderVal::Undef` path. The Python wrapper cannot
faithfully represent LLVM undef. It currently avoids injecting `None` from raw
setup results into parameter/cache storage by falling back to the previous
default value.

That prevents bogus Python crashes from uninitialized cache slots, but it is an
approximation. If the native code relies on undefined values being unreachable,
the Python path should prove the same reachability rather than quietly default.

### Raw Lifted Control Flow Can Be Wrong

The ABI wrapper can be correctly wired and still call incorrect lifted Python.
BSIM4 currently exposes this: the generated Python init path can recurse deeply
enough to hit Python's recursion limit. That is not primarily an ABI mapping
failure; it is a lowered-control-flow/lifting issue in the raw Python function.

The ABI layer should not hide this. It should make the failure visible so we can
debug MIR-to-LIR or LIR-to-Python translation separately.

### Parameter Defaults Depend On Lifted Setup Correctness

The Python model and instance parameter dictionaries are populated by calling
lifted model/init setup and reading `PlaceKind::Param` outputs. That is the
right shape, but it means default correctness depends on setup lifting
correctness.

If model setup lifting drops a value, mishandles a branch, or fails to implement
a callback such as `simparam_opt`, then eval may receive the wrong parameter
even though the ABI projection is generated from the right metadata.

### State Indexing Is Simplified

The Python instance currently initializes `state_idx` as `list(range(n))`. That
matches the simple direct checker shape, but native OSDI stores state indices in
the instance according to setup and simulator allocation. If a simulator uses a
different state layout, this is not sufficient.

## Current Evidence

The strongest current evidence is limited:

```text
./compare_random.sh diode 1
```

passes after the ABI changes. That confirms the path is no longer simply passing
zeroes or fake raw arguments to eval, and that the diode setup/eval/output
mapping is coherent for one random case.

BSIM4 does not yet pass the random checker. It reaches lifted Python setup and
then fails with a Python recursion error in raw init. That should be treated as
the next major lifting problem before making broad correctness claims.

## Practical Test Strategy

The intended debugging split is:

1. Verify the generated Python imports and exposes OSDI-shaped functions.
2. Verify setup wrappers populate model params, instance params, builtin params,
   and cache slots without `None`.
3. Verify eval wrappers build raw MIR arguments without `raw_args` or zero-fill.
4. Run random equality against native OSDI with optimizations disabled.
5. If unoptimized lifting matches, enable LIR passes one at a time.
6. If unoptimized lifting does not match, inspect MIR-to-LIR, LIR-to-Python, and
   ABI mapping separately.

The ABI bridge should stay dumb and generated. The more manually special-cased
it becomes, the more it risks becoming a second model implementation instead of
a compiler-generated view of the same MIR.
