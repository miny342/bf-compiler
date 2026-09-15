#!/usr/bin/env python3
"""Paired comparison of unary/nibble BF, independently for RemoteTransfer ON/OFF."""
import argparse
import json
import os
from pathlib import Path
import random
import re
import statistics
import sys
sys.dont_write_bytecode = True
import run as diag


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('root', type=Path)
    p.add_argument('--compiler', type=Path, default=diag.REPO/'target/release/bfc')
    p.add_argument('--interpreter', type=Path, default=diag.REPO/'target/release/bf-interpreter')
    p.add_argument('--baseline-compiler', type=Path)
    p.add_argument('--cases', nargs='+', choices=list(diag.cases()), default=[
        'scalar', 'optimizer', 'global-triple-full', 'global-triple-deep',
        'global-large', 'transport-16', 'transport-65280'])
    p.add_argument('--pairs', type=int, default=5)
    p.add_argument('--seconds', type=float, default=1)
    args = p.parse_args()
    if not 1 <= args.pairs <= 20 or not .05 <= args.seconds <= 5:
        p.error('pairs must be 1..20; seconds must be .05..5')
    root = args.root.resolve()
    if not root.is_relative_to((diag.REPO/'tmp').resolve()):
        p.error('output must be a new directory under repository tmp')
    root.mkdir(parents=True, exist_ok=False)
    (root/'compare-transfer.py').write_bytes(Path(__file__).read_bytes())
    (root/'run.py').write_bytes(Path(diag.__file__).read_bytes())
    compiler, interpreter = args.compiler.resolve(), args.interpreter.resolve()
    identities = [compiler, interpreter, Path(__file__), Path(diag.__file__),
        diag.REPO/'crates/bf-compiler/src/abi_codegen.rs',
        diag.REPO/'crates/bf-compiler/src/abi_codegen/portal_probe.rs']
    if args.baseline_compiler:
        identities.append(args.baseline_compiler.resolve())
    diag.save(root/'manifest.json', dict(
        commit=diag.invoke(['git','rev-parse','HEAD']).stdout.decode().strip(),
        diff_sha256=diag.digest(diag.invoke(['git','diff']).stdout),
        files={str(f):diag.digest(f.read_bytes()) for f in identities},
        pairs=args.pairs, seconds=args.seconds, cases=args.cases,
        note='Unprofiled AB/BA; repetition count shared within each RemoteTransfer setting. Compare normalized times across settings.'))
    variants = ['unary','nibble']
    for variant in variants:
        directory=root/variant
        directory.mkdir()
        if any(diag.cases()[name]['kind']=='transport' for name in args.cases):
            env=dict(os.environ, BFC_PORTAL_PROBE_OUTPUT=str(directory),
                     BFC_PORTAL_NIBBLE_TRANSFER=str(int(variant=='nibble')))
            result=diag.invoke(['cargo','test','--release','-p','bf-compiler','--lib',
                'abi_codegen::portal_probe::export_portal_transport_fixtures','--','--ignored','--exact'],env=env)
            (directory/'export.log').write_bytes(result.stdout+result.stderr)
    summaries={}
    for name in args.cases:
        print(f'{name}: build',flush=True)
        case=diag.cases()[name]
        paths={}
        for variant in variants:
            directory=root/variant
            if 'source' in case:
                source=directory/f'{name}.bfc';source.write_text(case['source'])
                cmd=['--compressed-bf','--unlimited-tape','--profile-map-output',directory/f'{name}.bfmap.json',source]
                result=diag.invoke([compiler,*(['--enable-nibble-transfer'] if variant=='nibble' else []),*cmd])
                (directory/f'{name}.bf').write_bytes(result.stdout)
                if variant=='nibble' and args.baseline_compiler:
                    old=diag.invoke([args.baseline_compiler.resolve(),'--compressed-bf','--unlimited-tape',source])
                    assert old.stdout==result.stdout, f'nibble opt-in must preserve previous BF: {name}'
            paths[variant]=directory/f"{case.get('fixture',name)}.bf"
            assert paths[variant].stat().st_size<=64*1024*1024
        def payload(repeats):
            return case['prefix']+case['block']*repeats+case['suffix']
        def execute(variant,rt,data):
            cmd=[interpreter,'--unlimited-tape','--no-progress','--stats','--timings']
            if not rt:cmd.append('--disable-remote-transfer')
            result=diag.invoke(['/usr/bin/time','-f','max_rss_kib=%M',*cmd,paths[variant]],data)
            stats={k.decode():int(v) for k,v in re.findall(rb'^(\w+)=(\d+)$',result.stderr,re.M)}
            return result.stdout,stats,result.stderr
        summary=dict(bf={v:dict(bytes=f.stat().st_size,sha256=diag.digest(f.read_bytes())) for v,f in paths.items()}, settings={})
        # Same BF must preserve logical counters when only interpreter RT changes.
        for variant in variants:
            a=execute(variant,False,payload(1));b=execute(variant,True,payload(1))
            assert a[0]==b[0]
            for key in ['executed_instructions','executed_rle_instructions','max_pointer']:
                assert a[1][key]==b[1][key],(name,variant,key)
        for rt in [True,False]:
            repeats=1
            for _ in range(3):
                slow=max(execute(v,rt,payload(repeats))[1]['execute_ns'] for v in variants)/1e9
                if slow>=args.seconds*.5:break
                repeats=min(max(repeats+1,int(repeats*args.seconds/max(slow,.001))),
                            max(1,16*1024*1024//len(case['block'])))
            data=payload(repeats)
            (root/f'{name}.{rt}.input').write_bytes(data)
            if case['kind']=='transport':
                expected=b''.join(data[i+1:i+8] for i in range(0,len(data)-1,8))
            else:
                ir=diag.invoke([compiler,'--run-ir',root/'unary'/f'{name}.bfc'],data)
                expected=ir.stdout
                (root/f'{name}.{rt}.ir.log').write_bytes(ir.stderr)
            (root/f'{name}.{rt}.expected').write_bytes(expected)
            rows=[]
            for pair in range(-1,args.pairs):
                for variant in (variants if pair%2 else list(reversed(variants))):
                    out,stats,log=execute(variant,rt,data)
                    assert out==expected,(name,variant,rt)
                    (root/f'{name}.{rt}.{pair}.{variant}.log').write_bytes(log)
                    if pair>=0:rows.append(dict(pair=pair,variant=variant,**stats))
            medians={v:{k:statistics.median(r[k] for r in rows if r['variant']==v)
                        for k in rows[0] if k not in ['pair','variant']} for v in variants}
            differences=[next(r['execute_ns'] for r in rows if r['pair']==i and r['variant']=='unary')-
                         next(r['execute_ns'] for r in rows if r['pair']==i and r['variant']=='nibble') for i in range(args.pairs)]
            rng=random.Random(20260916)
            boot=sorted(statistics.median(rng.choices(differences,k=len(differences))) for _ in range(10000))
            summary['settings'][str(rt)]=dict(repeats=repeats,records=repeats*case['records'],
                input_sha256=diag.digest(data),output_sha256=diag.digest(expected), medians=medians,runs=rows,
                unary_minus_nibble_ci95_ns=[boot[250],boot[9750]])
            print(f"{name} RT={rt}: unary={medians['unary']['execute_ns']/1e9:.4f}s nibble={medians['nibble']['execute_ns']/1e9:.4f}s repeats={repeats}",flush=True)
        summaries[name]=summary
        diag.save(root/f'{name}.summary.json',summary)
    diag.save(root/'summary.json',summaries)
    lines=['# Unary / nibble transfer comparison','','Times are unprofiled, normalized per input record (include fixture overhead).',
        'Negative change favors unary. Repeats are identical within each paired comparison.','',
        '| Case | RemoteTransfer | Unary ns/record | Nibble ns/record | Change |',
        '|---|---|---:|---:|---:|']
    for name,summary in summaries.items():
        for rt,s in summary['settings'].items():
            u,n=(s['medians'][v]['execute_ns']/s['records'] for v in variants)
            lines.append(f'| {name} | {rt} | {u:.0f} | {n:.0f} | {(u/n-1)*100:+.1f}% |')
    (root/'report.md').write_text('\n'.join(lines)+'\n')
    print(f'Completed: {root}',flush=True)


if __name__=='__main__':main()
