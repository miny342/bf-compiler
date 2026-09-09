#!/usr/bin/env python3
"""Write identity-bound phase configurations for the verified production CIR."""

import hashlib
import json
import sys
from pathlib import Path

from ir_artifact_identity import cir_identity, source_identity

if len(sys.argv) not in (2, 3):
    raise SystemExit("usage: write_phase_configs.py EXPERIMENT_ROOT [--disable-2c]")

root = Path(sys.argv[1]).resolve()
inline_branch_successors = sys.argv[2:] != ["--disable-2c"]
source_program = root / "source" / "stage2-compiler.bfc"
cir_program = root / "artifacts" / "cir" / "stage2-compiler.cir"
source_id = source_identity([str(source_program)], inline_branch_successors)
cir_id = cir_identity(str(cir_program), inline_branch_successors)

# These IDs were verified against the fixed production CIR and the source/CIR
# function correspondence recorded in the phase-portal evaluation. A new CIR
# must not inherit these IDs without a fresh correspondence check.
verified_cir_phase_ids = {
    "c7ff5b09e53714b4e9a2278ee4aee36c8fc88d868f49138c5c1a632fcdb5c2bb": [
        ("lexer", 16),
        ("parser", 113),
        ("macro_expansion", 127),
        ("semantic", 161),
        ("lowering", 192),
        ("codegen", 241),
    ]
}
cir_raw_sha256 = hashlib.sha256(cir_program.read_bytes()).hexdigest()
if inline_branch_successors and cir_raw_sha256 not in verified_cir_phase_ids:
    raise SystemExit(
        "refusing unknown production CIR hash; verify function IDs before adding it"
    )
if not inline_branch_successors:
    raise SystemExit("refusing unverified fixed-CIR mapping with --disable-2c")
cir_phases = verified_cir_phase_ids.get(cir_raw_sha256)
if cir_phases is None:
    raise SystemExit("no verified phase/function mapping for this production CIR")

lowering_options = {"inline_branch_successors": inline_branch_successors}


def write(path: Path, value: dict) -> None:
    if path.exists():
        raise SystemExit(f"refusing to overwrite phase config: {path}")
    path.write_text(json.dumps(value, indent=2) + "\n")


write(
    root / "source" / "phase-config-source.json",
    {
        "format": "bfc-ir-phase-config-v1",
        "artifact": {
            "kind": "source",
            "id": source_id,
            "identity_version": "bfc-ir-artifact-v1",
            "lowering_options": lowering_options,
        },
        "chunk_cells": [8, 16],
        "phases": [
            {"name": "lexer", "function_name": "next_token"},
            {"name": "parser", "function_name": "parse_ast_program"},
            {"name": "macro_expansion", "function_name": "expand_ast_macros"},
            {"name": "semantic", "function_name": "analyze_ast_program"},
            {"name": "lowering", "function_name": "lower_ast_program"},
            {"name": "codegen", "function_name": "emit_continuation_program"},
        ],
    },
)
write(
    root / "source" / "phase-config-cir.json",
    {
        "format": "bfc-ir-phase-config-v1",
        "artifact": {
            "kind": "cir",
            "id": cir_id,
            "identity_version": "bfc-ir-artifact-v1",
            "lowering_options": lowering_options,
        },
        "chunk_cells": [8, 16],
        "phases": [
            {"name": name, "function_id": function_id}
            for name, function_id in cir_phases
        ],
    },
)
