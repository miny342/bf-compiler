#!/usr/bin/env python3
"""Write the phase config for the isolated phase/portal overhead fixture."""

import json
import sys
from pathlib import Path


if len(sys.argv) != 3:
    raise SystemExit("usage: write_overhead_phase_config.py EXPERIMENT_ROOT ARTIFACT_ID")
root = Path(sys.argv[1]).resolve()
path = root / "source" / "phase-config-overhead.json"
if path.exists():
    raise SystemExit(f"refusing to overwrite phase config: {path}")
path.write_text(
    json.dumps(
        {
            "format": "bfc-ir-phase-config-v1",
            "artifact": {"kind": "source", "id": sys.argv[2]},
            "chunk_cells": [8, 16],
            "phases": [{"name": "main", "function_name": "main"}],
        },
        indent=2,
    )
    + "\n"
)
