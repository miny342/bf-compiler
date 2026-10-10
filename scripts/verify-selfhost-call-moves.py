#!/usr/bin/env python3
"""Check owned call arguments and direct return transport in the selfhost ABI."""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RLE_ONLY = ["--disable-clear", "--disable-scan", "--disable-transfer",
            "--disable-countdown", "--disable-compare", "--disable-remote-transfer"]
PROGRAM = """
struct Triple { cell x; cell y; cell z; }
struct Box { cell pad; Triple value; }
Triple global;
cell sum(cell a, cell b) { return a+b; }
Triple make(cell a) { Triple p; p.x=a; p.y=a+1; p.z=a+2; return p; }
Triple identity(Triple p) { return p; }
Triple repeated(Triple a, Triple b, cell n) {
    Triple r; r.x=a.x+b.y+n; r.y=a.y+b.z; r.z=a.z+b.x; return r;
}
cell change(cell n) { global.x=n; global.y=n+1; global.z=n+2; return n; }
cell first(Triple p, cell n) { output(p.x);output(p.y);output(p.z);return n; }
Triple get_global() { return global; }
Triple recurse(Triple p, cell d) {
    if(d) { Triple q=recurse(p,d-1); q.x+=1; return q; }
    return p;
}
cell recur_scalar(cell a, cell d) { if(d) { return recur_scalar(a+1,d-1); } return a; }
cell even(cell d, cell v) { if(d) { return odd(d-1,v+1); } return v; }
cell odd(cell d, cell v) { if(d) { return even(d-1,v+1); } return v; }
cell[32] echo(cell[32] p) { return p; }
Triple pick_field(Triple p) { Box b; b.value=p; return b.value; }
Triple pick_element(Triple p) { Triple[2] a; a[1]=p; return a[1]; }
cell scalar_field(Triple p) { return p.y; }
cell pick_byte(cell[32] p) { return p[31]; }
cell pick_dynamic(cell[32] p, cell index) { return p[index]; }
cell global_y() { return global.y; }
void discard(Triple p) {}
void main() {
    while(input()) {
        cell a=input(); cell b=input();
        cell n=sum(a,a);output(n);output(a);
        Triple p=make(a); Triple q=identity(p);
        output(q.x);output(q.y);output(q.z);output(p.x);output(p.y);output(p.z);
        Triple r=repeated(p,p,b);output(r.x);output(r.y);output(r.z);
        output(p.x);output(p.y);output(p.z);
        global=p;
        // The first argument must remain the old snapshot when change runs.
        cell t=first(global,change(b));output(t);
        Triple g=get_global();output(g.x);output(g.y);output(g.z);
        output(global.x);output(global.y);output(global.z);
        Triple deep=recurse(p,3);output(deep.x);output(deep.y);output(deep.z);
        output(p.x);output(p.y);output(p.z);
        Triple nested=identity(identity(make(b)));
        output(nested.x);output(nested.y);output(nested.z);
        discard(p);output(p.x);output(p.y);output(p.z);
        output(recur_scalar(a,3));output(a);output(b);
        // Reuse the same free frames at different depths, up to 255 calls.
        output(even(a,b));output(a);output(b);
        Triple f=pick_field(p);output(f.x);output(f.y);output(f.z);
        Triple e=pick_element(p);output(e.x);output(e.y);output(e.z);
        output(scalar_field(p));
        cell[32] payload; cell j;
        while(j<32) { payload[j]=a+j; j+=1; }
        cell[32] copy=echo(payload);j=0;
        while(j<32) { output(copy[j]);output(payload[j]);j+=1; }
        output(pick_byte(payload));output(payload[31]);output(global_y());
        cell k;if(a<32){k=a;}
        output(pick_dynamic(payload,k));output(payload[k]);
    }
}
"""


def fixture(values):
    data, expected = bytearray(), bytearray()
    for a in values:
        b = (a * 17 + 255) & 255
        p = [a, (a+1) & 255, (a+2) & 255]
        g = [b, (b+1) & 255, (b+2) & 255]
        data.extend((1, a, b))
        result = [2*a, a, *p, *p, p[0]+p[1]+b, p[1]+p[2], p[2]+p[0], *p,
                  *p, b, *g, *g, a+3, p[1], p[2], *p, *g, *p, a+3, a, b,
                  a+b, a, b, *p, *p, p[1]]
        result.extend(x for j in range(32) for x in (a+j, a+j))
        k = a if a < 32 else 0
        result.extend((a+31, a+31, g[1], a+k, a+k))
        expected.extend(x & 255 for x in result)
    data.append(0)
    return bytes(data), bytes(expected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    parser.add_argument("--selfhost-compiler", type=Path)
    parser.add_argument("--enable-nibble-transfer", action="store_true")
    args = parser.parse_args()
    compiler, interpreter = str(args.compiler.resolve()), str(args.interpreter.resolve())
    flags = ["--enable-nibble-transfer"] if args.enable_nibble_transfer else []

    def run(command, data=b""):
        result = subprocess.run(command, input=data, capture_output=True, cwd=ROOT, timeout=300)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        return result.stdout

    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="selfhost-call-moves-", dir=ROOT / "tmp") as directory:
        work = Path(directory)
        source = work / "program.bfc"
        source.write_text(PROGRAM)
        data, expected = fixture(range(256))
        small_data, small_expected = fixture([0, 255])
        assert run([compiler, "--run-ir", str(source)], data) == expected
        variants = []
        for entry in ["profile", "cir"]:
            driver = work / (entry + "-compiler.bfc")
            driver.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), entry, *flags]))
            generated = run([compiler, "--run-ir", "--no-ir-transitions", str(driver)], PROGRAM.encode())
            output = work / ("program.bf" if entry == "profile" else "program.cir")
            output.write_bytes(generated)
            if entry == "profile":
                assert generated.startswith(b"@BFCRLE2;@BFCDBG2;")
                variants.append(output)
                if args.selfhost_compiler:
                    assert generated == run([interpreter, "--unlimited-tape", "--no-progress",
                                             str(args.selfhost_compiler.resolve())], PROGRAM.encode())
            else:
                assert generated.startswith(b"BFCIR\0\x02\n")
                assert run([compiler, "--cir-input", str(output), "--run-ir"], data) == expected
                for name, codegen in [("default", []), ("triple", ["--enable-nibble-transfer",
                                     "--enable-inplace-compare", "--enable-anchor-bank"])]:
                    bf = work / ("cir-" + name + ".bf")
                    bf.write_bytes(run([compiler, "--cir-input", str(output), "--unlimited-tape",
                                        "--compressed-bf", *codegen]))
                    variants.append(bf)
            print(entry + ": generated and checked", flush=True)
        for bf in variants:
            command = [interpreter, "--unlimited-tape", "--no-progress"]
            assert run([*command, str(bf)], data) == expected, bf.name
            assert run([*command, *RLE_ONLY, str(bf)], small_data) == small_expected, bf.name
            print(f"{bf.name}: 256 inputs and RLE-only 0/255 passed", flush=True)
        print("Call moves: duplicate arguments, late global mutation, nested/recursive calls, "
              "mutual recursion through depth 255 and frame reuse, "
              "static field/index returns and dynamic/global fallbacks, "
              "global/local/scalar/32-cell returns, source preservation, and void returns passed.", flush=True)


if __name__ == "__main__":
    main()
