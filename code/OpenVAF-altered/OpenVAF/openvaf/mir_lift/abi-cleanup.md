# ABI Cleanup Plan

## Current Problem

OSDI ABI behavior is currently split across two places:

- Rust code that generates Python by building strings.
- `direct_compare.py` harness logic that knows how to set up and run OSDI comparisons.

That makes the boundary unclear. Some ABI rules live in generated wrapper text, while other rules live in the comparison harness. This makes the behavior harder to inspect, harder to test, and harder to preserve as the compiler changes.

It also creates risk for future SMT work. LIR should stay target-neutral. If target-specific OSDI behavior leaks into LIR lowering or runner behavior, it becomes harder to reason about the lifted program as a clean intermediate form.

## Goal

Keep LIR target-neutral.

Move OSDI setup and eval behavior into an explicit ABI plan/runtime contract. The compiler should describe what the ABI needs, and the runtime/harness should consume that description consistently.

The contract should be concrete enough for generated Python and `direct_compare.py` to share the same understanding, while still preserving future SMT feasibility. Target-specific ABI behavior should be represented as data and runtime operations, not hidden inside ad hoc generated code paths.

## Immediate Cleanup Plan

After BSIM4 `compare_random` is working:

1. Extract `AbiPlan` from wrapper generation.

   The wrapper generator should build an explicit plan for ABI setup, eval wiring, state layout, and host interactions before emitting Python.

2. Separate planning from Python emission.

   Planning should decide what must happen. Python emission should only render that plan into stable generated code.

3. Move `_pyosdi_*` runtime code out of Rust string concatenation.

   Put the runtime helpers in a template or include file. Rust should reference or embed that runtime as a maintained artifact instead of assembling it through many string fragments.

4. Make `direct_compare.py` consume the same plan.

   The comparison harness should not carry separate ABI knowledge. It should read or receive the same `AbiPlan` used for generated Python, then use that to initialize, evaluate, compare outputs, and interpret state.

5. Keep public generated functions stable during the refactor.

   Existing generated function names, call shapes, and expected outputs should remain stable while internals move behind the plan/runtime boundary. This keeps validation useful and avoids mixing interface churn with ABI cleanup.

## What Not To Do

- Do not add semantic hacks in the runner.
- Do not rewrite source models to make comparisons pass.
- Do not add trampolines to hide ABI mismatches.
- Do not add silent fallbacks when the ABI plan is incomplete.
- Do not add model-specific compiler fixes unless the behavior is represented in the ABI plan.

Failures should be visible and specific. If an OSDI behavior is required, it should be modeled directly.

## ABI Concepts To Model

The ABI plan should explicitly model at least these concepts:

- Parameters, including given values and defaults.
- Builtin parameters.
- Simulation parameters.
- Solve mapping.
- Cache and hidden state.
- Flags and effects.
- Residual and Jacobian layout.
- Host callbacks and limiters.

These concepts should have names, types or shapes where useful, and clear ownership. The important point is that the compiler, generated runtime, and comparison harness all agree on the same ABI description.

## Desired End State

LIR remains a target-neutral representation of lifted behavior.

OSDI-specific behavior lives in an explicit plan plus a small runtime contract. Generated Python and `direct_compare.py` both consume that contract. The result should be easier to audit, easier to test, and less likely to block future symbolic or SMT-based workflows.
