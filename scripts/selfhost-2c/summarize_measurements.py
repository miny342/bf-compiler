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
    raise SystemExit("usage: summarize_measurements.py EXPERIMENT_ROOT")
RUNS = ROOT / "measurements" / "runs" / "runs.tsv"
ARTIFACTS = ROOT / "artifacts"


def parse_kv(path):
    values = {}
    for line in Path(path).read_text().splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            if re.fullmatch(r"-?\d+", value):
                values[key] = int(value)
    return values


def elapsed_ns(path):
    for line in Path(path).read_text().splitlines():
        if "Elapsed (wall clock) time" in line:
            value = line.split(": ", 1)[1]
            parts = value.split(":")
            if len(parts) == 3:
                hours, minutes, seconds = parts
            else:
                hours, minutes, seconds = 0, parts[0], parts[1]
            return round((int(hours) * 3600 + int(minutes) * 60 + float(seconds)) * 1e9)
    raise ValueError(f"missing elapsed time in {path}")


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def bootstrap(values, seed=20260908, samples=10000):
    rng = random.Random(seed)
    estimates = []
    for _ in range(samples):
        estimates.append(statistics.median(values[rng.randrange(len(values))] for _ in values))
    estimates.sort()
    return [estimates[int(samples * 0.025)], estimates[int(samples * 0.975)]]


with RUNS.open() as handle:
    header = handle.readline().rstrip("\n").split("\t")
    rows = [dict(zip(header, line.rstrip("\n").split("\t"))) for line in handle]
if not rows:
    raise SystemExit("no measurement rows")
for row in rows:
    row["pair"] = int(row["pair"])
    row["status"] = int(row["status"])
    if row["status"] != 0:
        raise SystemExit(f"measurement failed: {row}")
    row["stats"] = parse_kv(Path(row["stderr"]))
    row["external_wall_ns"] = elapsed_ns(Path(row["stderr"]).with_suffix(".time"))
    row["output_sha256"] = sha256(row["output"])

metrics = [
    ("external_wall_ns", "process wall", 1e-6),
    ("process_total_ns", "process_total", 1e-6),
    ("parse_ns", "parse", 1e-6),
    ("fast_ir_build_ns", "fast_ir_build", 1e-6),
    ("execute_ns", "execute", 1e-6),
    ("output_write_ns", "output_write", 1e-6),
    ("executed_instructions", "raw instructions", 1),
    ("executed_rle_instructions", "RLE instructions", 1),
    ("native_operations", "native operations", 1),
    ("rle_operations", "RLE operations", 1),
    ("max_pointer", "max pointer", 1),
]

summary = {"bootstrap_seed": 20260908, "sets": []}
for family in ("source", "cir"):
    for input_name in ("hello", "stage5_functions", "stage8_aggregates"):
        selected = [
            row for row in rows
            if row["family"] == family and row["input"] == input_name and row["phase"] == "pair"
        ]
        by_variant = {
            variant: sorted(
                (row for row in selected if row["variant"] == variant),
                key=lambda row: row["pair"],
            )
            for variant in ("baseline", "candidate")
        }
        if any(len(value) != 10 for value in by_variant.values()):
            raise SystemExit(f"expected 10 pair values for {family}/{input_name}: {by_variant}")
        if [row["pair"] for row in by_variant["baseline"]] != list(range(1, 11)):
            raise SystemExit(f"missing baseline pair for {family}/{input_name}")
        if [row["pair"] for row in by_variant["candidate"]] != list(range(1, 11)):
            raise SystemExit(f"missing candidate pair for {family}/{input_name}")
        output_hashes = {row["output_sha256"] for row in selected}
        if len(output_hashes) != 1:
            raise SystemExit(f"output mismatch for {family}/{input_name}: {output_hashes}")
        measurements = {}
        for key, label, scale in metrics:
            baseline = [row.get(key, row["stats"].get(key)) for row in by_variant["baseline"]]
            candidate = [row.get(key, row["stats"].get(key)) for row in by_variant["candidate"]]
            if any(value is None for value in baseline + candidate):
                raise SystemExit(f"missing {key} for {family}/{input_name}")
            deltas = [c - b for b, c in zip(baseline, candidate)]
            baseline_median = statistics.median(baseline)
            candidate_median = statistics.median(candidate)
            measurements[label] = {
                "baseline_median": baseline_median * scale,
                "candidate_median": candidate_median * scale,
                "change_percent": (candidate_median / baseline_median - 1) * 100
                if baseline_median else None,
                "paired_delta_median": statistics.median(deltas) * scale,
                "paired_delta_bootstrap_95": [value * scale for value in bootstrap(deltas)],
                "baseline_values": baseline,
                "candidate_values": candidate,
            }
        summary["sets"].append({
            "family": family,
            "input": input_name,
            "pairs": len(by_variant["baseline"]),
            "output_sha256": next(iter(output_hashes)),
            "output_bytes": Path(by_variant["baseline"][0]["output"]).stat().st_size,
            "measurements": measurements,
        })

artifact_info = {}
for family in ("source", "cir"):
    artifact_info[family] = {}
    for variant in ("baseline", "candidate"):
        artifact = ARTIFACTS / family / f"{variant}.bf"
        profile_map = ARTIFACTS / family / f"{variant}.bfmap.json"
        artifact_info[family][variant] = {
            "bf_bytes": artifact.stat().st_size,
            "map_bytes": profile_map.stat().st_size,
            "bf_sha256": sha256(artifact),
            "map_sha256": sha256(profile_map),
        }
summary["artifacts"] = artifact_info
(ROOT / "measurements" / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

with (ROOT / "measurements" / "summary.md").open("w") as output:
    output.write("# Measurement summary\n\n")
    output.write("All values are medians of 10 paired measurements; percent is candidate vs baseline.\n\n")
    output.write("| route | input | metric | baseline | candidate | change | paired delta 95% bootstrap |\n")
    output.write("|---|---|---|---:|---:|---:|---:|\n")
    for item in summary["sets"]:
        for label in (
            "process wall", "process_total", "parse", "execute", "raw instructions",
            "RLE instructions", "native operations", "RLE operations", "max pointer",
        ):
            value = item["measurements"][label]
            interval = value["paired_delta_bootstrap_95"]
            output.write(
                f"| {item['family']} | {item['input']} | {label} | "
                f"{value['baseline_median']:.3f} | {value['candidate_median']:.3f} | "
                f"{value['change_percent']:.4f}% | "
                f"[{interval[0]:.3f}, {interval[1]:.3f}] |\n"
            )
