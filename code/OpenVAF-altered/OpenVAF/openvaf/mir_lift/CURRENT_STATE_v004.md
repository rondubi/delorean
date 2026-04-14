# CURRENT_STATE_v004

`mir_lift` currently lifts MIR in programmatic form to nested Python functions.

## Core lowering

- MIR basic blocks are grouped by predecessor-derived source sets.
- A block stays in its predecessor's group only if it has exactly one predecessor.
- Join blocks start a new emitted helper.
- Helpers are emitted as nested `_fn_N(...)` functions.
- Cross-group control flow is lowered as direct helper calls, not as a dispatcher loop.
- Phi values are passed as ordinary helper arguments.

## State handling

- Values used across helper boundaries are hoisted into the enclosing Python function.
- Values used only inside one helper stay local.
- Simple SSA aliases are compressed:
  - direct copies
  - boolean/int/float casts
  - logical negation
  - numeric negation
- Helper inlining is applied conservatively for single-caller helper groups without unsafe cycles.

## Returned values

- Standalone `mir_lift` on raw MIR text still returns the full MIR value environment.
- OpenVAF's `--backend mir-lift` path now uses explicit output roots instead:
  - `model` uses `compiled.model_param_intern.outputs`
  - `init` uses `compiled.init.intern.outputs`
  - `eval` uses `compiled.intern.outputs`
- This means OpenVAF-generated lifted Python no longer returns every SSA value by default.

## Current limitations

- The OpenVAF output-root choice is compiler-driven, not yet a user-defined semantic contract.
- `init` may therefore return an empty dict if no outputs are tracked there.
- `model` may still return many values if many outputs are tracked upstream.
- The standalone MIR CLI and the OpenVAF backend intentionally differ in return-set behavior.
