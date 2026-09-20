#!/usr/bin/env python3
"""Short paired interpreter benchmarks using the same selfhost compiler BF.

Build the baseline interpreter from the revision before compare-loop folding.
Use --profile-map for the same sample profiling mode as metrics.sh. Every run
has a timeout; the input is a small generated program, never compiler self-input.
"""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess


def wide_globals():
    lines = [f"cell[255][256] g{i};" for i in range(16)]
    lines += ["void main() {", "cell value;"]
    for turn in range(8):
        for i in range(16):
            row, col = (17 * i + 31 * turn) % 255, (13 * i + 47 * turn) % 256
            value = (i + turn) % 7 + 1
            lines += [f"g{i}[{row}][{col}] = {value};",
                      f"value = g{i}[{row}][{col}];", "output(value);"]
    return ("\n".join(lines + ["}"]) + "\n").encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--program", type=Path, required=True)
    parser.add_argument("--profile-map", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--pairs", type=int, default=4)
    parser.add_argument("--case", action="append", choices=[
        "hello", "stage7_globals", "stage8_aggregates", "wide-globals"])
    args = parser.parse_args()
    assert args.pairs > 0
    args.out.mkdir(parents=True, exist_ok=False)
    root = Path(__file__).resolve().parents[2]
    cases = args.case or ["hello", "stage7_globals", "stage8_aggregates", "wide-globals"]
    rows = []
    manifest = {name: {"path": str(path.resolve()),
                      "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                for name, path in [("baseline", args.baseline), ("candidate", args.candidate),
                                   ("program", args.program)]}
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    with (args.out / "runs.jsonl").open("w") as log:
        for case in cases:
            data = (wide_globals() if case == "wide-globals" else
                    (root / f"selfhost/stage2/examples/{case}.bfc").read_bytes())
            (args.out / f"{case}.bfc").write_bytes(data)
            reference = None
            for pair in range(args.pairs):
                order = ["baseline", "candidate"] if pair % 2 == 0 else ["candidate", "baseline"]
                for variant in order:
                    command = [str(getattr(args, variant).resolve()), "--unlimited-tape",
                               "--no-progress", "--stats", "--timings"]
                    profile = args.out / f"{case}-{pair}-{variant}.profile.json"
                    if args.profile_map:
                        command += ["--profile-map", str(args.profile_map), "--profile-mode", "sample",
                                    "--profile-format", "json", "--profile-output", str(profile)]
                    command.append(str(args.program))
                    result = subprocess.run(command, input=data, capture_output=True,
                                            check=True, timeout=60)
                    if args.profile_map:
                        report = json.loads(profile.read_text())
                        stats = report["run_stats"]
                        timings = report["phase_timings_ns"]
                    else:
                        values = {k: int(v) for k, v in
                                  (line.split("=", 1) for line in result.stderr.decode().splitlines())}
                        stats = {k: v for k, v in values.items() if not k.endswith("_ns")}
                        timings = {k[:-3]: v for k, v in values.items() if k.endswith("_ns")}
                    logical = {k: stats[k] for k in ["executed_instructions",
                               "executed_rle_instructions", "max_pointer"]}
                    if reference is None:
                        reference = (result.stdout, logical)
                        (args.out / f"{case}.out.bf").write_bytes(result.stdout)
                    assert (result.stdout, logical) == reference, (case, pair, variant)
                    row = dict(case=case, pair=pair, variant=variant, stats=stats, timings_ns=timings,
                               output_sha256=hashlib.sha256(result.stdout).hexdigest())
                    rows.append(row)
                    log.write(json.dumps(row) + "\n")
                    log.flush()
                    print(f"{case} {pair} {variant}: {timings['execute'] / 1e9:.6f}s", flush=True)
    summary = {}
    for case in cases:
        medians = {variant: statistics.median(r["timings_ns"]["execute"] / 1e9 for r in rows
                   if r["case"] == case and r["variant"] == variant) for variant in ["baseline", "candidate"]}
        summary[case] = dict(execute_seconds=medians,
                             reduction_percent=100 * (1 - medians["candidate"] / medians["baseline"]))
    (args.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
