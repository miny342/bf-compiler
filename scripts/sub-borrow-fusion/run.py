#!/usr/bin/env python3
"""Paired fusion benchmarks; keep artifacts and exact outputs under ./tmp."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--interpreter', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True, help='saved compressed stage2 source')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--pairs', type=int, default=5)
    args = parser.parse_args()
    assert args.pairs > 0
    work = args.output.resolve()
    work.mkdir(parents=True, exist_ok=False)
    source = args.source.read_text()
    harness = source.split('void main() {')[0] + '''void main() {
        WideValue count;
        count.low = 255; count.mid = 255; count.high = 255;
        while (count.high != 0) {
            emit_repeat_wide('>', count);
            count.high -= 1;
        }
    }
'''
    serialization = work / 'serialization.bfc'
    serialization.write_text(harness)
    pairs_input = bytes(v for a in range(256) for b in range(256) for v in (1, a, b)) + b'\0'
    pairs_expected = bytes(v for a in range(256) for b in range(256)
                           for v in ((a-b) % 256, a < b, a, b))
    cases = [
        ('pairs', Path(__file__).with_name('pairs.bfc'), pairs_input, pairs_expected),
        ('serialization', serialization, b'', ''.join(f'>{(h << 16) + 65535:08d}'
                                                    for h in range(255, 0, -1)).encode()),
        ('hello', args.source, (ROOT / 'selfhost/stage2/examples/hello.bfc').read_bytes(), None),
        ('aggregates', args.source, (ROOT / 'selfhost/stage2/examples/stage8_aggregates.bfc').read_bytes(), None),
    ]
    rows, sizes = [], {}
    for name, source_path, data, expected in cases:
        sizes[name] = {}
        for variant in ('baseline', 'candidate'):
            binary = getattr(args, variant).resolve()
            bf = work / f'{name}-{variant}.bf'
            with bf.open('wb') as output:
                subprocess.run([binary, '--unlimited-tape', '--compressed-bf', source_path.resolve()],
                               stdout=output, check=True)
            sizes[name][variant] = dict(bytes=bf.stat().st_size, sha256=hashlib.sha256(bf.read_bytes()).hexdigest())
        for pair in range(args.pairs):
            order = ('baseline', 'candidate') if pair % 2 == 0 else ('candidate', 'baseline')
            for variant in order:
                started = time.perf_counter()
                result = subprocess.run([args.interpreter.resolve(), '--unlimited-tape', '--stats', '--timings',
                                         work / f'{name}-{variant}.bf'], input=data,
                                        capture_output=True, check=True)
                elapsed = time.perf_counter() - started
                if expected is None:
                    expected = result.stdout
                assert result.stdout == expected, (name, pair, variant)
                counters = {key: float(value) if '.' in value else int(value) for key, value in
                            (line.split('=', 1) for line in result.stderr.decode().splitlines())}
                row = dict(case=name, pair=pair, variant=variant, seconds=elapsed, counters=counters)
                rows.append(row)
                print(json.dumps(row), flush=True)
        (work / f'{name}.expected').write_bytes(expected)
    summary = dict(sizes=sizes, medians={name: {variant: statistics.median(
        row['seconds'] for row in rows if row['case'] == name and row['variant'] == variant)
        for variant in ('baseline', 'candidate')} for name, *_ in cases},
        execute_median_ms={name: {variant: statistics.median(
            row['counters']['execute_ns'] / 1e6 for row in rows
            if row['case'] == name and row['variant'] == variant)
            for variant in ('baseline', 'candidate')} for name, *_ in cases})
    (work / 'results.json').write_text(json.dumps(dict(summary=summary, rows=rows), indent=2) + '\n')
    print(json.dumps(summary), flush=True)


if __name__ == '__main__':
    main()
