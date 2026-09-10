#!/usr/bin/env python3
"""Compare one BF artifact with RemoteTransfer disabled/enabled, without profiling.

Use a new output directory under ./tmp. Timings alternate AB/BA; output hashes,
logical BF/RLE counts, and maximum pointers must match in every execution.
"""
import argparse
import hashlib
import json
from pathlib import Path
import random
import statistics
import subprocess


def identity(path):
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def paired_intervals(rows):
    """Paired median differences, bootstrap 95% intervals (10,000 resamples)."""
    pairs = sorted({row["pair"] for row in rows if row["pair"] >= 0})
    by_pair = {(row["pair"], row["enabled"]): row for row in rows}
    result = {}
    for key in ["execute_ns", "parse_ns", "process_total_ns"]:
        differences = [by_pair[pair, True][key] - by_pair[pair, False][key] for pair in pairs]
        rng = random.Random(20260910)
        samples = sorted(statistics.median(rng.choices(differences, k=len(pairs))) for _ in range(10000))
        result[key] = {"median": statistics.median(differences), "ci95": [samples[250], samples[9750]]}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--interpreter", type=Path, required=True)
    parser.add_argument("--program", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, nargs="+", required=True)
    parser.add_argument("--pairs", type=int, default=10)
    args = parser.parse_args()
    if args.pairs < 1:
        parser.error("--pairs must be positive")
    args.output.mkdir(parents=True, exist_ok=False)
    command = [str(args.interpreter.resolve()), "--unlimited-tape", "--no-progress", "--stats", "--timings"]
    manifest = {"command": command, "pairs": args.pairs,
                "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                "diff_sha256": hashlib.sha256(subprocess.check_output(["git", "diff"])).hexdigest(),
                "files": [identity(path.resolve()) for path in [args.interpreter, args.program, *args.inputs, Path(__file__)]]}
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    summary = []
    with (args.output / "runs.jsonl").open("x") as journal:
        for path in args.inputs:
            data = path.read_bytes()
            invariant = None
            rows = []
            for pair in range(-1, args.pairs):
                for enabled in ([False, True] if pair % 2 == 0 else [True, False]):
                    result = subprocess.run(command + ([] if enabled else ["--disable-remote-transfer"]) + [str(args.program.resolve())], input=data, capture_output=True, check=True)
                    stats = {}
                    for line in result.stderr.decode().splitlines():
                        key, value = line.split("=", 1)
                        stats[key] = int(value)
                    checksum = hashlib.sha256(result.stdout).hexdigest()
                    actual = [checksum, *[stats[key] for key in ["executed_instructions", "executed_rle_instructions", "max_pointer"]]]
                    if invariant is None:
                        invariant = actual
                    if invariant != actual:
                        raise RuntimeError(f"semantic/count mismatch for {path}")
                    row = {"input": str(path), "pair": pair, "enabled": enabled, "output_sha256": checksum, **stats}
                    journal.write(json.dumps(row) + "\n")
                    journal.flush()
                    if pair >= 0:
                        rows.append(row)
            entry = {"input": str(path), "output_sha256": invariant[0]}
            entry["paired_differences"] = paired_intervals(rows)
            for enabled, name in [(False, "baseline"), (True, "candidate")]:
                entry[name] = {key: statistics.median(row[key] for row in rows if row["enabled"] == enabled) for key in stats}
            for key in ["execute_ns", "parse_ns", "process_total_ns"]:
                entry[key + "_change_percent"] = 100 * (entry["candidate"][key] / entry["baseline"][key] - 1)
            summary.append(entry)
            (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
            print(json.dumps(entry), flush=True)


if __name__ == "__main__":
    main()
