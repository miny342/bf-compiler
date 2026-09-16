# Subtraction/borrow fusion

The compiler tracks local value identities through copies and fuses a comparison
followed by subtraction of the same snapshots. The shared Frame pass runs on
source lowering before allocation and on imported binary CIR. No BFC intrinsic,
type, or serialized CIR opcode is added.

The BF template retains the comparison countdown remainder instead of discarding
it. Inputs are consumed; the wrapping difference is written before the selected
borrow value. Existing ABI scratch is zero on exit. The original standalone
comparison template is unchanged.

## Reproduce

Save a baseline `bfc` before building the candidate. Use the same interpreter for
both versions. With a saved compressed stage2 source:

```sh
python3 scripts/sub-borrow-fusion/run.py \
  --baseline tmp/sub-borrow-fusion/baseline-compiler \
  --candidate tmp/sub-borrow-fusion/build/release/bfc \
  --interpreter target/release/bf-interpreter \
  --source logs/full-selfhost-20260916-132209/stage2-compiler.bfc \
  --output tmp/sub-borrow-fusion/rerun --pairs 5
```

The output directory must not exist. The runner retains BF artifacts, their
hashes/sizes, exact expected output, counters, process times and execute times.
It alternates AB/BA order and checks output equality on every run. Pair output
and the production serializer harness also have independently calculated expected
values. `--stats` is enabled for both variants; times exclude profiling samples.

## Measurement (2026-09-16)

Baseline: `b9aad1e`. Results: `tmp/sub-borrow-fusion/bench-direct/results.json`.
Five pairs, same interpreter, execution-only medians (milliseconds):

| Case | Baseline | Fusion | Interpretation |
|---|---:|---:|---|
| All 65,536 byte pairs, difference + borrow + original inputs | 221.450 | 215.821 | 2.5% reduction |
| Production `emit_repeat_wide`, 255 large 24-bit counts | 101.481 | 100.950 | 0.5%; small difference |
| Compiler on hello | 50.348 | 49.362 | Identical operation counters; no demonstrated benefit |
| Compiler on aggregates | 596.114 | 597.296 | Essentially unchanged |

Pair raw-BF instruction count: 4,612,227,432 → 3,283,384,296 (-28.8%).
RLE instruction count: 698,795,996 → 514,049,500 (-26.4%).
Native operations: 73,398,857 → 72,415,561 (-1.3%).
The interpreter already accelerates transfers, so fewer BF instructions do not
translate proportionally into wall time.

The production serializer's raw count drops 10.6%, native operations 0.7%.
Compressed compiler BF shrinks 6,177,540 → 6,177,420 bytes. Maximum pointers are
unchanged in all four cases. `wide_subtract` receives two fused operations (low
and middle bytes); its allocated scalar slots increase from 5 to 6 without
increasing its frame chunk count in this source. High-byte checks span control
boundaries and remain comparisons. `arena_advance` and dispatch are unaffected.

The earlier `bench-initial` run overlapped test execution and used an extra
saved-difference copy; its wall times are not used to judge the final version.
Full selfhost completion time has not been measured. This is a small, bounded
optimization and a basis for further fusion, not a solution to linear comparison
or dispatcher cost.

Validation includes exhaustive source/CIR and template byte pairs, aliases,
inverted/custom borrow results, changed inputs, randomized intervening operations,
production 24-bit borrow propagation, and the workspace/selfhost checks.
IR artifact identity is v5; old phase configurations and BF/profile maps must be
regenerated. Binary CIR encoding is unchanged.
