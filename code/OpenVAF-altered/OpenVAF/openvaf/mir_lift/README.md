# mir_lift

`mir_lift` lifts OpenVAF MIR into best-effort Python through a small imperative LIR.

The easiest path is the wrapper script:

```text
./lift.sh
./lift.sh diode
./lift.sh bsim4
./lift.sh path/to/model.va -o output.py
./lift.sh --dump-lir -o output.py
```

The default `./lift.sh` invocation lifts the DIODE integration example.

The generated Python emits direct control-flow transfers from LIR labels: labels with a single
incoming transfer are inlined at the use site, while labels with multiple incoming transfers are
emitted as small nested helper functions. MIR `phi` nodes are lowered to explicit edge-copy blocks
before Python emission.

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

Raw MIR text CLI:

```text
cargo run -p mir_lift -- input.mir -o output.py
cargo run -p mir_lift -- input.mir -o output.py --compilation-db metadata.json
cargo run -p mir_lift -- input.mir -o output.lir --dump-lir
```
