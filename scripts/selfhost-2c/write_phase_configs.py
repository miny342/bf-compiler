#!/usr/bin/env python3
"""Write identity-bound phase configurations below one experiment root."""

import json
import sys
from pathlib import Path


if len(sys.argv) not in (4, 5):
    raise SystemExit(
        "usage: write_phase_configs.py EXPERIMENT_ROOT SOURCE_ID CIR_ID [OVERHEAD_ID]"
    )

root = Path(sys.argv[1]).resolve()
source_id = sys.argv[2]
cir_id = sys.argv[3]
overhead_id = sys.argv[4] if len(sys.argv) == 5 else None


def write(path: Path, value: dict) -> None:
    if path.exists():
        raise SystemExit(f"refusing to overwrite phase config: {path}")
    path.write_text(json.dumps(value, indent=2) + "\n")


write(
    root / "source" / "phase-config-source.json",
    {
        "format": "bfc-ir-phase-config-v1",
        "artifact": {"kind": "source", "id": source_id},
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
        "artifact": {"kind": "cir", "id": cir_id},
        "chunk_cells": [8, 16],
        "phases": [
            {"name": "lexer", "function_id": 16},
            {"name": "parser", "function_id": 113},
            {"name": "macro_expansion", "function_id": 127},
            {"name": "semantic", "function_id": 161},
            {"name": "lowering", "function_id": 192},
            {"name": "codegen", "function_id": 241},
        ],
    },
)
if overhead_id is not None:
    write(
        root / "source" / "phase-config-overhead.json",
        {
            "format": "bfc-ir-phase-config-v1",
            "artifact": {"kind": "source", "id": overhead_id},
            "chunk_cells": [8, 16],
            "phases": [{"name": "main", "function_name": "main"}],
        },
    )
