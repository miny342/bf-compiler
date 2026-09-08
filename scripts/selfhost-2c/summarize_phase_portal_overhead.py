#!/usr/bin/env python3
"""Summarize paired high-resolution phase/portal instrumentation overhead."""

import csv
import hashlib
import json
import random
import statistics
import sys
from pathlib import Path


if len(sys.argv) != 2:
    raise SystemExit("usage: summarize_phase_portal_overhead.py RUN_ROOT")
root = Path(sys.argv[1]).resolve()
out_root = root / "phase-portal-overhead"
output = root / "measurements" / "phase-portal-overhead-summary.json"
if output.exists():
    raise SystemExit(f"refusing to overwrite existing summary: {output}")

rows = list(csv.DictReader((out_root / "runs.tsv").open(), delimiter="\t"))
rows = [row for row in rows if row["pair"] != "warmup"]
by_pair = {}
for row in rows:
    if row["status"] != "0":
        raise SystemExit(f"failed run: {row}")
    by_pair.setdefault(row["pair"], {})[row["variant"]] = row
if len(by_pair) != 10 or any(set(pair) != {"phase_off", "phase_on"} for pair in by_pair.values()):
    raise SystemExit("expected ten complete phase_off/phase_on pairs")

normal_fields = (
    "executed_continuations",
    "executed_frame_instructions",
    "loop_iterations",
    "calls",
    "returns",
    "array_loads",
    "array_stores",
    "aggregate_loads",
    "aggregate_stores",
    "input_operations",
    "output_bytes",
)
output_hashes = set()
diffs = []
off_values = []
on_values = []
for pair in sorted(by_pair, key=int):
    off = by_pair[pair]["phase_off"]
    on = by_pair[pair]["phase_on"]
    off_report = json.loads(Path(off["metrics"]).read_text())
    on_report = json.loads(Path(on["metrics"]).read_text())
    if off_report["format"] != "bfc-continuation-ir-metrics-v1":
        raise SystemExit("phase_off must retain metrics v1")
    if on_report["format"] != "bfc-continuation-ir-metrics-v2":
        raise SystemExit("phase_on must emit metrics v2")
    if any(off_report["run"][field] != on_report["run"][field] for field in normal_fields):
        raise SystemExit(f"normal counters differ for pair {pair}")
    if on_report["phase_metrics"]["portal"]["total_requests"] != sum(
        on_report["run"][field] for field in ("array_loads", "array_stores", "aggregate_loads", "aggregate_stores")
    ):
        raise SystemExit(f"portal accounting differs for pair {pair}")
    off_values.append(int(off["execute_ns"]))
    on_values.append(int(on["execute_ns"]))
    diffs.append(on_values[-1] - off_values[-1])
    output_hashes.add(hashlib.sha256(Path(on["output"]).read_bytes()).hexdigest())


def percentile(values, fraction):
    values = sorted(values)
    index = fraction * (len(values) - 1)
    lower = int(index)
    upper = min(lower + 1, len(values) - 1)
    weight = index - lower
    return values[lower] * (1 - weight) + values[upper] * weight


rng = random.Random(20260909)
bootstrap = []
for _ in range(10_000):
    sample = [rng.choice(diffs) for _ in diffs]
    bootstrap.append(statistics.median(sample))
median_off = statistics.median(off_values)
median_on = statistics.median(on_values)
summary = {
    "format": "bfc-continuation-ir-phase-portal-overhead-v1",
    "measurement": {
        "pairs": 10,
        "order": "odd pairs on then off; even pairs off then on",
        "warmup": 1,
        "existing_transition_collection": True,
        "new_phase_portal_collection": "phase_on only",
        "timer": "bfc-ir phase=execute elapsed_ns",
        "bootstrap_seed": 20260909,
        "bootstrap_resamples": 10_000,
    },
    "execute_ns": {
        "phase_off_median": median_off,
        "phase_on_median": median_on,
        "median_difference": median_on - median_off,
        "median_difference_percent": (median_on / median_off - 1) * 100,
        "median_difference_bootstrap_95_percentile": [
            percentile(bootstrap, 0.025),
            percentile(bootstrap, 0.975),
        ],
        "phase_off": off_values,
        "phase_on": on_values,
        "paired_difference": diffs,
    },
    "output_sha256": sorted(output_hashes),
}
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps(summary, indent=2) + "\n")
