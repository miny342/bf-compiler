#!/usr/bin/env python3
"""Summarize v2 phase/portal metrics without retaining an execution trace."""

import collections
import json
import sys
from pathlib import Path


if len(sys.argv) != 2:
    raise SystemExit("usage: summarize_phase_portal.py RUN_ROOT")
root = Path(sys.argv[1]).resolve()
output = root / "measurements" / "phase-portal-summary.json"
if output.exists():
    raise SystemExit(f"refusing to overwrite existing summary: {output}")
output.parent.mkdir(parents=True, exist_ok=True)

summary = {"format": "bfc-continuation-ir-phase-portal-summary-v1", "inputs": {}}
for family in ("source", "cir"):
    for input_name in ("hello", "stage5_functions", "stage8_aggregates"):
        path = root / "ir-phase-portal" / family / input_name / "metrics.json"
        report = json.loads(path.read_text())
        if report.get("format") != "bfc-continuation-ir-metrics-v2":
            raise SystemExit(f"not v2 phase metrics: {path}")
        if report.get("accounting", {}).get("ok") is not True:
            raise SystemExit(f"transition accounting failed: {path}")
        phase_metrics = report["phase_metrics"]
        continuation_totals = collections.Counter()
        function_totals = collections.Counter()
        for row in phase_metrics["continuations"]:
            continuation_totals[row["phase"]] += row["executions"]
            function_totals[(row["phase"], row["function_id"], row["function_name"])] += row[
                "executions"
            ]
        transition_totals = collections.Counter()
        for row in phase_metrics["transitions"]:
            transition_totals[row["phase"]] += row["count"]
        terminal_total = sum(row["count"] for row in phase_metrics["terminals"])
        if sum(continuation_totals.values()) != report["run"]["executed_continuations"]:
            raise SystemExit(f"phase continuation accounting failed: {path}")
        if sum(transition_totals.values()) + terminal_total != report["run"]["executed_continuations"]:
            raise SystemExit(f"phase transition/terminal accounting failed: {path}")
        region_totals = collections.Counter()
        request_totals = collections.Counter()
        for row in phase_metrics["portal"]["requests"]:
            region = row["region"]["kind"]
            region_totals[(row["phase"], region)] += row["requests"]
            request_totals[
                (
                    row["phase"],
                    row["function_id"],
                    row["function_name"],
                    region,
                    row["operation"],
                    row["cells"],
                )
            ] += row["requests"]
        by_phase = {row["phase"]: row for row in phase_metrics["portal"]["by_phase"]}
        if sum(region_totals.values()) != phase_metrics["portal"]["total_requests"]:
            raise SystemExit(f"portal region accounting failed: {path}")

        def top_rows(counter, fields, limit=10):
            rows = []
            for key, count in counter.most_common(limit):
                rows.append(dict(zip(fields, key)) | {"requests": count})
            return rows

        summary["inputs"][f"{family}/{input_name}"] = {
            "artifact_identity": report["artifact_identity"],
            "run": report["run"],
            "phase_continuation_totals": dict(sorted(continuation_totals.items())),
            "phase_transition_totals": dict(sorted(transition_totals.items())),
            "top_phase_functions": [
                {
                    "phase": phase,
                    "function_id": function_id,
                    "function_name": function_name,
                    "executions": count,
                }
                for (phase, function_id, function_name), count in function_totals.most_common(10)
            ],
            "portal_total_requests": phase_metrics["portal"]["total_requests"],
            "portal_region_totals": {
                f"{phase}/{region}": count
                for (phase, region), count in sorted(region_totals.items())
            },
            "top_portal_request_groups": top_rows(
                request_totals,
                ("phase", "function_id", "function_name", "region", "operation", "cells"),
            ),
            "portal_by_phase": {
                phase: {
                    "requests": row["requests"],
                    "adjacent_pairs": row["adjacent_pairs"],
                    "adjacent_same_region": row["adjacent_same_region"],
                    "adjacent_same_region_rate": row["adjacent_same_region_rate"],
                    "offset_delta_histogram": row["offset_delta_histogram"],
                    "start_chunk_revisits": row["start_chunk_revisits"],
                }
                for phase, row in sorted(by_phase.items())
            },
        }

output.write_text(json.dumps(summary, indent=2) + "\n")
