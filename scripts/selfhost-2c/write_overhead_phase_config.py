#!/usr/bin/env python3
"""Write the phase config for the isolated phase/portal overhead fixture."""

import json
import sys
from pathlib import Path

from ir_artifact_identity import source_identity

if len(sys.argv) != 2:
    raise SystemExit("usage: write_overhead_phase_config.py EXPERIMENT_ROOT")
root = Path(sys.argv[1]).resolve()
path = root / "source" / "phase-config-overhead.json"
if path.exists():
    raise SystemExit(f"refusing to overwrite phase config: {path}")
program = root / "source" / "phase-portal-overhead.bfc"
artifact_id = source_identity([str(program)])
path.write_text(
    json.dumps(
        {
            "format": "bfc-ir-phase-config-v1",
            "artifact": {
                "kind": "source",
                "id": artifact_id,
                "identity_version": "bfc-ir-artifact-v1",
                "lowering_options": {"inline_branch_successors": True},
            },
            "chunk_cells": [8, 16],
            "phases": [{"name": "main", "function_name": "main"}],
        },
        indent=2,
    )
    + "\n"
)
print(artifact_id)
