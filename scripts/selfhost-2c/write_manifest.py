#!/usr/bin/env python3
import json
import os
import platform
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


if len(sys.argv) != 3:
    raise SystemExit("usage: write_manifest.py EXPERIMENT_ROOT REPOSITORY_ROOT")
ROOT = Path(sys.argv[1]).resolve()
REPO = Path(sys.argv[2]).resolve()


def command(*args):
    return subprocess.run(args, cwd=REPO, check=True, text=True, capture_output=True).stdout.strip()


def sha(path):
    return subprocess.run(("sha256sum", str(path)), check=True, text=True, capture_output=True).stdout.split()[0]


manifest = {
    "recorded_at": datetime.now(timezone.utc).isoformat(),
    "repository": str(REPO),
    "branch": command("git", "branch", "--show-current"),
    "head": command("git", "rev-parse", "HEAD"),
    "baseline": "bd3560470b9f7c5c8f06f413d1a8cd6224878c34",
    "baseline_checkout": str(ROOT / "baseline-checkout"),
    "working_tree_status": command("git", "status", "--short").splitlines(),
    "build": {
        "profile": "release",
        "rustc": command("rustc", "--version"),
        "cargo": command("cargo", "--version"),
        "target": "native Linux x86_64",
        "compiler_baseline": str(ROOT / "bin/bfc-baseline"),
        "compiler_candidate": str(ROOT / "bin/bfc-candidate"),
        "interpreter": str(ROOT / "bin/bf-interpreter-candidate"),
    },
    "environment": {
        "date": command("date", "--iso-8601=ns"),
        "uname": platform.platform(),
        "cpu_count": os.cpu_count(),
        "loadavg": list(os.getloadavg()),
        "memory": Path("/proc/meminfo").read_text().splitlines()[:5],
        "process_snapshot": command("ps", "-eo", "pid,stat,comm").splitlines(),
        "external_load": "Observed process snapshot only; external load was not independently controlled.",
    },
    "sources": {
        name: {"path": str(ROOT / "source" / name), "sha256": sha(ROOT / "source" / name)}
        for name in (
            "stage2-compiler.bfc", "stage2-cir-compiler.bfc", "hello.bfc",
            "stage5_functions.bfc", "stage8_aggregates.bfc", "arena_micro.bfc",
            "ir-overhead.bfc",
        )
    },
    "fixed_cir": {
        "path": str(ROOT / "artifacts/cir/stage2-compiler.cir"),
        "sha256": sha(ROOT / "artifacts/cir/stage2-compiler.cir"),
        "bytes": (ROOT / "artifacts/cir/stage2-compiler.cir").stat().st_size,
    },
    "commands": {
        "build_artifacts": "scripts/selfhost-2c/build_artifacts.sh RUN_ROOT REPOSITORY_ROOT",
        "measure": "scripts/selfhost-2c/run_measurements.sh RUN_ROOT REPOSITORY_ROOT",
        "summarize": "python3 scripts/selfhost-2c/summarize_measurements.py RUN_ROOT",
        "ir_metrics": "scripts/selfhost-2c/run_ir_metrics.sh RUN_ROOT REPOSITORY_ROOT",
        "ir_phase_portal": "scripts/selfhost-2c/run_ir_phase_portal.sh RUN_ROOT REPOSITORY_ROOT",
        "ir_artifact_identity": "python3 scripts/selfhost-2c/ir_artifact_identity.py source|cir PATH...",
        "write_phase_configs": "python3 scripts/selfhost-2c/write_phase_configs.py RUN_ROOT",
        "ir_phase_portal_summary": "python3 scripts/selfhost-2c/summarize_phase_portal.py RUN_ROOT",
        "ir_overhead": "scripts/selfhost-2c/run_ir_overhead.sh RUN_ROOT REPOSITORY_ROOT",
        "phase_portal_overhead": "scripts/selfhost-2c/run_phase_portal_overhead.sh RUN_ROOT REPOSITORY_ROOT",
        "phase_portal_overhead_summary": "python3 scripts/selfhost-2c/summarize_phase_portal_overhead.py RUN_ROOT",
        "ir_overhead_summary": "python3 scripts/selfhost-2c/summarize_ir_overhead.py RUN_ROOT",
        "arena": "scripts/selfhost-2c/run_arena_measurements.sh RUN_ROOT REPOSITORY_ROOT",
        "arena_summary": "python3 scripts/selfhost-2c/summarize_arena.py RUN_ROOT",
        "profile": "scripts/selfhost-2c/run_hello_profiles.sh RUN_ROOT REPOSITORY_ROOT",
        "tests": "TMPDIR=RUN_ROOT/test-tmp cargo test --workspace",
    },
}
(ROOT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
