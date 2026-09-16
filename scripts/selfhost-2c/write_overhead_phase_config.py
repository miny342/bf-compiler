#!/usr/bin/env python3
"""Write the phase config for the isolated phase/portal overhead fixture."""

import json
import argparse
import sys
from pathlib import Path

# Keep generated cache files out of the source directory.
sys.dont_write_bytecode = True
from ir_artifact_identity import VERSION, source_identity

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("root", type=Path)
parser.add_argument("--enable-local-control-flow", dest="local", action="store_true")
parser.add_argument("--disable-local-control-flow", dest="local", action="store_false")
parser.set_defaults(local=True)
args = parser.parse_args()
root = args.root.resolve()
path = root / "source" / "phase-config-overhead.json"
if path.exists():
    raise SystemExit(f"refusing to overwrite phase config: {path}")
program = root / "source" / "phase-portal-overhead.bfc"
artifact_id = source_identity([str(program)], structure_local_control_flow=args.local)
path.write_text(
    json.dumps(
        {
            "format": "bfc-ir-phase-config-v1",
            "artifact": {
                "kind": "source",
                "id": artifact_id,
                "identity_version": VERSION.decode(),
                "lowering_options": {"inline_branch_successors": True,
                                     "structure_local_control_flow": args.local},
            },
            "chunk_cells": [16],
            "phases": [{"name": "main", "function_name": "main"}],
        },
        indent=2,
    )
    + "\n"
)
print(artifact_id)
