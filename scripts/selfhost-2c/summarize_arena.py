#!/usr/bin/env python3
import hashlib
import json
import random
import re
import statistics
import sys
from pathlib import Path


ROOT = Path(sys.argv[1]).resolve() if len(sys.argv) == 2 else None
if ROOT is None:
    raise SystemExit("usage: summarize_arena.py EXPERIMENT_ROOT")
RUNS = ROOT / "measurements" / "arena" / "runs.tsv"


def kv(path):
    result = {}
    for line in Path(path).read_text().splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            if re.fullmatch(r"-?\d+", value):
                result[key] = int(value)
    return result


def wall(path):
    line = next(line for line in Path(path).read_text().splitlines()
                 if "Elapsed (wall clock) time" in line)
    value = line.split(": ", 1)[1]
    parts = value.split(":")
    if len(parts) == 2:
        minutes, seconds = parts
        return round((int(minutes) * 60 + float(seconds)) * 1e9)
    hours, minutes, seconds = parts
    return round((int(hours) * 3600 + int(minutes) * 60 + float(seconds)) * 1e9)


def checksum(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def ci(values):
    rng = random.Random(20260908)
    estimates = []
    for _ in range(10000):
        estimates.append(statistics.median(values[rng.randrange(len(values))] for _ in values))
    estimates.sort()
    return [estimates[250], estimates[9750]]


with RUNS.open() as handle:
    header = handle.readline().rstrip().split("\t")
    rows = [dict(zip(header, line.rstrip().split("\t"))) for line in handle]
for row in rows:
    row["pair"] = int(row["pair"])
    row["status"] = int(row["status"])
    if row["status"] != 0:
        raise SystemExit(f"measurement failed: {row}")
    row["stats"] = kv(row["stderr"])
    row["wall_ns"] = wall(row["time"])

result = {"routes": {}}
for route in ("source", "cir"):
    route_rows = [row for row in rows if row["route"] == route and row["phase"] == "pair"]
    baseline = sorted((row for row in route_rows if row["variant"] == "baseline"), key=lambda row: row["pair"])
    candidate = sorted((row for row in route_rows if row["variant"] == "candidate"), key=lambda row: row["pair"])
    if len(baseline) != 10 or len(candidate) != 10:
        raise SystemExit(f"invalid pair count for {route}")
    if [row["pair"] for row in baseline] != list(range(1, 11)):
        raise SystemExit(f"missing baseline pair for {route}")
    if [row["pair"] for row in candidate] != list(range(1, 11)):
        raise SystemExit(f"missing candidate pair for {route}")
    output_hashes = {checksum(row["output"]) for row in route_rows}
    if len(output_hashes) != 1:
        raise SystemExit(f"output mismatch for {route}")
    values = {}
    for key, label in (
        ("wall_ns", "process wall"), ("process_total_ns", "process_total"),
        ("parse_ns", "parse"), ("execute_ns", "execute"),
        ("executed_instructions", "raw instructions"),
        ("executed_rle_instructions", "RLE instructions"),
        ("native_operations", "native operations"), ("rle_operations", "RLE operations"),
    ):
        left = [row[key] if key == "wall_ns" else row["stats"][key] for row in baseline]
        right = [row[key] if key == "wall_ns" else row["stats"][key] for row in candidate]
        delta = [b - a for a, b in zip(left, right)]
        scale = 1e-6 if key in ("wall_ns", "process_total_ns", "parse_ns", "execute_ns") else 1
        values[label] = {
            "baseline_median": statistics.median(left) * scale,
            "candidate_median": statistics.median(right) * scale,
            "change_percent": (statistics.median(right) / statistics.median(left) - 1) * 100,
            "paired_delta_median": statistics.median(delta) * scale,
            "paired_delta_bootstrap_95": [value * scale for value in ci(delta)],
        }
    result["routes"][route] = {
        "output_sha256": next(iter(output_hashes)),
        "measurements": values,
    }
(ROOT / "measurements" / "arena-summary.json").write_text(json.dumps(result, indent=2) + "\n")
