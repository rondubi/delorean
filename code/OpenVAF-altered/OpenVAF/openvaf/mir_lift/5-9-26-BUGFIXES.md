# Bug Fix Notes - 2026-05-09

This note covers two bugs fixed in the MIR-to-LIR-to-Python comparison path.
The goal is to leave enough context for the next engineer to understand what
failed, why it failed, and how to check that it stays fixed.

## 1. Stale guarded capture output

### Symptoms

Lifted Python sometimes kept an old captured value after a later path failed to
produce a real value. The guard correctly avoided writing `_LIR_UNDEF` into the
output dictionary, but it left the previous value in place.

In plain terms: a cache slot could look valid because it still contained an
older value from another path, even though the current path had no value for it.
That made debugging cache setup misleading and could make eval use stale data.

### Debugging approach

The issue was reduced to a small LIR function with two paths:

- one path writes a capture;
- another path reaches the same capture point with the input still undefined.

The generated Python was then executed directly. The important check was not
only "do not store `_LIR_UNDEF`"; it was also "remove any old value for this key
when the current path has no value."

### Root cause

Guarded captures only skipped the assignment when an input was `_LIR_UNDEF`.
Skipping the assignment was not enough because `_lir_outputs` is a mutable
dictionary reused through the function call. If the key already existed, it
stayed there.

### Fix

When a guarded capture sees an undefined input, the emitted Python now removes
that output key:

```python
_lir_outputs.pop("slot", None)
```

So the output dictionary now matches the current path instead of preserving an
old value.

### Verification

A regression test was added in `src/lir_to_python.rs`:

```text
guarded_capture_clears_stale_output_when_input_is_undefined
```

The test builds a small LIR function, emits Python, runs it, and checks both
cases:

- undefined input returns `{}`;
- defined input returns `{"slot": 9}`.

## 2. `compare_random.sh` harness incomparability

### Symptoms

`compare_random.sh` could report that native OSDI and lifted Python were "not
comparable" even when both sides ran. The native side was collecting OSDI-shaped
outputs such as residual arrays, Jacobian arrays, and flags. The lifted Python
side was returning a dictionary with lifted output names and full internal
arrays.

In plain terms: the harness was asking "are these equal?" before both results
had been translated into the same shape.

### Debugging approach

The comparison was split into two questions:

1. Did native OSDI and lifted Python run the same random input case?
2. Are their observable outputs represented with the same keys and array
   positions before comparison?

The first part was already mostly true: the harness seeded the random generator
and passed the same solve/state/flag data to Python. The second part was the
broken part.

### Root cause

The native OSDI descriptor separates resistive and reactive Jacobian entries.
Lifted Python produced arrays in descriptor order, while the OSDI smoke path
read compact resistive/reactive arrays. The harness did not project the Python
arrays into the same compact shape before comparing them.

That made valid results look incomparable because the harness compared two
different views of the same kind of data.

### Fix

The harness now derives the native output shape from the OSDI descriptor and
uses that shape when reading lifted Python results. In particular, it maps the
full lifted Jacobian arrays onto the compact resistive and reactive arrays used
by native OSDI.

It also rejects `None` in ABI output paths before comparison. That catches
missing lifted outputs early, with a direct error, instead of letting them turn
into confusing numeric mismatches later.

### Verification

The useful checks are:

```sh
cargo test -p mir_lift guarded_capture_clears_stale_output_when_input_is_undefined
./compare_random.sh diode 1 1
./compare_random.sh bsim4 1 1
```

The first check covers the stale capture bug directly. The `compare_random.sh`
checks cover the harness path: build/load native OSDI, generate lifted Python,
run seeded random cases, normalize output shape, and compare like with like.

If a future `compare_random.sh` failure is a real model mismatch, it should now
show up as a specific value mismatch or lifted Python runtime error, not as a
generic "not comparable" result caused by mismatched output shapes.
