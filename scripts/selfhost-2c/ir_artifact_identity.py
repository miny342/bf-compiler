#!/usr/bin/env python3
"""Compute the identity used by the direct IR runner phase configuration."""

import hashlib
import argparse
from pathlib import Path


VERSION = b"bfc-ir-artifact-v4"


def frame(value: bytes) -> bytes:
    return len(value).to_bytes(8, "big") + value


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def source_identity(paths: list[str], inline_branch_successors: bool = True,
                    structure_local_control_flow: bool = True) -> str:
    data = bytearray(VERSION)
    data.extend(frame(b"source"))
    data.extend(frame(f"inline_branch_successors={str(inline_branch_successors).lower()}".encode()))
    data.extend(frame(f"structure_local_control_flow={str(structure_local_control_flow).lower()}".encode()))
    data.extend(len(paths).to_bytes(8, "big"))
    for path in paths:
        data.extend(frame(Path(path).read_bytes()))
    return digest(bytes(data))


def cir_identity(path: str, inline_branch_successors: bool = True,
                 structure_local_control_flow: bool = True) -> str:
    data = bytearray(VERSION)
    data.extend(frame(b"cir"))
    data.extend(frame(f"inline_branch_successors={str(inline_branch_successors).lower()}".encode()))
    data.extend(frame(f"structure_local_control_flow={str(structure_local_control_flow).lower()}".encode()))
    data.extend(frame(Path(path).read_bytes()))
    return digest(bytes(data))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=["source", "cir"])
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--disable-2c", action="store_true")
    parser.add_argument("--enable-local-control-flow", dest="local", action="store_true")
    parser.add_argument("--disable-local-control-flow", dest="local", action="store_false")
    parser.set_defaults(local=True)
    args = parser.parse_args()
    kind, paths = args.kind, args.paths
    if kind == "source":
        print(source_identity(paths, not args.disable_2c, args.local))
    elif kind == "cir" and len(paths) == 1:
        print(cir_identity(paths[0], not args.disable_2c, args.local))
    else:
        raise SystemExit("cir identity requires exactly one input path")
