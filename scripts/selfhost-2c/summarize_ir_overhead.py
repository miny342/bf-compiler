#!/usr/bin/env python3
import hashlib
import json
import random
import statistics
import sys
from pathlib import Path


ROOT = Path(sys.argv[1]).resolve() if len(sys.argv) == 2 else None
if ROOT is None:
    raise SystemExit("usage: summarize_ir_overhead.py EXPERIMENT_ROOT")
RUNS = ROOT / "ir-overhead" / "runs.tsv"


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def wall_ns(path):
    line = next(line for line in Path(path).read_text().splitlines()
                 if "Elapsed (wall clock) time" in line)
    value = line.split(": ", 1)[1]
    parts = value.split(":")
    if len(parts) == 2:
        minutes, seconds = parts
        return round((int(minutes) * 60 + float(seconds)) * 1e9)
    hours, minutes, seconds = parts
    return round((int(hours) * 3600 + int(minutes) * 60 + float(seconds)) * 1e9)


def kv(path):
    line = next(line for line in Path(path).read_text().splitlines()
                 if "phase=execute status=finished" in line)
    return {
        key: int(value)
        for key, value in (field.split("=", 1) for field in line.split() if "=" in field)
        if value.isdigit()
    }


def bootstrap(values, seed=20260909, samples=10000):
    rng = random.Random(seed)
    estimates = []
    for _ in range(samples):
        estimates.append(statistics.median(values[rng.randrange(len(values))] for _ in values))
    estimates.sort()
    return [estimates[int(samples * 0.025)], estimates[int(samples * 0.975)]]


with RUNS.open() as handle:
    header = handle.readline().rstrip("\n").split("\t")
    rows = [dict(zip(header, line.rstrip("\n").split("\t"))) for line in handle]
if len(rows) != 20:
    raise SystemExit(f"expected 20 overhead runs, got {len(rows)}")
for row in rows:
    row["pair"] = int(row["pair"])
    row["status"] = int(row["status"])
    row["execute_ns"] = int(row["execute_ns"])
    if row["status"] != 0 or row["execute_ns"] <= 0:
        raise SystemExit(f"invalid overhead run: {row}")
    row["process_wall_ns"] = wall_ns(row["time"])
    row["counters"] = kv(row["log"])

by_variant = {
    variant: sorted((row for row in rows if row["variant"] == variant), key=lambda row: row["pair"])
    for variant in ("off", "on")
}
if any(len(value) != 10 for value in by_variant.values()):
    raise SystemExit(f"expected 10 runs per variant: {by_variant}")
if any([row["pair"] for row in value] != list(range(1, 11)) for value in by_variant.values()):
    raise SystemExit("missing overhead pair")
output_hashes = {sha256(row["output"]) for row in rows}
if len(output_hashes) != 1:
    raise SystemExit(f"output mismatch: {output_hashes}")

counter_keys = (
    "continuations", "frame_instructions", "loop_iterations", "calls", "returns",
    "array_loads", "array_stores", "aggregate_loads", "aggregate_stores",
    "input_operations", "output_bytes",
)
counter_values = {
    key: {variant: [row["counters"][key] for row in by_variant[variant]] for variant in ("off", "on")}
    for key in counter_keys
}
if any(len(set(values[variant])) != 1 for values in counter_values.values() for variant in ("off", "on")):
    raise SystemExit("normal counters varied within a variant")
if any(values["off"] != values["on"] for values in counter_values.values()):
    raise SystemExit("normal counters differ between transition collection modes")

measurements = {}
for key in ("execute_ns", "process_wall_ns"):
    off = [row[key] for row in by_variant["off"]]
    on = [row[key] for row in by_variant["on"]]
    delta = [right - left for left, right in zip(off, on)]
    measurements[key] = {
        "off_median_ns": statistics.median(off),
        "on_median_ns": statistics.median(on),
        "on_vs_off_percent": (statistics.median(on) / statistics.median(off) - 1) * 100,
        "paired_delta_median_ns": statistics.median(delta),
        "paired_delta_bootstrap_95_ns": bootstrap(delta),
        "off_values_ns": off,
        "on_values_ns": on,
    }

result = {
    "benchmark": {
        "program": "scripts/selfhost-2c/fixtures/ir-overhead.bfc",
        "input_byte": 32,
        "repetitions": "32 * 255 * 255 inner loop iterations",
        "measurement": "bfc-ir execute elapsed_ns, excluding lowering and process startup",
        "pairs": 10,
        "order": "AB/BA alternating, A=on, B=off",
        "bootstrap_seed": 20260909,
    },
    "output_sha256": next(iter(output_hashes)),
    "counters": counter_values,
    "measurements": measurements,
}
(ROOT / "ir-overhead" / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
