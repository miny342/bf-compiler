#!/usr/bin/env python3
"""Compute the identity used by the direct IR runner phase configuration."""

import hashlib
import sys
from pathlib import Path


VERSION = b"bfc-ir-artifact-v1"


def frame(value: bytes) -> bytes:
    return len(value).to_bytes(8, "big") + value


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def source_identity(paths: list[str], inline_branch_successors: bool = True) -> str:
    data = bytearray(VERSION)
    data.extend(frame(b"source"))
    data.extend(frame(f"inline_branch_successors={str(inline_branch_successors).lower()}".encode()))
    data.extend(len(paths).to_bytes(8, "big"))
    for path in paths:
        data.extend(frame(Path(path).read_bytes()))
    return digest(bytes(data))


def cir_identity(path: str, inline_branch_successors: bool = True) -> str:
    data = bytearray(VERSION)
    data.extend(frame(b"cir"))
    data.extend(frame(f"inline_branch_successors={str(inline_branch_successors).lower()}".encode()))
    data.extend(frame(Path(path).read_bytes()))
    return digest(bytes(data))


if __name__ == "__main__":
    if len(sys.argv) < 3:
        raise SystemExit(
            "usage: ir_artifact_identity.py source|cir PATH... [--disable-2c]"
        )
    kind = sys.argv[1]
    disable = sys.argv[-1] == "--disable-2c"
    paths = sys.argv[2:-1] if disable else sys.argv[2:]
    if not paths:
        raise SystemExit("at least one input path is required")
    if kind == "source":
        print(source_identity(paths, not disable))
    elif kind == "cir" and len(paths) == 1:
        print(cir_identity(paths[0], not disable))
    else:
        raise SystemExit("cir identity requires exactly one input path")
