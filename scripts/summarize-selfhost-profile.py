#!/usr/bin/env python3
"""Summarize a source-granularity self-host profile.

The profile report stores source byte spans on compiler-generated sites.  This
tool aggregates all attributed sites by source span, prints the usual ABI and
function summaries, and can make a non-compiling inspection copy of the BFC
source with per-line sample comments.
"""

import argparse
import collections
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def load(path):
    return json.loads(path.read_text())


def context(site, kind, by_id):
    while site:
        if site["kind"] == kind:
            return site["stable_key"]
        site = by_id.get(site.get("parent"))
    return "(outside function/continuation sites)"


def reduction(current, old):
    if old == 0:
        return "n/a"
    return f"{(old - current) / old * 100:7.3f}%"


def pct(value, total):
    return f"{value / total * 100:7.3f}%" if total else "n/a"


def duration_by_key(sites, by_id, kind):
    result = collections.Counter()
    for site in sites:
        key = site["stable_key"] if kind == "key" else context(site, kind, by_id)
        result[key] += site["exclusive_duration_ns"]
    return result


def source_file_paths(report_path, report):
    paths = {}
    for file in report["artifact"].get("files", []):
        path = Path(file["path"])
        if not path.is_absolute():
            path = ROOT / path
        paths[file["id"]] = path
    return paths


def source_location(file_path, start, end, cache):
    data = cache.setdefault(file_path, file_path.read_bytes())
    start = min(start, len(data))
    line = data.count(b"\n", 0, start) + 1
    line_start = data.rfind(b"\n", 0, start) + 1
    column = start - line_start + 1
    next_newline = data.find(b"\n", start)
    if next_newline < 0:
        next_newline = len(data)
    text = data[line_start:next_newline].decode("utf-8", errors="replace").strip()
    return line, column, text, start, min(end, len(data))


def aggregate_sources(report_path, report, sites, stable_key=None):
    files = source_file_paths(report_path, report)
    cache = {}
    result = {}
    for site in sites:
        if stable_key is not None and site["stable_key"] != stable_key:
            continue
        span = site.get("source")
        if not span or span["file_id"] not in files:
            continue
        key = (span["file_id"], span["start_byte"], span["end_byte"])
        row = result.setdefault(
            key,
            {
                "file_id": span["file_id"],
                "start": span["start_byte"],
                "end": span["end_byte"],
                "exclusive": 0,
                "inclusive": 0,
                "samples": 0,
                "entries": 0,
                "sites": 0,
            },
        )
        row["exclusive"] += site["exclusive_duration_ns"]
        row["inclusive"] += site["inclusive_duration_ns"]
        row["samples"] += site.get("samples", 0)
        row["entries"] += site.get("profile_block_executions", 0)
        row["sites"] += 1
    for row in result.values():
        path = files[row["file_id"]]
        line, column, text, start, end = source_location(
            path, row["start"], row["end"], cache
        )
        row.update(path=path, line=line, column=column, text=text, start=start, end=end)
    return list(result.values())


def write_annotated_source(path, output, source_rows, total):
    data = path.read_bytes()
    line_rows = collections.defaultdict(list)
    for row in source_rows:
        line_rows[row["line"]].append(row)
    lines = data.splitlines(keepends=True)
    annotated = []
    for number, line in enumerate(lines, 1):
        rows = line_rows.get(number, [])
        if rows:
            exclusive = sum(row["exclusive"] for row in rows)
            samples = sum(row["samples"] for row in rows)
            spans = len(rows)
            comment = (
                f"// PROFILE samples={samples} "
                f"exclusive_s={exclusive / 1e9:.6f} "
                f"percent={exclusive / total * 100:.3f} spans={spans}\n"
            )
            annotated.append(comment.encode())
        annotated.append(line)
    output.write_bytes(b"".join(annotated))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--annotated-source", type=Path)
    parser.add_argument("--ir", type=Path, default=ROOT / "tmp/full25-ir.txt")
    parser.add_argument("--top", type=int, default=30)
    args = parser.parse_args()

    report = load(args.report)
    sites = report["profile"]["sites"]
    by_id = {site["id"]: site for site in sites}
    total = report["profile"]["measured_execute_time_ns"]
    source_rows = aggregate_sources(args.report, report, sites)
    names = {}
    source_path = next(iter(source_file_paths(args.report, report).values()), None)
    if report.get("version", 1) == 1 and args.ir.exists():
        names = dict(re.findall(r"^FUNCTION (\d+) (\S+)", args.ir.read_text(), re.M))
    for site in sites:
        match = re.fullmatch(r"function\.(\d+)", site["stable_key"])
        if site["kind"] == "function" and match and site.get("attributes", {}).get("name"):
            names[match[1]] = site["attributes"]["name"]

    lines = []
    write = lines.append
    write(f"Selfhost profile summary: {args.report}")
    write("=" * (len(lines[-1])))
    write("")
    write("This is sample-attributed time. It is not exact per-site call-stack time.")
    write("")
    write("Overall")
    write("-------")
    write(f"Execute: {total / 1e9:.3f} seconds")
    write(f"Process: {report['phase_timings_ns']['process_total'] / 1e9:.3f} seconds")
    write(f"Parse: {report['phase_timings_ns']['parse'] / 1e9:.3f} seconds")
    write(f"Profile map read: {report['phase_timings_ns']['profile_map_read'] / 1e9:.3f} seconds")
    write(f"Output write: {report['phase_timings_ns']['output_write'] / 1e9:.3f} seconds")
    write(f"Artifact instructions: {report['artifact']['instruction_count']}")
    write(f"Artifact FNV-1a: {report['artifact']['fnv1a64']}")
    write(f"Hash kind: {report['artifact'].get('hash_kind', 'expanded_bf')}")
    write(f"Sampling interval: {report['profile']['sampling_interval_ns'] / 1e6:.3f} ms")
    write(f"Total samples: {report['profile']['total_samples']}")
    write(f"Profile sites: {len(sites)}")
    write(f"Source span groups: {len(source_rows)}")
    write(f"Mixed-provenance native operations: {report['profile']['mixed_provenance_native_operations']}")
    write("")

    write("Run files")
    write("---------")
    for name in ["stage2-compiler.bfc", "tmp.bf", "tmp.bfmap.json", "tmp.tmp.bf"]:
        path = args.report.parent / name
        if path.exists():
            write(f"{name}: {path.stat().st_size} bytes")
    write("")

    if args.baseline and args.baseline.exists():
        baseline = load(args.baseline)
        old_total = baseline["profile"]["measured_execute_time_ns"]
        write("Baseline comparison")
        write("-------------------")
        write(f"Baseline: {args.baseline}")
        write(f"Baseline execute: {old_total / 1e9:.3f} seconds")
        write(f"Baseline -> current execute reduction: {reduction(total, old_total)}")
        for key in ["executed_instructions", "executed_rle_instructions", "max_pointer"]:
            old = baseline["run_stats"][key]
            current = report["run_stats"][key]
            write(f"{key}={old} -> {current} ({reduction(current, old)})")
        for key in sorted(report["run_stats"]["optimization"]):
            old = baseline["run_stats"]["optimization"].get(key, 0)
            current = report["run_stats"]["optimization"][key]
            write(f"optimization.{key}={old} -> {current} ({reduction(current, old)})")
        old_sites = baseline["profile"]["sites"]
        old_by_id = {site["id"]: site for site in old_sites}
        old_keys = duration_by_key(old_sites, old_by_id, "key")
        current_keys = duration_by_key(sites, by_id, "key")
        write("")
        write("Stable-key changes")
        for key, current in current_keys.most_common(args.top):
            old = old_keys.get(key, 0)
            write(f"{key}: {old / 1e9:.3f}s -> {current / 1e9:.3f}s ({reduction(current, old)})")
        write("")

    for kind in ["key", "function", "continuation"]:
        write(kind)
        counts = duration_by_key(sites, by_id, kind)
        for key, duration in counts.most_common(args.top):
            match = re.match(r"function\.(\d+)", key)
            name = names.get(match[1], "") if match else ""
            write(f"{pct(duration, total)} {duration / 1e9:10.3f}s {key} {name}")
        write("")

    write("Source locations")
    write("----------------")
    for row in sorted(source_rows, key=lambda row: (row["exclusive"], row["samples"]), reverse=True)[: args.top]:
        try:
            display_path = row["path"].relative_to(ROOT)
        except ValueError:
            display_path = row["path"]
        write(
            f"{pct(row['exclusive'], total)} {row['exclusive'] / 1e9:10.3f}s "
            f"samples={row['samples']} sites={row['sites']} "
            f"{display_path}:{row['line']}:{row['column']} {row['text']}"
        )
    write("")

    write("Source locations by hot stable key")
    write("----------------------------------")
    for stable_key in [
        "abi.navigation.global",
        "abi.frame.copy",
        "abi.frame.compare",
        "abi.dispatch.page.countdown",
    ]:
        rows = aggregate_sources(args.report, report, sites, stable_key)
        write(stable_key)
        if not rows:
            write("  (no source-attributed sites)")
            continue
        for row in sorted(rows, key=lambda row: (row["exclusive"], row["samples"]), reverse=True)[:10]:
            try:
                display_path = row["path"].relative_to(ROOT)
            except ValueError:
                display_path = row["path"]
            write(
                f"  {pct(row['exclusive'], total)} {row['exclusive'] / 1e9:10.3f}s "
                f"samples={row['samples']} {display_path}:{row['line']}:{row['column']} "
                f"{row['text']}"
            )
        write("")

    write("Top sites")
    write("---------")
    for site in sorted(sites, key=lambda item: item["exclusive_duration_ns"], reverse=True)[: args.top]:
        write(
            f"{pct(site['exclusive_duration_ns'], total)} "
            f"{site['exclusive_duration_ns'] / 1e9:10.3f}s "
            f"samples={site.get('samples', 0)} {site['stable_key']} "
            f"{context(site, 'continuation', by_id)}"
        )
    write("")

    write("Counter sums across profile sites")
    counter_keys = sorted({key for site in sites for key in site["counters"]})
    for key in counter_keys:
        value = sum(site["counters"].get(key, 0) for site in sites)
        write(f"{key}={value}")

    args.output.write_text("\n".join(lines) + "\n")
    if args.annotated_source:
        if source_path is None:
            raise SystemExit("profile has no source file")
        write_annotated_source(source_path, args.annotated_source, source_rows, total)
    print(args.output)
    if args.annotated_source:
        print(args.annotated_source)


if __name__ == "__main__":
    main()
