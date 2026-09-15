#!/usr/bin/env python3
"""Bounded portal diagnostics using production code. All artifacts stay in ./tmp."""
import argparse
import hashlib
import json
import os
import signal
from pathlib import Path
import re
import statistics
import subprocess
import time

REPO = Path(__file__).resolve().parents[2]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def save(path, value):
    with path.open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def invoke(command, data=b'', env=None):
    command = list(map(str, command))
    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, cwd=REPO, start_new_session=True, env=env) as process:
        try:
            stdout, stderr = process.communicate(data, timeout=60)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            raise
        if process.returncode:
            raise RuntimeError(f"command failed ({process.returncode}): {command}\n{stderr.decode(errors='replace')}")
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)



def optimizer_source():
    directory = REPO / 'selfhost/stage2/compiler'
    arena = (directory / '06_arena.bfc').read_text()
    helpers = arena[arena.index('struct WideValue {'):arena.index('WideValue wide_multiply_length(')]
    return ('const cell COMPRESSED_BF_OUTPUT = 1;\n' + helpers
            + (directory / '09_bf_optimizer.bfc').read_text()
            + (directory / '09_bf_serialization.bfc').read_text() + r'''
macro compiler_output(value) { output(value); }
void fail(cell first, cell second) { output('E'); output(first); output(second); abort(); }
void main() {
    cell command = input();
    while (command != 0) {
        if (command == '!') { flush_bf_optimizer(); reset_bf_optimizer(); }
        else if (command == 'R') {
            cell character = input(); WideValue count;
            count.low = input(); count.mid = input(); count.high = input();
            emit_repeat_wide(character, count);
        } else { emit_bf_character(command); }
        command = input();
    }
    flush_bf_optimizer();
}
''')


def array_case(region='global', width=3, depth=0, padding=16, seed=255, large=False):
    declaration = ('cell[32][256] data;' if large else
                   ('Triple[16] data;' if width == 3 else 'cell[16] data;'))
    offsets = ([0, 15, 16, 255, 256, 4095, 4096, 8191] if large else list(range(16)))
    # Populate all payload chunks touched by these base-16 portal paths; static
    # stores avoid adding dynamic portal requests to the setup measurement.
    if large:
        chunks = {0}
        for offset in offsets:
            q = offset // 16
            position = 0
            for digit, jump in [(q % 16, 1), ((q // 16) % 16, 16), (q // 256, 256)]:
                for _ in range(digit):
                    position += jump
                    chunks.add(position - 1)  # swapped payload, prefix is chunk 0
            chunks.add(q)  # selected payload is one chunk beyond the context
        initialize = '\n'.join(f'data[{i // 256}][{i % 256}] = seed;'
                               for chunk in sorted(chunks) for i in range(chunk*16, chunk*16+16))
        access = 'cell high = input(); cell low = input(); cell value = input(); data[high][low] = value; output(data[high][low]);'
    elif width == 3:
        initialize = '\n'.join(f'data[{i}].{f} = seed;' for i in range(16) for f in 'abc')
        access = '''cell index = input(); Triple value;
value.a = input(); value.b = input(); value.c = input();
data[index] = value; Triple got = data[index];
output(got.a); output(got.b); output(got.c);'''
    else:
        initialize = '\n'.join(f'data[{i}] = seed;' for i in range(16))
        access = 'cell index = input(); cell value = input(); data[index] = value; output(data[index]);'
    observe = '\n'.join('output(' + line.split(' = ')[0] + ');' for line in initialize.splitlines())
    source = f'''struct Triple {{ cell a; cell b; cell c; }}
{declaration if region == 'global' else ''}
void exercise(cell depth) {{
    cell[{padding}] keep;
    {declaration if region == 'frame' else ''}
    keep[0] = 173; keep[{padding-1}] = 219;
    if (depth != 0) {{ exercise(depth - 1); }} else {{
        cell seed = input();
        {initialize}
        cell more = input();
        while (more != 0) {{ {access} more = input(); }}
        {observe}
    }}
    output(keep[0]); output(keep[{padding-1}]);
}}
void main() {{ exercise(input()); }}
'''
    records = b''
    for offset in offsets:
        address = bytes([offset // 256, offset % 256]) if large else bytes([offset])
        records += b'\1' + address + bytes([seed]) * width
    return dict(source=source, prefix=bytes([depth, seed]), block=records,
                records=len(offsets), suffix=b'\0', kind='array', region=region,
                width=width, depth=depth, padding=padding, seed=seed, large=large,
                portal_requests_per_record=2*width)


def cases():
    result = {}
    for name, kwargs in [
        ('global-byte-zero', dict(width=1, seed=0)),
        ('global-byte-full', dict(width=1)),
        ('global-triple-zero', dict(seed=0)),
        ('global-triple-full', {}),
        ('frame-triple-full', dict(region='frame')),
        ('global-triple-deep', dict(depth=8)),
        ('global-triple-wide', dict(padding=256)),
        ('frame-triple-deep', dict(region='frame', depth=8)),
        ('global-large', dict(width=1, large=True)),
    ]:
        result[name] = array_case(**kwargs)
    # No giant expanded output: compressed serializer is the actual production
    # one. Includes ring eviction, merging, cancellation, clear and IO barriers.
    block = b'>+'*24 + b'-<'*12 + b'+++[---]++[-].,'
    block += b'R>\xff\xff\x01R<\xfe\xff\x01' + b'!'
    result['optimizer'] = dict(source=optimizer_source(), prefix=b'', block=block,
                               suffix=b'\0', records=1, kind='optimizer')
    for padding in [16, 256]:
        block = b''.join(b'\1'+bytes((v+f*31) % 256 for f in range(7)) for v in range(256))
        result[f'transport-{padding}'] = dict(prefix=b'', block=block, suffix=b'\0',
            records=256, kind='transport', request_bytes=7)
    for value in [0, 1, 15, 16, 127, 255]:
        result[f'transport-v{value}'] = dict(prefix=b'', block=(b'\1'+bytes([value])*7)*256,
            suffix=b'\0', records=256, kind='transport', request_bytes=7,
            fixture='transport-16', controlled_value=value)
    return result


def category(key):
    # Exclusive samples partition the total. Navigation inside a fused
    # transport belongs to its transport ancestor when not separately visible.
    for prefix, name in [
        ('abi.portal.route.decompose', 'route.decompose'),
        ('abi.portal.route.pack', 'route.pack'),
        ('abi.portal.route.transport', 'route.transport'),
        ('abi.portal.stage.', 'request.prepare'),
        ('abi.portal.request', 'request.residual'),
        ('abi.portal.start', 'request.prepare'),
        ('abi.portal.window.exchange', 'window.exchange'),
        ('abi.portal.window.', 'window.control'),
        ('abi.portal.offset', 'offset'),
        ('abi.portal.payload.select', 'payload.select'),
        ('abi.portal.payload.transfer', 'payload.transfer'),
        ('abi.portal.load', 'payload.residual'),
        ('abi.portal.store', 'payload.residual'),
        ('abi.portal.resume', 'resume'),
        ('abi.navigation.global', 'navigation'),
        ('abi.portal.router.global.', 'router.residual'),
        ('abi.portal.route.field.', 'router.residual'),
        ('abi.dispatch.', 'dispatch'),
        ('fixture.', 'fixture'),
    ]:
        if key.startswith(prefix):
            return name
    return 'other'


def profile_summary(profile, requests):
    groups = {}
    by_key = {}
    for site in profile['sites']:
        key = site['stable_key']
        group = groups.setdefault(category(key), dict(samples=0, counters={}))
        group['samples'] += site['samples']
        row = by_key.setdefault(key, dict(samples=0, counters={}))
        row['samples'] += site['samples']
        for counter, value in site['counters'].items():
            if isinstance(value, int):
                combine = max if counter == 'maximum_pointer_observed' else lambda a,b: a+b
                group['counters'][counter] = combine(group['counters'].get(counter, 0), value)
                row['counters'][counter] = combine(row['counters'].get(counter, 0), value)
    total = profile['total_samples']
    assert sum(g['samples'] for g in groups.values()) == total
    for group in groups.values():
        group['sample_percent'] = group['samples'] / total * 100 if total else None
        group['low_confidence'] = bool(total and group['samples'] < 20)
        if requests:
            group['per_request'] = {k:v/requests for k,v in group['counters'].items() if k != 'maximum_pointer_observed'}
    return dict(groups=groups, by_key=by_key, total_samples=total,
                measured_execute_time_ns=profile['measured_execute_time_ns'],
                mixed_provenance_native_operations=profile['mixed_provenance_native_operations'],
                request_count=requests, note='Exclusive groups; fused work stays at its LCA. Scan counts are logical BF counts, not probe work.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--compiler', type=Path, default=REPO/'target/release/bfc')
    parser.add_argument('--interpreter', type=Path, default=REPO/'target/release/bf-interpreter')
    parser.add_argument('--baseline-compiler', type=Path)
    parser.add_argument('--cases', nargs='+', choices=list(cases()))
    parser.add_argument('--pairs', type=int, default=5)
    parser.add_argument('--seconds', type=float, default=2)
    args = parser.parse_args()
    if not 1 <= args.pairs <= 20 or not .05 <= args.seconds <= 5:
        parser.error('pairs must be 1..20; seconds must be .05..5')
    root = args.root.resolve()
    if not root.is_relative_to((REPO/'tmp').resolve()):
        parser.error('root must be a NEW directory under repository tmp')
    root.mkdir(parents=True, exist_ok=False)
    (root/'runner.py').write_bytes(Path(__file__).read_bytes())
    compiler, interpreter = args.compiler.resolve(), args.interpreter.resolve()
    selected = {k:v for k,v in cases().items() if not args.cases or k in args.cases}
    inputs = [compiler, interpreter, Path(__file__), REPO/'crates/bf-compiler/src/abi_codegen.rs',
              REPO/'crates/bf-compiler/src/abi_codegen/portal_probe.rs',
              *[REPO/'selfhost/stage2/compiler'/f for f in ['06_arena.bfc','09_bf_optimizer.bfc','09_bf_serialization.bfc']]]
    if args.baseline_compiler:
        inputs.append(args.baseline_compiler.resolve())
    save(root/'manifest.json', dict(commit=invoke(['git','rev-parse','HEAD']).stdout.decode().strip(),
        tracked_diff_sha256=digest(invoke(['git','diff']).stdout), pairs=args.pairs, seconds=args.seconds,
        files={str(p):dict(sha256=digest(p.read_bytes()), bytes=p.stat().st_size) for p in inputs},
        cases=list(selected), timeout_seconds=60, timing='unprofiled execute_ns; separate sample/counters runs'))
    if any(c['kind']=='transport' for c in selected.values()):
        env = dict(os.environ, BFC_PORTAL_PROBE_OUTPUT=str(root))
        result = invoke(['cargo','test','--release','-p','bf-compiler','--lib',
            'abi_codegen::portal_probe::export_portal_transport_fixtures','--','--ignored','--exact'],
            env=env)
        (root/'export.log').write_bytes(result.stdout+result.stderr)
    summaries = {}
    for name, case in selected.items():
        print(f'{name}: build and calibrate', flush=True)
        bf, map_path = root/f'{name}.bf', root/f'{name}.bfmap.json'
        source = root/f'{name}.bfc'
        if 'fixture' in case:
            bf.write_bytes((root/(case['fixture']+'.bf')).read_bytes())
            map_path.write_bytes((root/(case['fixture']+'.bfmap.json')).read_bytes())
        if 'source' in case:
            source.write_text(case['source'])
            command = ['--compressed-bf','--unlimited-tape','--profile-granularity','continuation',
                       '--profile-map-output',map_path,source]
            built = invoke([compiler,*command])
            bf.write_bytes(built.stdout)
            (root/f'{name}.build.log').write_bytes(built.stderr)
            if args.baseline_compiler:
                old_map = root/f'{name}.baseline.bfmap.json'
                old = invoke([args.baseline_compiler.resolve(),*command[:5],old_map,source])
                assert old.stdout == built.stdout, f'profiling changed BF: {name}'
                (root/f'{name}.baseline.build.log').write_bytes(old.stderr)
        if bf.stat().st_size > 64*1024*1024:
            raise RuntimeError(f'fixture exceeded 64 MiB compact BF limit: {name}')

        def payload(repeats):
            return case['prefix'] + case['block']*repeats + case['suffix']

        def execute(data, enabled=True, profile=None, stem=None):
            cmd = [interpreter,'--unlimited-tape','--no-progress','--stats','--timings']
            if not enabled:
                cmd += ['--disable-remote-transfer']
            if profile:
                cmd += ['--profile-map',map_path,'--profile-mode',profile,
                        '--profile-output',root/f'{stem}.profile.json','--profile-format','json']
            start = time.perf_counter_ns()
            run = invoke(['/usr/bin/time', '-f', 'max_rss_kib=%M', *cmd,bf], data)
            elapsed = time.perf_counter_ns()-start
            stats = {k:int(v) for k,v in re.findall(rb'^(\w+)=(\d+)$',run.stderr,re.M)}
            stats = {k.decode():v for k,v in stats.items()}
            stats['process_total_ns'] = elapsed
            if stem:
                (root/f'{stem}.log').write_bytes(run.stderr)
            return run.stdout, stats

        # Calibrate both settings; do not let disabled RemoteTransfer turn a
        # seconds-long benchmark into an unbounded run. Maximum input is 16 MiB.
        repetitions = 1
        for _ in range(3):
            times = [execute(payload(repetitions), enabled)[1]['execute_ns']/1e9 for enabled in [True,False]]
            if max(times) >= args.seconds*.5:
                break
            repetitions = min(max(repetitions+1, int(repetitions*args.seconds/max(max(times),.001))),
                              max(1, 16*1024*1024//len(case['block'])))
        data = payload(repetitions)
        (root/f'{name}.input').write_bytes(data)
        if case['kind']=='transport':
            expected = b''.join(data[i+1:i+8] for i in range(0,len(data)-1,8))
            requests = repetitions*case['records']
        else:
            oracle = invoke([compiler,'--run-ir',source],data)
            expected = oracle.stdout
            (root/f'{name}.ir.log').write_bytes(oracle.stderr)
            requests = repetitions*case['records']*case['portal_requests_per_record'] if case['kind']=='array' else None
            if case['kind']=='array':
                # Exactly one load and store per logical element, no hidden
                # dynamic accesses in setup/validation. IR counters prove it.
                counters = dict(re.findall(rb'(aggregate_loads|aggregate_stores|array_loads|array_stores)=(\d+)',oracle.stderr))
                actual = sum(int(v) for v in counters.values())
                assert actual == repetitions*case['records']*2, (name,actual,repetitions)
        (root/f'{name}.expected').write_bytes(expected)
        rows = []
        invariant = None
        for pair in range(-1,args.pairs):
            for enabled in ([True,False] if pair % 2 else [False,True]):
                out,stats = execute(data,enabled,stem=f'{name}.{pair}.{enabled}')
                assert out == expected, f'output mismatch: {name}, remote={enabled}'
                current = [stats[k] for k in ['executed_instructions','executed_rle_instructions','max_pointer']]
                if invariant is None:
                    invariant = current
                assert invariant == current, f'logical counter mismatch: {name}'
                if pair >= 0:
                    rows.append(dict(pair=pair,remote_transfer=enabled,**stats))
        profiles = {}
        for enabled in [True,False]:
            for mode in ['sample','counters']:
                stem=f'{name}.{enabled}.{mode}'
                out,_ = execute(data,enabled,mode,stem)
                assert out == expected
                raw = json.loads((root/f'{stem}.profile.json').read_text())
                profiles[f'{enabled}.{mode}'] = profile_summary(raw['profile'],requests)
        means = {str(enabled):{k:statistics.median(row[k] for row in rows if row['remote_transfer']==enabled)
                               for k in rows[0] if k not in ['pair','remote_transfer']} for enabled in [True,False]}
        summary = dict(case={k:v for k,v in case.items() if k not in ['source','prefix','block','suffix']},
            repetitions=repetitions, input_sha256=digest(data), output_sha256=digest(expected),
            artifacts={p.name:dict(sha256=digest(p.read_bytes()),bytes=p.stat().st_size) for p in [bf,map_path]},
            medians=means, runs=rows, profiles=profiles)
        summary['request_sites'] = [site['attributes'] for site in json.loads(map_path.read_text())['sites'] if site['stable_key'] == 'abi.portal.request']
        summaries[name]=summary
        save(root/f'{name}.summary.json',summary)
        save(root/f'{name}.runs.json',rows)
        print(f'{name}: ON={means["True"]["execute_ns"]/1e9:.3f}s OFF={means["False"]["execute_ns"]/1e9:.3f}s repeats={repetitions}',flush=True)
    save(root/'summary.json',summaries)
    lines = ['# Portal diagnostic run', '',
             'Unprofiled median execute time. ON/OFF share the same BF and input.', '',
             '| Case | BF bytes | ON seconds | OFF seconds | ON ns/request |',
             '|---|---:|---:|---:|---:|']
    for name, summary in summaries.items():
        on, off = summary['medians']['True'], summary['medians']['False']
        requests = summary['profiles']['True.sample']['request_count']
        per_request = f"{on['execute_ns']/requests:.0f}" if requests else 'n/a'
        lines.append(f"| {name} | {summary['artifacts'][name+'.bf']['bytes']} | {on['execute_ns']/1e9:.3f} | {off['execute_ns']/1e9:.3f} | {per_request} |")
    lines += ['', '## Exclusive sample groups (RemoteTransfer ON)', '',
              'Counts below 20 samples are low confidence. Fused work remains at its common ancestor.', '']
    for name, summary in summaries.items():
        profile = summary['profiles']['True.sample']
        lines += [f'### {name}', '', f"Total samples: {profile['total_samples']}", '']
        for key, group in sorted(profile['groups'].items(), key=lambda kv:-kv[1]['samples']):
            if group['samples']:
                lines.append(f"- {key}: {group['sample_percent']:.1f}% ({group['samples']} samples" + (', low confidence)' if group['low_confidence'] else ')'))
        lines.append('')
    lines += ['The fixture reproduces production templates, not the complete full14 phase mix.',
              'Per-request totals include fixture setup, output and cleanup; stage counters separate their costs.']
    (root/'report.md').write_text('\n'.join(lines)+'\n')
    print(f'Completed: {root}',flush=True)


if __name__ == '__main__':
    main()
