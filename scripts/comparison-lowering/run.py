#!/usr/bin/env python3
"""Paired ordinary-BF benchmark of every unsigned pair and four relations."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--baseline', type=Path, required=True)
parser.add_argument('--candidate', type=Path, required=True)
parser.add_argument('--interpreter', type=Path, required=True)
parser.add_argument('--pairs', type=int, default=5)
args = parser.parse_args()
assert args.pairs > 0
root = Path(__file__).resolve().parents[2]
(root / 'tmp').mkdir(exist_ok=True)
source = Path(__file__).with_name('pairs.bfc')
expected = bytes(v for a in range(256) for b in range(256)
                 for v in (a < b, a <= b, a > b, a >= b, a, b))
rows = []
sizes = {}
with tempfile.TemporaryDirectory(prefix='comparison-', dir=root / 'tmp') as directory:
    directory = Path(directory)
    for name in ('baseline', 'candidate'):
        target = directory / f'{name}.bf'
        with target.open('wb') as output:
            subprocess.run([getattr(args, name).resolve(), source], stdout=output, check=True)
        sizes[name] = target.stat().st_size
    for pair in range(args.pairs):
        order = ('baseline', 'candidate') if pair % 2 == 0 else ('candidate', 'baseline')
        for name in order:
            started = time.perf_counter()
            result = subprocess.run([args.interpreter.resolve(), '--stats', directory / f'{name}.bf'],
                                    capture_output=True, check=True)
            elapsed = time.perf_counter() - started
            assert result.stdout == expected, (pair, name)
            counters = {key: int(value) for key, value in
                        (line.split('=', 1) for line in result.stderr.decode().splitlines())}
            row = dict(pair=pair, variant=name, process_seconds=elapsed, counters=counters)
            rows.append(row)
            print(json.dumps(row), flush=True)
print(json.dumps(dict(ordinary_bf_bytes=sizes, output_bytes=len(expected),
    output_sha256=hashlib.sha256(expected).hexdigest(),
    median_process_seconds={name: statistics.median(row['process_seconds'] for row in rows
        if row['variant'] == name) for name in ('baseline', 'candidate')})), flush=True)
