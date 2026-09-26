#!/usr/bin/env python3
"""Check shared selfhost portals, physical aliases, bounds and bounded code size."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    parser.add_argument("--selfhost-compiler", type=Path,
                        help="also generate each fixture with this BF compiler and compare bytes")
    args = parser.parse_args()
    compiler = str(args.compiler.resolve())
    interpreter = str(args.interpreter.resolve())
    (ROOT / "tmp").mkdir(exist_ok=True)

    def run(command, data=b""):
        result = subprocess.run(command, input=data, capture_output=True, timeout=180)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        return result

    with tempfile.TemporaryDirectory(prefix="selfhost-portals-", dir=ROOT / "tmp") as directory:
        work = Path(directory)
        source = work / "compiler.bfc"
        source.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), "profile"]).stdout)

        def check(name, program, data, expected=None, maximum_bytes=150_000, ordinary=False):
            path = work / f"{name}.bfc"
            path.write_text(program)
            if expected is None:
                expected = run([compiler, "--run-ir", str(path)], data).stdout
            bf = run([compiler, "--run-ir", "--disable-function-inline", str(source)],
                     program.encode()).stdout
            assert b"BFC_STAGE12_ERROR" not in bf, bf[-100:]
            if args.selfhost_compiler:
                bootstrapped = run([interpreter, "--unlimited-tape", "--no-progress",
                                    str(args.selfhost_compiler.resolve())], program.encode()).stdout
                assert bootstrapped == bf, (name, "BF and IR compiler outputs differ")
            assert len(bf) < maximum_bytes, (name, len(bf))
            artifact = path.with_suffix(".bf")
            artifact.write_bytes(bf)
            result = run([interpreter, "--unlimited-tape", "--no-progress",
                          "--accept-embedded-profile", "--profile-mode", "counters",
                          "--profile-format", "json", str(artifact)], data)
            assert result.stdout == expected, (name, result.stdout[:100], expected[:100])
            report = json.loads(result.stderr)
            functions = {s["label"] for s in report["profile"]["sites"] if s["kind"] == "function"}
            assert {"portal0", "portal1", "portal2", "portal3"} <= functions
            if ordinary:
                plain_source = work / "plain-compiler.bfc"
                plain_source.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), "main"]).stdout)
                plain = run([compiler, "--run-ir", "--disable-function-inline", str(plain_source)],
                            program.encode()).stdout
                encoded = re.sub(rb"@[^;]*;", b"", bf)
                expanded = b"".join(m[1] * int(m[2] or b"1", 16) if m[1] else m[3]
                                    for m in re.finditer(rb"([+<>-])([0-9a-fA-F]*)|([\[\].,])", encoded))
                assert plain == expanded
                artifact.write_bytes(plain)
                assert run([interpreter, "--unlimited-tape", "--no-progress", str(artifact)], data).stdout == expected
            print(f"{name}: {len(bf)} bytes, {len(expected)} output bytes matched", flush=True)
            return len(bf)

        accesses = [(0, i) for i in range(256)] + [(p, 253) for p in range(255)]
        accesses += [(p, i) for p in (1, 15, 16, 17, 31, 32, 127, 128, 254)
                     for i in (0, 1, 15, 16, 254, 255)]
        data = b"".join(bytes((p, i, (p * 19 + i * 7) % 256)) for p, i in accesses) + b"\xff"
        body = """void main(){before=91;after=92;while(1){cell p=input();if(p==255){return;}
            cell i=input();cell v=input();
            TARGET[p][i]=v;output(TARGET[p][i]);
            TARGET[p][i]+=17;output(TARGET[p][i]);
            TARGET[p][i]-=39;output(TARGET[p][i]);
            output(before);output(after);}}
        """
        expected = b"".join(bytes((v, (v + 17) % 256, (v - 22) % 256, 91, 92))
                             for p, i in accesses for v in [(p * 19 + i * 7) % 256])
        check("unaligned-full", "cell before;cell[255][256] data;cell after;"
              + body.replace("TARGET", "data"), data, expected)

        # A nested array starts at an unaligned offset in its enclosing struct.
        check("nested-offset", "struct Box{cell[254] prefix;cell[255][256] data;cell tail;}"
              "cell before;Box box;cell after;" + body.replace("TARGET", "box.data"),
              data, expected)

        # 65,520 payload cells, offset by 32: the last access needs a carry into
        # page 256 even though its logical index still fits 16 bits.
        check("page-256-carry", """cell[32] prefix;cell[16][15][7][39] a;cell tail;
            void main(){cell i=input();cell j=input();cell k=input();cell l=input();
                prefix[31]=81;tail=82;a[i][j][k][l]=67;
                output(a[i][j][k][l]);output(a[15][14][6][38]);
                output(prefix[31]);output(tail);}""", bytes((15, 14, 6, 38)), b"CCQR")

        bounds = """cell[3][129] data;void main(){cell p=input();cell i=input();
            data[p][i]=77;output(data[p][i]);data[p][i]+=3;output(data[p][i]);
            data[p][i]-=5;output(data[p][i]);output(data[0][0]);}"""
        check("partial-page", bounds, bytes((2, 128)))
        check("out-of-bounds", bounds, bytes((3, 0)), bytes(4))

        recursive = """cell[255][256] a;cell marker;cell[255][256] b;
            cell visit(cell p,cell depth){a[p][255]=p;b[p][0]=255-p;
                if(depth){output(visit(p+1,depth-1));}
                output(a[p][255]);output(b[p][0]);return a[p][255]+1;}
            void main(){marker=89;output(visit(15,3));output(marker);
                output(a[15][255]);output(b[18][0]);}"""
        check("recursive-regions", recursive, b"")

        calls = ("cell[255][256] a;" + "".join(
            f"cell f{i}(){{return {i % 256};}}" for i in range(270))
            + "void main(){cell p=input();cell i=input();a[p][i]=f255();"
              "output(a[p][i]);a[p][i]+=f257();output(a[p][i]);output(f256());}")
        check("dispatch-pages", calls, bytes((254, 255)), bytes((255, 0, 0)), maximum_bytes=500_000)

        # The same load/store sites remain essentially constant size as the
        # array grows from one page to 255 pages; no per-element case expansion.
        sizes = []
        for pages in (1, 16, 255):
            program = (f"cell[{pages}][256] a;void main(){{cell p=input();cell i=input();"
                       "a[p][i]=65;output(a[p][i]);}")
            sizes.append(check(f"size-{pages}", program, bytes((pages - 1, 255)), b"A", ordinary=pages == 1))
        assert max(sizes) - min(sizes) < 2_000, sizes
        print(f"1/16/255-page code sizes: {sizes}", flush=True)

        # Exhaust the constant-bound predicate independently of array lowering.
        # Each generated predicate reads all 256 possible bytes, must report
        # value < limit, consume its input and preserve an enclosing BF loop.
        harness = work / "bounds.bfc"
        harness.write_text(source.read_text().rsplit("void main() {", 1)[0] + """
void main(){
    output('@');output('B');output('F');output('C');output('R');
    output('L');output('E');output('2');output(';');
    cell limit;cell position;
    while(1){
        position=emit_input(position,3);
        position=emit_portal_less_constant(position,3,4,limit);
        position=emit_move_to(position,4);compiler_output!('.');
        position=emit_move_to(position,3);compiler_output!('.');
        position=emit_constant(position,20,255);
        position=emit_loop_open(position,20);compiler_output!('-');
        position=emit_input(position,3);
        position=emit_portal_less_constant(position,3,4,limit);
        position=emit_move_to(position,4);compiler_output!('.');
        position=emit_move_to(position,3);compiler_output!('.');
        position=emit_loop_close(position,20);
        limit+=1;if(limit==0){return;}
    }
}
""")
        artifact = work / "bounds.bf"
        artifact.write_bytes(run([compiler, "--run-ir", "--disable-function-inline", str(harness)]).stdout)
        expected = b"".join(bytes((value < limit, 0)) for limit in range(256) for value in range(256))
        assert run([interpreter, "--unlimited-tape", "--no-progress", str(artifact)],
                   bytes(range(256)) * 256).stdout == expected
        print("constant bounds: all 65,536 value/limit pairs passed", flush=True)


if __name__ == "__main__":
    main()
