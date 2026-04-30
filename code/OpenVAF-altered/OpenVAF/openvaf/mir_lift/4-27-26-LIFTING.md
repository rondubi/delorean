# Lifting Overview (2026-04-27)

This note describes how lifting currently works in `openvaf/mir_lift`, based on the implementation in [src/lib.rs](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:13), the CLI wrapper in [src/main.rs](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/main.rs:6), and the OpenVAF integration in [../openvaf/src/lib.rs](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/openvaf/src/lib.rs:293).

## 1. What the crate does

`mir_lift` takes OpenVAF MIR and emits best-effort Python. The output is not a general MIR interpreter. It is a source-to-source lifter that:

- parses MIR text into `mir::Function` objects,
- groups basic blocks into larger Python helper regions,
- lowers liftable MIR instructions into Python expressions/statements,
- preserves SSA merge semantics with explicit PHI argument passing,
- falls back to `mir_unlifted(...)` for instructions or control-flow cases it cannot render directly.

The main public entrypoints are:

- `lift_text(input, metadata_json)` for textual MIR plus optional slicing metadata: [src/lib.rs:13](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:13)
- `lift_function(function, resolver)` for lifting a whole in-memory MIR function with default return behavior: [src/lib.rs:40](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:40)
- `lift_function_with_returns(function, return_values, resolver)` for lifting an in-memory MIR function with an explicit return set: [src/lib.rs:45](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:45)

Both code paths prepend the generated Python with:

- `import math`
- a stub `mir_unlifted(text)` function that returns `"<unlifted:...>"`

That prelude is emitted in `lift_text` and `emit_function_unit`: [src/lib.rs:23](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:23), [src/lib.rs:54](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:54).

## 2. Input normalization before parsing

The textual MIR path does a normalization pass before calling `mir_reader::parse_functions`: [src/lib.rs:14](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:14), [src/lib.rs:63](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:63).

That pass currently does three things:

1. It drops line comments that begin with `//`: [src/lib.rs:68](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:68)
2. It strips leading `@<hex...>` prefixes from lines, which appear to be MIR text annotations not needed by the parser: [src/lib.rs:71](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:71)
3. It rewrites some function/signature spelling so parsed names are stable:
   - `inst123 = fn ...` becomes `fn123 = fn ...`
   - `call inst123` is rewritten to `call fn123`
   - function names containing punctuation are sanitized to `_`

That logic is all in `normalize_signature_decl`: [src/lib.rs:89](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:89).

The consequence is that lifting from text is intentionally a little forgiving about MIR spelling and naming artifacts.

## 3. How functions are chosen for lifting

The crate lifts a list of `FunctionUnit`s, not raw MIR functions. `build_units` constructs those units from parsed functions plus optional metadata: [src/lib.rs:184](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:184).

There are two modes.

### 3.1 No metadata: lift each full MIR function

If `metadata.functions` is empty, each parsed MIR function becomes one `FunctionUnit::whole(...)`: [src/lib.rs:185](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:185), [src/lib.rs:223](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:223).

For whole-function lifting, the unit records:

- all blocks in layout order,
- the entry block,
- per-block instruction lists,
- predecessor lists filtered to the selected block set,
- formal parameters collected from MIR `ValueDef::Param`,
- return values.

Whole-function return values default to every parameter plus every SSA result encountered in the selected block set: [src/lib.rs:231](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:231), [src/lib.rs:367](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:367).

That is why the standalone `lift_text` path returns a large environment map by default.

### 3.2 Metadata present: lift whole functions or slices

If metadata is present, each metadata record becomes a unit: [src/lib.rs:196](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:196).

The metadata schema is:

```json
{
  "functions": [
    {
      "name": "helper",
      "mir_name": "source_function_name",
      "blocks": ["block2", "block3"]
    }
  ]
}
```

Schema fields are defined in `CompilationDb` and `FunctionMetadata`: [src/lib.rs:146](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:146).

Behavior:

- If `blocks` is omitted or empty, the named output function is a renamed whole-function lift: [src/lib.rs:208](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:208)
- If `blocks` is present, `FunctionUnit::slice(...)` lifts only those blocks: [src/lib.rs:213](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:213), [src/lib.rs:282](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:282)

Slice lifting infers Python parameters by scanning block arguments and collecting any value defined outside the slice:

- MIR params are always external
- constants are not parameters
- instruction results whose defining block is outside the slice are parameters

That logic is in `collect_slice_params`: [src/lib.rs:404](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:404).

So a slice becomes a Python helper with an interface inferred from cross-slice data dependencies, not from original MIR signature boundaries.

## 4. High-level emission strategy

Each `FunctionUnit` is emitted as one top-level Python function: [src/lib.rs:434](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:434).

Inside that function, `emit_grouped_body(...)` does the real work: [src/lib.rs:451](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:451).

The body-generation pipeline is:

1. Partition MIR blocks into block groups.
2. Decide which groups can be inlined.
3. Compute alias information.
4. Compute which values must be materialized as shared state.
5. Compute which return roots can stay local and be stored in `_ret`.
6. Emit one nested `_fn_<id>(...)` helper for each non-inlineable group.
7. Call the entry helper.
8. Return a Python dictionary of requested outputs.

This crate therefore lowers a MIR function into:

- one outer Python function,
- zero or more nested helper functions,
- direct nested calls between helper functions instead of a dispatcher loop or explicit block program counter.

## 5. How block grouping works

Grouping is driven entirely by predecessor structure in `build_block_groups`: [src/lib.rs:516](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:516).

The grouping rule is:

- the entry block is always a leader,
- any block with exactly one predecessor inherits its predecessor’s leader,
- any block with zero or multiple predecessors starts a new leader/group.

In effect:

- straight-line chains collapse into the same helper,
- join points start new helpers,
- blocks reached from a single predecessor continue inside the same helper,
- the grouping ignores semantic shape and uses CFG predecessor count only.

This is the basis for the “predecessor-derived source set” description in the crate docs.

## 6. Shared state and return-root classification

Before emitting helpers, the crate decides which SSA values need outer-scope storage and which can remain local.

### 6.1 Cross-group materialized values

`cross_group_materialized_values(...)` scans every instruction argument and asks whether the referenced value is defined in a different block group: [src/lib.rs:651](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:651).

It resolves through:

- direct copy aliases via `OptBarrier`
- simple unary expression aliases such as casts and negations

The boundary test is in `crosses_group_boundary`: [src/lib.rs:1572](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1572).

If a value crosses a group boundary, the crate materializes its canonical root into shared outer state via `collect_materialized_values`: [src/lib.rs:1593](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1593).

Those shared values are then emitted in the outer Python function as `vX = None` initializations, except for formal parameters: [src/lib.rs:469](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:469).

### 6.2 Localized return roots

`localized_return_roots(...)` identifies returned SSA roots that do not need cross-group materialization: [src/lib.rs:671](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:671).

The rule in `collect_localized_roots(...)` is:

- params, consts, and invalid values are ignored,
- a result value is “localized” only if it is returned and not used as cross-group shared state.

Those values are stored in `_ret[...]` at the point where they are produced: [src/lib.rs:1635](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1635), [src/lib.rs:1650](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1650).

This `_ret` map is not the final return structure by itself. It is a scratch area that preserves values that stayed local to a helper but still need to appear in the final returned dictionary.

## 7. How helper groups are emitted

Each non-inlineable block group becomes:

```python
def _fn_<group_id>(phi_args...):
    nonlocal ...
    ...
```

This is emitted in `emit_group_function`: [src/lib.rs:557](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:557).

Important details:

- helper parameters are the PHI results at the group’s first block: [src/lib.rs:571](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:571)
- helper-local `nonlocal` declarations are generated only for shared outer-state values assigned by that group or any inlineable descendants reachable from it: [src/lib.rs:578](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:578), [src/lib.rs:626](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:626)
- PHI results are immediately bound either to shared state variables or to alias strings: [src/lib.rs:590](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:590)

The outer function later enters execution by calling the helper for the entry group: [src/lib.rs:496](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:496).

## 8. PHI handling

PHI handling is central to the current design.

### 8.1 PHIs are not emitted as ordinary instructions

`emit_group_block(...)` skips `InstructionData::PhiNode(_)` entirely during ordinary instruction emission: [src/lib.rs:936](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:936).

Instead, PHIs are treated as helper-function interface values.

### 8.2 Group-entry PHI results become helper parameters

`group_phi_results(...)` collects the leading PHIs of a block: [src/lib.rs:690](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:690).

The first block of each group uses those PHI results as `_fn_N(...)` parameters. That means SSA merge values are modeled as explicit arguments supplied by predecessor transitions.

### 8.3 Predecessor transitions pick the correct PHI edge value

When control leaves one group for another, `transition_to_group(...)`:

1. looks up the destination’s PHI results,
2. finds the PHI edge value associated with the predecessor block,
3. renders those incoming values as Python expressions,
4. either binds them inline or passes them to the destination helper call.

That logic is in [src/lib.rs:1112](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1112), especially the call to `phi_edge_val(phi, pred)`: [src/lib.rs:1139](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1139).

So the current implementation preserves PHI semantics by turning them into explicit predecessor-to-successor argument passing.

## 9. Control-flow lowering

Recursive emission is handled by `emit_group_block(...)`: [src/lib.rs:899](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:899).

### 9.1 Intra-group flow

If a `Jump` stays inside the same group, emission continues recursively in the destination block without creating another helper call: [src/lib.rs:960](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:960).

If a `Branch` stays inside the same group on one or both arms, those arms become ordinary Python `if ... else ...` subtrees: [src/lib.rs:999](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:999).

### 9.2 Inter-group flow

If control goes to a different group, `transition_to_group(...)` is used: [src/lib.rs:979](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:979), [src/lib.rs:1039](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1039), [src/lib.rs:1081](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1081).

For non-inlineable destination groups, the emitted code is literally:

```python
return _fn_<dst>(...)
```

That return-based call chaining is emitted here: [src/lib.rs:1190](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1190).

This means helper calls serve as the current control-flow transfer mechanism. The implementation does not synthesize a loop/dispatcher or block label machine.

### 9.3 Backedge / already-emitted handling

If recursive emission reaches an already emitted block, the code falls back to `transition_to_group(...)` using that block as both destination and predecessor marker: [src/lib.rs:916](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:916).

That is part of how the emitter avoids endlessly re-expanding already visited structure.

## 10. Inlineable-group heuristic

Some groups are not emitted as standalone nested helpers. Instead they are inlined into the predecessor group when safe.

This decision is made by `compute_inlineable_groups(...)`: [src/lib.rs:700](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:700).

A group is inlineable only if all of the following hold:

- it is not the entry group,
- it has no internal cycle: `group_has_internal_cycle(...)` [src/lib.rs:763](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:763)
- it has exactly one cross-group callsite,
- it has exactly one caller group,
- inlining would not introduce recursion through caller reachability checks: [src/lib.rs:754](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:754)

If a transition targets an inlineable group, the code does not emit `return _fn_X(...)`. Instead it:

- binds the incoming PHI values directly,
- then continues recursively into the destination group’s blocks.

That path is in `transition_to_group(...)`: [src/lib.rs:1157](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1157).

So helper creation is conservative. Straight-line or single-caller non-cyclic regions may collapse back into surrounding code, but join-heavy or cyclic regions remain explicit helpers.

## 11. Instruction lifting

Instruction emission is split between `emit_inst(...)` and `lift_inst(...)`: [src/lib.rs:1201](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1201), [src/lib.rs:1277](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1277).

### 11.1 Lifted instruction classes

Currently the direct lifting cases are:

- unary MIR ops handled by `unary_expr(...)`: [src/lib.rs:1337](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1337)
- binary MIR ops handled by `binary_expr(...)`: [src/lib.rs:1369](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1369)
- `Call { func_ref, args }`, lowered to `sanitized_name(arg0, arg1, ...)`: [src/lib.rs:1297](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1297)

Unary support includes:

- logical/bitwise-not as Python `not`
- numeric negation
- int/float/bool casts
- `OptBarrier` as identity
- many math functions such as `sqrt`, `exp`, `log`, trigonometric functions, and hyperbolic functions

Binary support includes:

- integer/float arithmetic
- shifts and bit ops
- comparisons
- `hypot`, `atan2`, and `pow`

### 11.2 Non-lifted instructions

If `lift_inst(...)` returns `None`, the emitter preserves the MIR text as a comment and assigns or calls `mir_unlifted(...)`: [src/lib.rs:1254](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1254).

That fallback applies to:

- unsupported opcodes,
- any non-terminator instruction not explicitly handled,
- unexpected terminators at the control-flow layer.

This is why the output is “best-effort” Python rather than guaranteed executable semantics for every MIR program.

## 12. Alias compression

The crate intentionally compresses some SSA noise.

### 12.1 Direct copy aliases

`compute_copy_aliases(...)` treats unary `OptBarrier` as a direct copy and canonicalizes chains of those copies: [src/lib.rs:1497](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1497), [src/lib.rs:1537](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1537), [src/lib.rs:1544](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1544).

### 12.2 Simple unary expression aliases

`compute_expr_aliases(...)` records simple unary ops as inlineable aliases: [src/lib.rs:1518](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1518).

These include:

- logical negation,
- numeric negation,
- scalar casts.

The supported alias opcodes are defined in `is_inline_alias_opcode(...)`: [src/lib.rs:1555](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1555).

### 12.3 Where aliases are used

`value_expr(...)` first checks emitted alias strings, then canonical copy aliases, then expression aliases before falling back to raw value names or constants: [src/lib.rs:1459](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1459).

`emit_inst(...)` prefers to keep single-result aliasable expressions out of named temporaries unless the value must exist as shared state: [src/lib.rs:1234](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1234).

The practical result is that the Python output is deliberately less SSA-literal than the MIR.

## 13. Return-value behavior

The emitter always returns a Python dictionary keyed by MIR value name strings: [src/lib.rs:1395](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1395).

Return expression selection works like this:

1. resolve through copy aliases,
2. if the value is a localized return root, read it from `_ret[...]`,
3. else if there is a live alias string, use that,
4. else if there is an expression alias, reconstruct that expression,
5. else render the raw value/constant name.

That logic is in `return_value_expr(...)`: [src/lib.rs:1427](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1427).

There are two distinct return-set policies today:

- `lift_text(...)` with no metadata returns all params plus all SSA results in the selected unit
- `lift_function_with_returns(...)` returns only the explicit roots supplied by the caller, deduplicated by `dedup_values(...)`: [src/lib.rs:255](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:255), [src/lib.rs:332](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:332)

## 14. OpenVAF backend integration

The `openvaf` crate’s `compile_with_mir_lift(...)` path is what powers `openvaf-driver --backend mir-lift`: [../openvaf/src/lib.rs:293](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/openvaf/src/lib.rs:293).

For each compiled module it:

1. builds three MIR functions:
   - model parameter setup
   - init
   - eval
2. renames them to `<module>_model`, `<module>_init`, and `<module>_eval`: [../openvaf/src/lib.rs:319](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/openvaf/src/lib.rs:319)
3. extracts explicit output roots from:
   - `model_param_intern.outputs`
   - `init.intern.outputs`
   - `intern.outputs`
4. calls `mir_lift::lift_function_with_returns(...)` for each function: [../openvaf/src/lib.rs:323](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/openvaf/src/lib.rs:323), [../openvaf/src/lib.rs:346](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/openvaf/src/lib.rs:346)
5. concatenates the lifted Python units, stripping duplicate preludes after the first one: [../openvaf/src/lib.rs:383](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/openvaf/src/lib.rs:383)

This is an important current distinction: the OpenVAF backend does not use the “return every SSA value” policy. It uses compiler-selected output roots.

The standalone CLI in this crate is much thinner. It just reads MIR text, optional metadata JSON, calls `lift_text`, and writes the Python file: [src/main.rs:6](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/main.rs:6).

## 15. What “currently works” well

From the current implementation, the lifting strategy is strongest in these cases:

- mostly structured CFGs where predecessor-count grouping lines up with source structure,
- straight-line arithmetic/logical code,
- MIR with simple PHI joins,
- cases where selective inlining can eliminate one-off helper wrappers,
- analysis/debugging scenarios where “best effort” readable Python is more important than exact executable coverage for every instruction kind.

## 16. Current limitations visible in code

These are the main limitations I would call out from the implementation itself.

### 16.1 Control flow is structural, not semantic

Grouping is based only on predecessor count, not dominance, loop structure, or higher-level control reconstruction: [src/lib.rs:516](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:516).

That is simple and pragmatic, but it means the emitted helper boundaries are CFG-driven rather than source-driven.

### 16.2 Unsupported MIR stays partially opaque

Unsupported instructions are preserved as comments and `mir_unlifted(...)` placeholders, not interpreted: [src/lib.rs:1254](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1254).

So output readability degrades gracefully, but full semantic executability is not guaranteed.

### 16.3 Return behavior differs by caller

Standalone text lifting and OpenVAF backend lifting intentionally differ in what they return:

- standalone: full visible MIR environment for the unit
- backend: explicit compiler-selected outputs

That difference is not accidental; it is encoded directly in which public API gets called.

### 16.4 Localized return tracking is deliberately partial

`maybe_store_localized_alias(...)` contains a no-op branch for some expression-alias cases: [src/lib.rs:1650](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1650).

That suggests localized return caching for alias-only values is intentionally conservative or not fully generalized.

### 16.5 Terminator coverage is narrow

The control-flow layer explicitly understands:

- `Jump`
- `Branch`
- `Exit`

Anything else falls through to `mir_unlifted(...)` plus `return`: [src/lib.rs:1100](/home/ron/delorean/code/OpenVAF-altered/OpenVAF/openvaf/mir_lift/src/lib.rs:1100).

### 16.6 The emitted Python is a readable lowering, not an ABI

Function names, helper names, return dict keys, and slice parameter inference are implementation choices intended to be useful, not a stable formal contract.

## 17. Bottom line

The current lifting design is:

- parse MIR,
- optionally slice it into user-specified units,
- partition blocks by predecessor-derived regions,
- model each region as a nested Python helper,
- move PHIs onto helper boundaries,
- materialize only cross-group state in outer scope,
- compress trivial SSA aliases,
- emit direct Python for a useful subset of MIR ops,
- return selected values as a dictionary,
- fall back to `mir_unlifted(...)` when necessary.

That is a practical CFG-to-Python reconstruction pass, not a full MIR interpreter and not a complete decompiler. Its main strengths are readability and preservation of enough data/control structure to inspect OpenVAF-generated MIR, especially through the `--backend mir-lift` path.

## 18. Proposed replacement: lift MIR to a small imperative LIR first

The current emitter mixes four separate jobs:

- MIR normalization and slicing,
- SSA/PHI lowering,
- control-flow reconstruction,
- Python source generation.

That is the main reason the implementation feels ad-hoc. A better split is:

```text
MIR FunctionUnit
  -> MIR-to-LIR lowering
  -> optional LIR cleanup / structuring passes
  -> LIR-to-Python emission
```

The important design constraint is that LIR should not be a Python-shaped IR. It should be a small imperative program representation that is easy to emit to Python today and easy to inspect, test, simplify, or emit to another target later.

### 18.1 LIR design goals

LIR should be:

- non-SSA: variables can be assigned more than once;
- explicit about function parameters, local variables, statements, and returns;
- expression-oriented only for side-effect-free computation;
- statement-oriented for assignments, calls with side effects, unsupported MIR, and control flow;
- independent of Python details such as nested helper functions, `nonlocal`, `_ret`, and Python identifier quirks;
- able to represent arbitrary MIR CFGs before any pretty structuring pass runs.

The key choice is to make LIR an imperative CFG with labels and gotos, not a nested-helper representation. That gives MIR-to-LIR a simple, total lowering path. Later passes can recover `if`, `while`, and straight-line blocks where the CFG is structured enough, but correctness should not depend on that reconstruction.

### 18.2 Core LIR model

A reasonable Rust shape is:

```rust
pub struct Program {
    pub functions: Vec<Function>,
}

pub struct Function {
    pub name: Symbol,
    pub params: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub entry: Label,
    pub blocks: Vec<Block>,
    pub returns: Vec<ReturnSlot>,
}

pub struct Local {
    pub id: LocalId,
    pub name_hint: String,
    pub ty: LirType,
}

pub enum LirType {
    Bool,
    Int,
    Real,
    Unknown,
}

pub struct Block {
    pub label: Label,
    pub stmts: Vec<Stmt>,
    pub term: Terminator,
}

pub enum Stmt {
    Declare(LocalId),
    Assign { dst: LocalId, value: Expr },
    Expr(Expr),
    Unsupported { dsts: Vec<LocalId>, text: String },
}

pub enum Terminator {
    Goto(Label),
    Branch { cond: Expr, then_label: Label, else_label: Label },
    Return(Vec<ReturnValue>),
    Unreachable,
}

pub enum Expr {
    Local(LocalId),
    Const(ConstValue),
    Unary { op: UnaryOp, arg: Box<Expr> },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Call { target: Symbol, args: Vec<Expr> },
    Unsupported { text: String, args: Vec<Expr> },
}
```

This is deliberately close to a simple C-like function in three-address form. The only non-C-looking parts are `Unsupported`, which preserves unlifted MIR without losing dataflow shape, and explicit labels/gotos, which are needed to cover arbitrary MIR before structural cleanup.

`Declare` can be kept as a statement if we want source-like output, or omitted from block bodies and emitted from `Function.locals` by the backend. I would keep declarations in `Function.locals` as the source of truth and only use `Stmt::Declare` if a later target benefits from declaration placement.

### 18.3 Values, locals, and names

MIR `Value`s should map to stable LIR `LocalId`s. The LIR local table owns display names separately from identity:

- MIR params become LIR params and locals.
- MIR instruction results become LIR locals.
- Constants become `Expr::Const` unless preserving them as locals is useful for debugging.
- Sanitized backend names are not stored as identity. They are emitted from `name_hint` plus a collision-avoidance table.

This avoids bugs where a Python name, MIR value string, and semantic identity are treated as the same thing.

Return values should also be represented explicitly:

```rust
pub struct ReturnSlot {
    pub key: String,
    pub value: LocalId,
}

pub enum ReturnValue {
    Named { key: String, value: Expr },
}
```

For the current Python behavior, the backend can still return a dictionary keyed by MIR value names. The policy deciding which roots are returned remains outside LIR construction: standalone text lifting can request all visible values, while the OpenVAF backend can request compiler-selected output roots.

### 18.4 Expressions and operations

LIR expressions should cover only operations we are prepared to define target-independently:

```rust
pub enum UnaryOp {
    Not,
    Neg,
    Cast(LirType),
    Math1(MathUnary),
}

pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LogicalAnd,
    LogicalOr,
    Math2(MathBinary),
}
```

`OptBarrier` should lower to a plain assignment or be removed by a copy-propagation cleanup pass. It should not leak into Python-specific alias state.

Math operations should stay semantic in LIR, for example `UnaryOp::Math1(MathUnary::Sin)`, and the Python backend can decide that this prints as `math.sin(...)`. That keeps target naming out of MIR-to-LIR.

Unsupported MIR should be explicit and dataflow-aware:

```rust
Stmt::Unsupported {
    dsts: vec![v42],
    text: "v42 = some_mir_op ...".to_owned(),
}
```

The Python backend can emit `v42 = mir_unlifted("...")`. Another backend could instead error, log, or interpret it.

### 18.5 PHI lowering

LIR should have no PHI nodes. MIR-to-LIR should eliminate PHIs on predecessor edges by inserting assignments before the edge transfer.

For a MIR block:

```text
block_join:
  v3 = phi [block_a: v1], [block_b: v2]
```

The LIR predecessor edges become:

```text
block_a:
  ...
  v3 = v1
  goto block_join

block_b:
  ...
  v3 = v2
  goto block_join
```

If a destination block has multiple PHIs, the edge assignments must be parallel, not sequential. The lowering should handle this by introducing edge temporaries when needed:

```text
tmp_phi0 = incoming_for_v3
tmp_phi1 = incoming_for_v4
v3 = tmp_phi0
v4 = tmp_phi1
goto block_join
```

The clean implementation is to create explicit edge blocks for CFG edges that need PHI assignments:

```text
pred -> edge_pred_to_join -> join
```

The edge block contains only PHI-copy assignments and a `Goto(join)`. This keeps ordinary blocks simple and avoids having to splice assignments into branch arms in awkward ways.

### 18.6 Control flow

The initial LIR should preserve MIR control flow directly:

- MIR `Jump` lowers to `Terminator::Goto`.
- MIR `Branch` lowers to `Terminator::Branch`.
- MIR `Exit` lowers to `Terminator::Return(...)`.
- Unknown terminators lower to `Stmt::Unsupported` plus `Terminator::Return(...)` or `Terminator::Unreachable`, depending on what is safer for the caller.

This means the first MIR-to-LIR implementation does not need predecessor-count grouping, nested helper functions, inlineable-group heuristics, `nonlocal`, or return-chained helper calls.

Later, an optional LIR structuring pass can convert easy CFG patterns into structured statements:

```rust
pub enum StructuredStmt {
    Assign { dst: LocalId, value: Expr },
    Expr(Expr),
    If { cond: Expr, then_body: Vec<StructuredStmt>, else_body: Vec<StructuredStmt> },
    While { cond: Expr, body: Vec<StructuredStmt> },
    Return(Vec<ReturnValue>),
}
```

But this should be an optimization for readability, not part of core semantic lowering. Keeping unstructured LIR valid is what prevents the backend from becoming another fragile CFG decompiler.

### 18.7 LIR-to-Python strategy

The first Python backend can be intentionally mechanical without synthesizing a block program
counter:

1. Emit imports and `mir_unlifted`.
2. Emit one Python function per LIR function.
3. Initialize non-parameter locals to `None`.
4. Count incoming transfers to each LIR label.
5. Inline a target block when the target has one incoming transfer.
6. Emit a nested helper function when the target has multiple incoming transfers, and call that
   helper at each transfer site.

```python
def lifted(a, b):
    v3 = None
    v3 = a + b
    return {"v3": v3}
```

This keeps the backend straightforward while avoiding a synthesized `_pc` variable and giant label
switch. A later structuring pass can still emit direct Python `while` for natural loops and
prettier diamonds, but the semantic fallback is explicit block helpers rather than a dispatcher.

The backend should be the only place that knows:

- how to sanitize Python identifiers,
- how to print Python operators,
- how to map semantic math ops to `math.*`,
- how to represent unsupported MIR as `mir_unlifted(...)`,
- whether returns are dictionaries, tuples, or another ABI.

### 18.8 Cleanup passes worth having

Once MIR-to-LIR is simple and total, cleanup should happen as small LIR-to-LIR passes:

- copy propagation for `OptBarrier` and PHI copies;
- dead assignment elimination for locals not needed by requested returns or side-effecting operations;
- constant folding for simple numeric and boolean expressions;
- block merging when a block has one predecessor and the predecessor ends in an unconditional goto;
- branch simplification when both targets are the same or condition is constant;
- optional structuring of diamonds and natural loops.

These passes are easier to test against LIR snapshots than against generated Python text.

### 18.9 Proposed module split

A concrete crate layout could be:

```text
src/
  lib.rs                 public API and orchestration
  lir.rs                 LIR data structures
  mir_to_lir.rs          FunctionUnit -> lir::Function
  lir_simplify.rs        cleanup passes
  lir_to_python.rs       Python backend
  normalize.rs           textual MIR normalization
  units.rs               FunctionUnit and metadata slicing
```

The public APIs can keep their current shape at first:

```rust
lift_text(...)
lift_function(...)
lift_function_with_returns(...)
```

Internally they would become:

```text
parse/normalize -> build FunctionUnit -> lower_lir -> simplify_lir -> emit_python
```

That allows this refactor to land without immediately changing callers in `openvaf`.

### 18.10 Minimal first milestone

The first useful milestone should be intentionally narrow:

1. Add `lir.rs` with the core unstructured LIR types.
2. Add `mir_to_lir.rs` that lowers params, locals, unary ops, binary ops, calls, jumps, branches, exits, PHIs via edge blocks, and unsupported instructions.
3. Add `lir_to_python.rs` that inlines single-use labels and emits helper functions for shared labels.
4. Keep the existing public API and return-selection behavior.
5. Add golden tests that compare LIR snapshots separately from Python output.

This replaces the most fragile part of the current design first: direct recursive CFG-to-Python emission. Pretty Python can come after the semantic path is clean.
