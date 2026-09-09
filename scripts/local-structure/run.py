#!/usr/bin/env python3
"""Compare one fixed source/CIR pair; all artifacts live in a new run root."""
import argparse
import hashlib
import json
from pathlib import Path
import random
import re
import statistics
import subprocess
import time


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    with path.open("x") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


def ci(values):
    rng = random.Random(20260909)
    medians = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(10000))
    return [medians[250], medians[9750]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--interpreter", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--cir", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, nargs="+", required=True)
    parser.add_argument("--pairs", type=int, default=10)
    args = parser.parse_args()
    if args.pairs < 2 or len({p.name for p in args.inputs}) != len(args.inputs):
        parser.error("need at least two pairs and distinct input filenames")
    root = args.root.resolve()
    # Root must be new, including on failed reruns. Never overwrite evidence.
    root.mkdir(parents=True, exist_ok=False)
    driver, interpreter = args.driver.resolve(), args.interpreter.resolve()
    programs = {"source": args.source.resolve(), "cir": args.cir.resolve()}
    inputs = [p.resolve() for p in args.inputs]
    manifest = {
        "start_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "diff": subprocess.check_output(["git", "diff"], text=True),
        "pairs": args.pairs, "clock": "perf_counter_ns process wall; interpreter execute_ns separately",
        "options": "2c enabled in both; candidate adds post-allocation local structure; IR metrics off",
        "files": {str(p): {"sha256": sha(p), "bytes": p.stat().st_size}
                  for p in [driver, interpreter, *programs.values(), *inputs, Path(__file__)]},
    }
    save(root / "manifest.json", manifest)

    def invoke(command, stdin, stdout, stderr, time_path=None):
        if time_path:
            command = ["/usr/bin/time", "-v", "-o", str(time_path), *command]
        with stdin.open("rb") as src, stdout.open("xb") as out, stderr.open("xb") as err:
            start = time.perf_counter_ns()
            subprocess.run(list(map(str, command)), stdin=src, stdout=out, stderr=err, check=True)
            return time.perf_counter_ns() - start

    expected = {}
    ir_rows = []
    normal_counters = {}
    for route, program in programs.items():
        for input_path in inputs:
            for variant in ["baseline", "candidate"]:
                stem = root / f"ir-{route}-{input_path.stem}-{variant}"
                output = stem.with_suffix(".out")
                invoke([driver, route, variant, "ir", program, output], input_path,
                       stem.with_suffix(".stdout"), stem.with_suffix(".log"))
                counters = dict(re.findall(r"(\w+)=(\w+)", stem.with_suffix(".log").read_text().splitlines()[-1]))
                invariant = {k: counters[k] for k in ["calls", "returns", "array_loads", "array_stores",
                             "aggregate_loads", "aggregate_stores", "input", "output", "aborted"]}
                key = (route, input_path.name)
                if key in normal_counters and normal_counters[key] != invariant:
                    raise RuntimeError(f"IR counter mismatch: {stem}")
                normal_counters[key] = invariant
                ir_rows.append(dict(route=route, input=input_path.name, variant=variant, counters=counters))
                digest = sha(output)
                name = input_path.name
                if name in expected and expected[name] != digest:
                    raise RuntimeError(f"IR output mismatch: {stem}")
                expected[name] = digest
                print(f"IR verified {stem.name}", flush=True)
    save(root / "ir.json", ir_rows)

    artifacts = {}
    for route, program in programs.items():
        for variant in ["baseline", "candidate"]:
            bf = root / f"{route}-{variant}.bf"
            invoke([driver, route, variant, "bf", program, bf], inputs[0],
                   root / f"{route}-{variant}.build.stdout",
                   root / f"{route}-{variant}.build.log", root / f"{route}-{variant}.build.time")
            artifacts[f"{route}-{variant}"] = {
                "bf_sha256": sha(bf), "bf_bytes": bf.stat().st_size,
                "map_sha256": sha(Path(str(bf) + "map.json")),
                "map_bytes": Path(str(bf) + "map.json").stat().st_size,
            }
            print(f"BF generated {route}/{variant}", flush=True)
    save(root / "artifacts.json", artifacts)

    rows = []
    with (root / "runs.jsonl").open("x") as journal:
        for route in programs:
            for input_path in inputs:
                for pair in range(args.pairs + 1):
                    order = ["baseline", "candidate"] if pair % 2 else ["candidate", "baseline"]
                    for variant in order:
                        stem = root / f"run-{route}-{input_path.stem}-{pair}-{variant}"
                        output, log = stem.with_suffix(".out"), stem.with_suffix(".log")
                        wall = invoke([interpreter, "--unlimited-tape", "--no-progress", "--stats",
                                       "--timings", root / f"{route}-{variant}.bf"],
                                      input_path, output, log, stem.with_suffix(".time"))
                        if sha(output) != expected[input_path.name]:
                            raise RuntimeError(f"BF output mismatch: {stem}")
                        stats = {k: int(v) for k, v in re.findall(r"^(\w+)=(\d+)$", log.read_text(), re.M)}
                        stats["wall_ns"] = wall
                        rss = re.search(r"Maximum resident set size \(kbytes\): (\d+)", stem.with_suffix(".time").read_text())
                        stats["rss_kb"] = int(rss[1])
                        row = dict(route=route, input=input_path.name, pair=pair, variant=variant, stats=stats)
                        journal.write(json.dumps(row) + "\n")
                        journal.flush()
                        rows.append(row)
                        print(f"{stem.name} execute_ns={stats['execute_ns']}", flush=True)
    summary = {"outputs": expected, "comparisons": {}}
    for route in programs:
        for input_path in inputs:
            variants = {v: [r["stats"] for r in rows if r["route"] == route
                            and r["input"] == input_path.name and r["pair"] > 0 and r["variant"] == v]
                        for v in ["baseline", "candidate"]}
            values = {}
            for key in ["wall_ns", "parse_ns", "execute_ns", "executed_instructions",
                        "executed_rle_instructions", "native_operations", "max_pointer", "rss_kb"]:
                a, b = ([s[key] for s in variants[v]] for v in ["baseline", "candidate"])
                ma, mb = statistics.median(a), statistics.median(b)
                values[key] = dict(baseline=ma, candidate=mb, change_percent=(mb / ma - 1) * 100,
                                   paired_difference_ci95=ci([y - x for x, y in zip(a, b)]))
            summary["comparisons"][f"{route}/{input_path.name}"] = values
    save(root / "summary.json", summary)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
