#!/usr/bin/env python3
"""Compare two compiler-generated BF files, using one unchanged interpreter.

Each file may be plain or BFCRLE. Writes only small measurements, never the
program's potentially huge output. Pass --inputs for compiler-BF experiments;
omit it for the repeat.bfc output-loop benchmark.
"""
import argparse
import hashlib
import json
from pathlib import Path
import runpy
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--interpreter", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, nargs="*")
    parser.add_argument("--pairs", type=int, default=10)
    args = parser.parse_args()
    if args.pairs < 1:
        parser.error("--pairs must be positive")
    helpers = runpy.run_path(str(Path(__file__).resolve().parents[1] / "remote-transfer/run.py"))
    args.output.mkdir(parents=True, exist_ok=False)
    command = [str(args.interpreter.resolve()), "--unlimited-tape", "--no-progress", "--stats", "--timings"]
    files = [args.interpreter, args.baseline, args.candidate, *(args.inputs or []), Path(__file__)]
    (args.output / "manifest.json").write_text(json.dumps({
        "command": command, "pairs": args.pairs,
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "files": [helpers["identity"](p.resolve()) for p in files],
    }, indent=2) + "\n")
    summary = []
    with (args.output / "runs.jsonl").open("x") as journal:
        for path in args.inputs or [None]:
            data = path.read_bytes() if path else b""
            rows = []
            expected = None
            for pair in range(-1, args.pairs):
                for enabled in ([False, True] if pair % 2 == 0 else [True, False]):
                    program = args.candidate if enabled else args.baseline
                    result = subprocess.run(command + [str(program.resolve())], input=data, capture_output=True, check=True)
                    stats = {key: int(value) for key, value in (line.split("=", 1) for line in result.stderr.decode().splitlines())}
                    checksum = hashlib.sha256(result.stdout).hexdigest()
                    if expected is None:
                        expected = checksum
                    if checksum != expected:
                        raise RuntimeError(f"output mismatch for {path}")
                    row = {"input": str(path), "pair": pair, "enabled": enabled,
                           "output_bytes": len(result.stdout), "output_sha256": checksum, **stats}
                    journal.write(json.dumps(row) + "\n")
                    journal.flush()
                    if pair >= 0:
                        rows.append(row)
            entry = {"input": str(path), "output_sha256": expected,
                     "output_bytes": rows[0]["output_bytes"],
                     "paired_differences": helpers["paired_intervals"](rows)}
            for enabled, name in [(False, "baseline"), (True, "candidate")]:
                entry[name] = {key: statistics.median(r[key] for r in rows if r["enabled"] == enabled) for key in stats}
            summary.append(entry)
            (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
            print(json.dumps(entry), flush=True)


if __name__ == "__main__":
    main()
