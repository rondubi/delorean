# mir_lift

`mir_lift` lifts OpenVAF MIR text into best-effort Python.

It prefers direct Python expressions for simple arithmetic and comparisons. For non-trivial control
flow it groups MIR basic blocks by predecessor-derived source sets, emits numbered Python helper
functions for those groups, and preserves SSA `phi` semantics via predecessor-based assignments.

Optional compilation metadata can be provided as JSON:

```json
{
  "functions": [
    {
      "name": "helper",
      "mir_name": "",
      "blocks": ["block2", "block3", "block4"]
    }
  ]
}
```

If `blocks` is omitted or empty, the entire MIR function is lifted as one Python function. If
present, the listed blocks are lifted as a separate Python function and external SSA inputs are
turned into Python parameters automatically.

CLI:

```text
cargo run -p mir_lift -- input.mir -o output.py
cargo run -p mir_lift -- input.mir -o output.py --compilation-db metadata.json
```
