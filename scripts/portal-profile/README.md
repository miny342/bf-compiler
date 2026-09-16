# Compact portal diagnostics

Uses the production ABI and stage2 BF serializer. It does not run the compiler on its own source or change the ABI, optimizer, or RemoteTransfer implementation.

```sh
cargo build --release -p bf-compiler -p bf-interpreter
python3 scripts/portal-profile/run.py tmp/portal-run-01
```

The output directory must be new and under repository `tmp`. Each subprocess has a 60-second timeout; fixture BF must fit in 64 MiB. Default calibration targets two seconds for the slower RemoteTransfer setting, followed by one warmup and five measured ON/OFF pairs in alternating order. Input is capped at 16 MiB. `--seconds .2 --pairs 1` is useful for a smoke test, not for drawing performance conclusions.

For a small initial selection:

```sh
python3 scripts/portal-profile/run.py tmp/portal-run-02 \
  --cases optimizer transport-16 global-triple-full frame-triple-full
```

Byte transport defaults to unary. Add `--enable-nibble-transfer` to select the earlier nibble templates in both source and transport fixtures. This changes compiler output; RemoteTransfer ON/OFF changes only interpreter execution. Offset decomposition and base-16 window movement are unaffected.

`--baseline-compiler PATH` optionally checks that an earlier compiler produces byte-identical compressed BF for every source fixture. When comparing against the compiler before unary became the default, also pass `--enable-nibble-transfer`. It generates a separate old map and checks BF, not site IDs. The ordinary tests additionally check annotated/plain optimization equivalence, all 256 transported values, both RemoteTransfer settings, and native transfer attribution.

## Comparing unary and nibble generation

```sh
python3 scripts/portal-profile/compare-transfer.py tmp/transfer-comparison
```

This generates both variants, checks output against IR or controlled request bytes, and measures five unprofiled AB/BA pairs after warmup. RemoteTransfer ON and OFF are calibrated and compared independently, targeting one second for the slower variant. Input and repetition count are identical **within** each paired comparison; use time per record when comparing the two interpreter settings. `--cases`, `--pairs`, and `--seconds` narrow the experiment. Optional `--baseline-compiler PATH` checks that **nibble** output matches the earlier compiler. Use `run.py` separately for sample/counters diagnosis of a selected generation mode.

Exact sources, inputs, outputs, BF/maps, run logs, script snapshots and binary hashes remain in the output directory. Summaries record execute/parse times, logical counters, RSS, all pairs and a paired bootstrap interval for the execute-time difference. These compact cases do not establish a full selfhost speedup.

## Cases

- `optimizer`: historical case name, now reads the production `09_bf_serialization.bfc` and arithmetic helpers. Exercises immediate output with byte and wide runs; no ring buffer or peephole optimization remains. Input is retained for convenience, but output and work differ from measurements before the full16 rollback.
- `scalar`: copies a global byte to a local snapshot, overwrites the global, then observes both values. Covers all 256 byte values and the separate global-to-frame copy template.
- `global-byte-zero/full`, `global-triple-zero/full`: dynamically store and load 16-element arrays; one-cell and three-cell values, with zero/nonzero payload. Three-cell indices include physical chunk crossings. Constant-index setup and final observation do not issue dynamic portal requests.
- `frame-triple-full`, `global-triple-deep/wide`, `frame-triple-deep`: change region, eight extra recursive activations, or persistent frame padding. Caller sentinel values are read after returning to verify preservation.
- `global-large`: a 32×256 array, with offsets 0, 15, 16, 255, 256, 4095, 4096, 8191. Seeds the payload chunks touched by the production base-16 jump paths. Final observation verifies that exchanged payload is restored, including cells outside the selected element.
- `transport-16/256`: exports a small fixture through an explicitly selected ignored Rust test. It calls the same `move_global_portal_request` as production, in an initialized ABI frame. Seven controlled request bytes cover all 256 values. No dispatcher interprets these artificial bytes as PCs. Results are observed and the global prefix is cleared after every request. The suffix is frame **padding cells**, not stack depth; actual stack chunks are in `*.fixture.json`.
- `transport-65280`: extends the live stack flags with 4,064 caller chunks beyond a valid padded frame, moving the active context to their end. This exercises a long navigation/probe path without declaring an oversized function or compiling the compiler. Eight records cover zero, byte/nibble boundaries and large values; fields are offset by 31 modulo 256. The suffix denotes total padding across the synthetic live region; actual chunks and generation mode are recorded in the fixture metadata.
- `transport-v0/v1/v15/v16/v127/v255`: the same small transport BF with constant request bytes, separating value-dependent preparation from stack traversal. These are controlled primitive measurements, not valid execution PCs.

The Rust fixture exporter is test-only and adds no public API or production benchmark switch. It emits compressed BF/maps using the ordinary optimizer. Building it needs the workspace dev dependencies, just like `cargo test`.

## Reading results

`report.md` gives unprofiled median execute times and exclusive sample groups. `summary.json` and per-case summaries retain every measured run, grouped/individual counters, profile overhead, static request metadata (including encoded PC values), BF/map identities and sizes. Raw sample/counters reports, IR logs, exact input/expected output and build logs are retained. `manifest.json` identifies the compiler, interpreter, fixture implementation and production source files; `runner.py` preserves the script used for that run.

- Performance values are from **unprofiled** runs. Sample and counters are separate diagnostic executions. Their times are available but do not establish the ON/OFF speedup.
- Array cases check IR load/store totals against the workload. BF primitive requests are `records × 2 × value width`. The transport fixture executes one seven-byte request per record. The optimizer case has no claimed request denominator.
- Per-request totals include initialization, input/output, fixture observation and cleanup. Use the stage counters for the cost within a specific template; do not read the whole-case average as isolated router latency.
- Exclusive groups partition sample counts. A fused native instruction belongs to the common ancestor of its contributing sites; parent residue is reported, not redistributed. Fewer than 20 samples is marked low confidence. Sample-mode operation counters are unmeasured, not zero-cost operations.
- `remote_transfer_loops` records recognized executions (including zero-source skips), `remote_transfer_fallbacks` records failed runtime checks. Raw/RLE instructions and scan counters describe BF semantics; they are not a measurement of the implementation's probe work. No interpreter instrumentation was added to infer the latter.
- ON/OFF must have identical output, logical BF/RLE counts and maximum pointer. Source fixtures are also checked against IR output. Every initialized payload cell and caller sentinel is observed at the end.
- Small fixtures change dispatch size and encoded PC values. The transport fixture controls request bytes independently to expose this difference. Production request attributes record **static origins**, not dynamic caller attribution of a shared router.

These cases reproduce the production templates and buffer behavior. They do not establish the phase mix, percentage breakdown, or end-to-end speedup of `full14`. Keep interpreter settings fixed when comparing later compiler changes; compare changes independently.
