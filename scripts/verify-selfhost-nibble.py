#!/usr/bin/env python3
"""Check opt-in selfhost nibble transport, encodings, CIR and BF bootstrap."""
import argparse
import hashlib
import itertools
import json
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RLE_ONLY = ["--disable-clear", "--disable-scan", "--disable-transfer",
            "--disable-countdown", "--disable-compare", "--disable-remote-transfer"]
PROFILE = re.compile(rb"@BFCDBG2;|@ENDDBG;|@F[0-9a-fA-F]{4}:[0-9a-fA-F]*;|@C[0-9a-fA-F]{4};|@P[0-4];")


def normalized(data):
    data = PROFILE.sub(b"", data)
    encoded = data.startswith(b"@BFCRLE2;")
    if encoded:
        data = data[9:]
    pattern = rb"([+<>-])([0-9a-fA-F]*)|([\[\].,])" if encoded else rb"([+<>-])()|([\[\].,])"
    runs, end = [], 0
    for match in re.finditer(pattern, data):
        assert match.start() == end, data[end:end + 80]
        end = match.end()
        runs.append((match[1] or match[3], int(match[2] or b"1", 16)))
    assert end == len(data)
    return [(op, sum(n for _, n in group))
            for op, group in itertools.groupby(runs, key=lambda item: item[0])]


def fixtures():
    yield "scalar-bytes", """cell before;cell g;cell after;
cell f(cell depth){cell saved=g;if(depth){output(f(depth-1));}output(saved);return g;}
void main(){cell n;while(1){before=81;after=82;g=input();output(f(4));output(g);
output(before);output(after);n+=1;if(n==0){return;}}} """, bytes(range(256))
    yield "inline-bytes", """cell[31] a;cell g;
void main(){cell n;while(1){cell i=input();g=input();a[i]=g;output(a[i]);
a[i]+=17;output(a[i]);a[i]-=39;output(a[i]);output(g);n+=1;if(n==0){return;}}} """, \
        b"".join(bytes((v % 31, v)) for v in range(256))
    yield "portal-bytes", """struct Box{cell[17] pad;cell[3][256] a;cell tail;}Box box;cell marker;
void main(){cell n;while(1){cell p=input();cell i=input();cell v=input();
box.pad[16]=81;box.tail=82;marker=83;box.a[p][i]=v;output(box.a[p][i]);
box.a[p][i]+=17;output(box.a[p][i]);box.a[p][i]-=39;output(box.a[p][i]);
output(box.pad[16]);output(box.tail);output(marker);n+=1;if(n==0){return;}}} """, \
        b"".join(bytes((v % 3, v, 255 - v)) for v in range(256))
    yield "portal-recursion", """cell[3][256] a;cell g;
cell f(cell p,cell i,cell depth){cell saved=g;a[p][i]+=1;
if(depth){output(f((p+1),i,depth-1));}output(a[p][i]);output(saved);return a[p][i];}
void main(){g=91;cell i=input();cell p;a[p][i]=255;p+=1;a[p][i]=16;p+=1;a[p][i]=128;
output(f(0,i,2));output(g);output(a[p][i]);} """, b"\xff"
    yield "portal-deep", """cell[3][256] a;
void work(cell depth){if(depth){work(depth-1);}else{cell i;cell p=1;
while(1){a[p][i]=input();output(a[p][i]);i+=1;if(i==0){return;}}}}
void main(){work(64);} """, bytes(range(256))
    for path in sorted((ROOT / "selfhost/stage2/examples").glob("*.bfc")):
        yield path.stem, path.read_text(), b""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--baseline-source", type=Path, help="optional pre-change profile compiler source")
    parser.add_argument("--baseline-cir-source", type=Path)
    args = parser.parse_args()
    compiler, interpreter = str(args.compiler.resolve()), str(args.interpreter.resolve())
    (ROOT / "tmp").mkdir(exist_ok=True)
    if args.output_dir:
        args.output_dir.mkdir(parents=True, exist_ok=False)
        check(args, args.output_dir.resolve(), compiler, interpreter)
    else:
        with tempfile.TemporaryDirectory(prefix="selfhost-nibble-", dir=ROOT / "tmp") as directory:
            check(args, Path(directory), compiler, interpreter)


def check(args, work, compiler, interpreter):
    def run(command, data=b"", tag=None, timeout=240):
        result = subprocess.run(command, input=data, capture_output=True, timeout=timeout, cwd=ROOT)
        if tag:
            (work / f"{tag}.log").write_bytes(result.stderr)
        assert result.returncode == 0, (command, result.stderr.decode(errors="replace")[-2000:])
        return result

    def concat(entry, enabled):
        command = [str(ROOT / "scripts/concat-stage2-compiler.sh"), entry]
        if enabled:
            command.append("--enable-nibble-transfer")
        path = work / f"{entry}-{int(enabled)}.bfc"
        path.write_bytes(run(command).stdout)
        assert path.read_bytes().count(f"const cell NIBBLE_BF_TRANSFER = {int(enabled)};".encode()) == 1
        return path

    def generate(source, payload, tag):
        bf = run([compiler, "--run-ir", "--disable-function-inline", str(source)], payload, tag).stdout
        assert b"BFC_STAGE12_ERROR" not in bf, (tag, bf[-100:])
        (work / f"{tag}.bf").write_bytes(bf)
        return bf

    def execute(path, data, flags, tag):
        result = run([interpreter, "--unlimited-tape", "--no-progress", "--stats", *flags, str(path)], data, tag)
        stats = {m[1].decode(): int(m[2]) for m in re.finditer(rb"^([a-z_]+)=(\d+)$", result.stderr, re.M)}
        return result.stdout, stats

    sources = {enabled: concat("profile", enabled) for enabled in (False, True)}
    # Exercise the decomposition with dirty scratch and every possible byte.
    harness = work / "split.bfc"
    harness.write_text(sources[True].read_text().rsplit("void main() {", 1)[0] + """
void main(){
output('@');output('B');output('F');output('C');output('R');output('L');output('E');output('2');output(';');
cell position;position=emit_constant(position,21,1);position=emit_loop_open(position,21);
position=emit_clear(position,21);position=emit_input(position,16);
position=emit_constant(position,9,255);cell index=11;
while(index<=15){position=emit_constant(position,index,255);index+=1;}
position=emit_constant(position,10,255);
position=emit_nibble_split(position,16,9,11,15);
position=emit_move_to(position,11);compiler_output!('.');
position=emit_move_to(position,15);compiler_output!('.');
position=emit_move_to(position,9);compiler_output!('.');
position=emit_move_to(position,16);compiler_output!('.');
index=12;while(index<=14){position=emit_move_to(position,index);compiler_output!('.');index+=1;}
position=emit_move_to(position,10);compiler_output!('.');
position=emit_add_constant(position,20,1);position=emit_frame_copy(position,20,21);
position=emit_loop_close(position,21);
} """)
    generate(harness, b"", "split")
    split_expected = b"".join(bytes((v % 16, v // 16, 0, 0, 0, 0, 0, 0)) for v in range(256))
    for flags in ([], RLE_ONLY):
        assert execute(work / "split.bf", bytes(range(256)), flags, f"split-run-{len(flags)}")[0] == split_expected
    print("split: all 256 bytes, dirty scratch, native/RLE-only matched", flush=True)

    rows = []
    cases = list(fixtures())
    for name, program, data in cases:
        payload = program.encode()
        path = work / f"input-{name}.bfc"
        path.write_bytes(payload)
        expected = run([compiler, "--run-ir", str(path)], data).stdout
        for enabled in (False, True):
            tag = f"{name}-{int(enabled)}"
            bf = generate(sources[enabled], payload, tag)
            if not enabled and args.baseline_source:
                assert bf == generate(args.baseline_source.resolve(), payload, f"baseline-{name}"), (name, "default BF changed")
            logical = None
            for flags in ([], RLE_ONLY):
                output, stats = execute(work / f"{tag}.bf", data, flags, f"{tag}-run-{len(flags)}")
                assert output == expected, (tag, flags, output[:100], expected[:100])
                counts = tuple(stats[k] for k in ("executed_instructions", "executed_rle_instructions", "max_pointer"))
                assert logical is None or logical == counts, (tag, "logical counters changed")
                logical = counts
                rows.append(dict(case=name, nibble=enabled, rle_only=bool(flags), bytes=len(bf), **stats))
        print(f"{name}: outputs and logical counters matched", flush=True)
    (work / "results.json").write_text(json.dumps(rows, indent=2) + "\n")

    # Output encoding and profiling must not alter the nibble instruction stream.
    name, program, data = cases[0]
    reference = normalized((work / f"{name}-1.bf").read_bytes())
    for entry in ("main", "compressed"):
        bf = generate(concat(entry, True), program.encode(), f"encoding-{entry}")
        assert normalized(bf) == reference, entry
    print("main/compressed/profile: nibble instruction streams matched", flush=True)

    cir_sources = {enabled: concat("cir", enabled) for enabled in (False, True)}
    for name, program, data in cases[:4]:
        wire = [run([compiler, "--run-ir", "--disable-function-inline", str(cir_sources[e])], program.encode()).stdout for e in (False, True)]
        assert wire[0] == wire[1], (name, "CIR changed with BF option")
        if args.baseline_cir_source:
            assert wire[0] == run([compiler, "--run-ir", "--disable-function-inline", str(args.baseline_cir_source.resolve())], program.encode()).stdout
        cir = work / f"{name}.cir"
        cir.write_bytes(wire[0])
        result = run([compiler, "--cir-input", str(cir), "--compressed-bf", "--enable-nibble-transfer"])
        (work / f"{name}-rust-cir.bf").write_bytes(result.stdout)
        expected = run([compiler, "--run-ir", str(work / f"input-{name}.bfc")], data).stdout
        assert execute(work / f"{name}-rust-cir.bf", data, [], f"{name}-rust-cir-run")[0] == expected
    print("public CIR unchanged; Rust nibble re-lowering matched", flush=True)

    # Compile each configured compiler into BF using selfhost codegen through
    # the IR VM, then execute that BF compiler (not only the Rust-built stage1).
    for enabled in (False, True):
        tag = f"stage2-{int(enabled)}"
        generate(sources[enabled], sources[enabled].read_bytes(), tag)
        for name, program, _ in cases[:5]:
            emitted, stats = execute(work / f"{tag}.bf", program.encode(), [], f"{tag}-compile-{name}")
            assert emitted == (work / f"{name}-{int(enabled)}.bf").read_bytes(), (tag, name, "BF/IR codegen differ")
            rows.append(dict(case=f"compile-{name}", nibble=enabled, rle_only=False,
                             compiler_bytes=(work / f"{tag}.bf").stat().st_size, **stats))
            print(f"{tag} compiles {name}: RLE={stats['executed_rle_instructions']:,}", flush=True)
    (work / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    (work / "completion.json").write_text(json.dumps(dict(cases=len(cases), rows=len(rows),
        compiler_sha256=hashlib.sha256(Path(compiler).read_bytes()).hexdigest(),
        interpreter_sha256=hashlib.sha256(Path(interpreter).read_bytes()).hexdigest()), indent=2) + "\n")
    print(f"All selfhost nibble checks passed; results in {work}", flush=True)


if __name__ == "__main__":
    main()
