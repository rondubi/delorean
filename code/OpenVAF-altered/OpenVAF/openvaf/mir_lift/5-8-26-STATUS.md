# LIR / Python OSDI Lifting Status - 2026-05-08

## Goal

The goal is to lift OpenVAF MIR into a simple imperative LIR, optimize that LIR with small reusable compiler passes, and emit runnable Python with an OSDI-shaped API that can be compared against the native OSDI backend. The lifted Python should come from the same compiler metadata and MIR semantics as the existing MIR-to-LLVM-to-OSDI path, so failures in random comparison isolate lifting bugs rather than differences in hand-written ABI glue.

The intended end state is:

- One simple script invocation to produce lifted Python.
- One simple script invocation to run random OSDI-vs-Python checks with reproducible seeds.
- LIR code that is understandable enough to inspect when a comparison fails.
- Optimization implemented as forward and backward IR passes, not one-off Python emitter tricks.
- Python ABI generation driven by OpenVAF compilation data, not hardcoded BSIM4 knowledge.

## Current State

The lifting path now has a real LIR layer, a Python emitter, a simple lift script, and a random comparison runner. The generated Python for BSIM4 compiles and imports, and its OSDI-shaped entry points can be invoked. The current BSIM4 random comparison still fails at runtime with a `ZeroDivisionError`, so semantic equivalence is not yet established.

The latest root cause under investigation is cache initialization. The native OSDI setup path stores cached init values at the point where those MIR values are defined. The Python path previously forced cached init values through final raw returns, which is not equivalent. A new LIR `capture` statement now models "store this value when it is defined", which is closer to native OSDI setup behavior. This fixed some incorrect cache/default behavior, but at least one BSIM4 cache slot still remains zero on the failing random case.

## Design

### LIR

LIR is a small imperative IR used as the boundary between MIR and Python emission. It has:

- Functions and helper functions.
- Locals declared by first assignment in emitted Python.
- Assignments, expression statements, calls, branches, returns, and captures.
- Expressions for literals, locals, arithmetic, logical operations, calls, and unsupported MIR forms.
- Structured helper functions produced from MIR control-flow structure instead of a giant `_pc` switch.

The intent is that MIR-to-LIR is where MIR-specific semantics are lowered, and LIR-to-Python is a mostly mechanical emitter over a small imperative language.

### Pass Framework

The optimizer is organized around reusable forward and backward pass infrastructure.

Forward passes currently cover constant propagation and expression rewriting. The constant propagation pass is part of the existing forward framework, not a separate emitter-side peephole.

Backward passes currently cover liveness, dead-code elimination, helper live-in computation, optional helper live-ins, helper push-up facts, and related cleanup. The optional-helper-live-in machinery is what tells the Python emitter when a helper argument may be unavailable on some path.

The cleanup pipeline is deliberately bounded. It is not trying to compute an unbounded fixed point. The cleanup rounds were reduced after earlier runs showed that blindly increasing pass counts was the wrong answer.

### Control Flow

The generated Python no longer models the program counter as a global `_pc` variable assigned inside a massive switch. Instead, LIR helpers represent merged blocks or structured regions. Helper tail calls are emitted through a small top-level loop that dispatches explicit call tuples returned from helpers. This avoids Python recursion for remaining tail-call-shaped control flow without reintroducing a large PC switch.

### OSDI-Shaped Python ABI

The Python OSDI API is generated as part of the lifting backend. The relevant wrappers are driven by compiler data, including `CompiledModule`, model and instance parameter interners, init/eval interners, DAE system information, cache slots, cached init values, builtin parameters, and hidden state information.

The lifted Python should expose OSDI-shaped entry points such as model setup, instance setup, and eval. These wrappers are intended to be part of the lifted output, not external metadata glue. The current design keeps the existing MIR-to-LLVM-to-OSDI backend untouched, so native OSDI remains the control path.

### Cache Captures

Native OSDI setup does not gather every cached init value as a final return. It stores each cached value at the MIR instruction that defines it. To mirror that, LIR now has:

```text
capture "value_key" = expr;
```

During Python emission, captures write into a local raw-output dictionary. Captures depending on optional undefined helper inputs are skipped so `_LIR_UNDEF` does not poison the cache. This is a runtime representation of facts computed by the backward pass, not a substitute for correct MIR semantics.

## Effective Changes

- Added LIR scaffolding with a simple imperative shape.
- Routed lifting through a simple shell invocation.
- Added LIR dumping so the lifted IR can be inspected directly.
- Added a random comparison runner for lifted Python vs native OSDI using reproducible seeds.
- Added forward-pass constant propagation.
- Added backward-pass liveness and dead-code elimination.
- Added helper live-in analysis so helper arguments are derived from actual use.
- Removed global forward declarations in emitted Python; variables are introduced by first definition.
- Added helper inlining / cleanup support to reduce unnecessary helper boundaries.
- Added helper tail-call emission through a bounded loop instead of recursive Python calls.
- Added hidden-state/cache slot scaffolding from compiler metadata instead of hardcoded Python placeholders.
- Reworked init cached values from final-return values into LIR captures at definition points.
- Kept the native MIR-to-LLVM-to-OSDI path separate as the comparison control.

## Ineffective Or Retired Changes

- Treating cached init values as final raw returns was wrong. Native OSDI stores cached values at definition points, so the Python lift must do the same.
- A forward return-pruning pass was tried and removed. It pruned branch-local values too aggressively at joins and caused setup to lose important cache/default values.
- A plain `_LIR_UNDEF` runtime sentinel was not enough by itself. It is still useful as the representation of optional helper live-ins, but the decision about optionality must come from LIR pass facts.
- Increasing cleanup rounds was not a real solution. The current direction is to strengthen individual passes and keep the number of rounds bounded.
- External JSON metadata is not the intended source of OSDI ABI shape. The Python OSDI backend uses the compiler's existing compilation data. Standalone tooling may still have generic dump/debug metadata paths, but that should not define BSIM4 ABI behavior.
- Placeholder defaults such as arbitrary zero-valued hidden/cache data caused misleading behavior. They can avoid immediate crashes but do not establish semantic equivalence.

## Verification Performed

Recent checks completed successfully:

```sh
cargo fmt
cargo check -p mir_lift
cargo check -p openvaf
./lift.sh bsim4 -o /tmp/bsim4_capture.py
python3 -m py_compile /tmp/bsim4_capture.py
```

The generated BSIM4 Python imports and its setup wrappers can be invoked. After the capture work, some cache slots are populated with nonzero/default values, and the raw output dictionary is no longer polluted by `_LIR_UNDEF` objects.

Current failing check:

```sh
./compare_random.sh bsim4 1 1
```

This still fails in lifted Python with a `ZeroDivisionError`.

## Known Failure

The current BSIM4 random comparison failure is a division by zero in eval. The denominator comes from an eval parameter loaded from a cache slot. In the failing generated file, that slot corresponds to a raw init value key similar to `v10339`, and the cache index is still zero when eval runs.

The important distinction is:

- The new capture mechanism prevents undefined helper values from being blindly stored.
- It does not prove that every cache slot required by eval is initialized on every path.

The remaining bug is likely in one of these areas:

- A cached init value is defined only on some MIR paths, but eval assumes it is always available.
- The Python eval argument mapping from cache slots to MIR values is wrong.
- The Python setup inputs do not match the native OSDI setup conditions closely enough.
- The capture placement is still missing a valid MIR definition site for this cache value.

This needs to be isolated by tracing the specific cache slot through compiled metadata, MIR value, LIR capture, generated setup Python, and eval argument binding.

## Hygiene

The direction is cleaner than the earlier versions:

- Optimization decisions are represented as LIR pass facts.
- Constant propagation is in the forward-pass framework.
- Liveness, DCE, helper live-ins, optional live-ins, and capture preservation are in the backward-pass framework.
- Python emission consumes LIR and pass facts instead of rediscovering program analysis ad hoc.
- OSDI ABI generation is attached to the lifting backend and uses OpenVAF compilation data.
- The existing native OSDI backend remains unmodified as the control.

There is still hygiene debt:

- The Python emitter knows about `_LIR_UNDEF` and guarded captures. That is acceptable as code generation for optional-live-in facts, but it should stay small and mechanical.
- The comparison runner has accumulated debug history. It should remain simple: build/load OSDI, load Python, generate seeded random cases, compare outputs.
- Unsupported MIR forms and exact OSDI callback semantics should be documented and handled explicitly rather than silently defaulted.
- Cache-slot mapping needs stronger assertions so a missing or zero default is caught at setup boundary rather than later as a numerical crash.

## Next Debugging Questions

The next useful debug work is narrow:

1. Identify the native OSDI cache slot that corresponds to the failing eval denominator.
2. Identify the MIR value assigned to that slot by compiled metadata.
3. Confirm whether native OSDI stores that slot in setup for the same random case.
4. Confirm whether LIR emits a `capture` for that MIR value and whether the capture is reachable.
5. Confirm whether Python eval binds the expected cache slot to the expected MIR eval parameter.

If native OSDI initializes the slot and lifted Python does not, the bug is in MIR-to-LIR capture placement, helper live-ins, or Python setup ABI wiring. If neither initializes it, then the comparison setup is not matching native simulator conditions or the eval call is being made with an invalid state.
