#!/usr/bin/env python3
"""Exhaust selfhost ordering comparisons and the SLIDE scratch/alias contract."""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RLE_ONLY = ["--disable-clear", "--disable-scan", "--disable-transfer",
            "--disable-countdown", "--disable-compare", "--disable-remote-transfer"]
PROGRAM = """void main(){while(input()){cell a=input();cell b=input();
    output(a<b);output(a<=b);output(a>b);output(a>=b);output(a);output(b);}}"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    parser.add_argument("--selfhost-compiler", type=Path)
    args = parser.parse_args()
    compiler, interpreter = str(args.compiler.resolve()), str(args.interpreter.resolve())

    def run(command, data=b""):
        result = subprocess.run(command, input=data, capture_output=True, timeout=300, cwd=ROOT)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        return result.stdout

    pairs = [(a, b) for a in range(256) for b in range(256)]
    selected = [(0, 0), (0, 255), (255, 0), (255, 255), (1, 1),
                (1, 255), (255, 1), (127, 128), (128, 127), (254, 255)]

    def data(rows):
        return b"".join(bytes((1, a, b)) for a, b in rows) + b"\0"

    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="selfhost-comparisons-", dir=ROOT / "tmp") as directory:
        work = Path(directory)
        source = work / "compiler.bfc"
        source.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), "profile"]))
        program = work / "relations.bf"
        bf = run([compiler, "--run-ir", "--no-ir-transitions", str(source)], PROGRAM.encode())
        if args.selfhost_compiler:
            assert bf == run([interpreter, "--unlimited-tape", "--no-progress",
                              str(args.selfhost_compiler.resolve())], PROGRAM.encode())
        program.write_bytes(bf)
        for rows, flags in [(pairs, []), (selected, RLE_ONLY)]:
            expected = b"".join(bytes((a < b, a <= b, a > b, a >= b, a, b)) for a, b in rows)
            assert run([interpreter, "--unlimited-tape", "--no-progress", *flags,
                        str(program)], data(rows)) == expected
        print("All four relations: 65,536 pairs, operand preservation, and RLE-only edges passed.", flush=True)

        # Exercise the real emitter with dirty private scratch, unrelated live
        # data between operands, and the separate left==right fast path.
        prefix = source.read_text().rsplit("void main() {", 1)[0]
        for alias in [False, True]:
            left, right = 16, 16 if alias else 23
            harness = work / ("alias.bfc" if alias else "scratch.bfc")
            harness.write_text(prefix + f"""
void main(){{
    output('@');output('B');output('F');output('C');output('R');
    output('L');output('E');output('2');output(';');
    cell p;
    p=emit_input(p,0);p=emit_loop_open(p,0);
    p=emit_input(p,{left});p=emit_input(p,{right});
    p=emit_constant(p,18,77);p=emit_constant(p,21,88);
    p=emit_constant(p,11,{0 if alias else 255});
    p=emit_constant(p,12,{0 if alias else 123});
    p=emit_constant(p,13,{0 if alias else 42});
    p=emit_constant(p,14,{0 if alias else 99});
    p=emit_frame_less(p,{left},{right});
    p=emit_move_to(p,{left});compiler_output!('.');
    p=emit_move_to(p,{right});compiler_output!('.');
    p=emit_move_to(p,18);compiler_output!('.');
    p=emit_move_to(p,21);compiler_output!('.');
    p=emit_move_to(p,11);compiler_output!('.');
    p=emit_move_to(p,12);compiler_output!('.');
    p=emit_move_to(p,13);compiler_output!('.');
    p=emit_move_to(p,14);compiler_output!('.');
    p=emit_input(p,0);p=emit_loop_close(p,0);
}}
""")
            generated = harness.with_suffix(".bf")
            generated.write_bytes(run([compiler, "--run-ir", "--no-ir-transitions", str(harness)]))
            for rows, flags in [(pairs, []), (selected, RLE_ONLY)]:
                expected = b"".join(bytes((0 if alias else a < b, 0, 77, 88, 0, 0, 0, 0))
                                    for a, b in rows)
                assert run([interpreter, "--no-progress", *flags, str(generated)], data(rows)) == expected
            print(f"{'Alias' if alias else 'Dirty scratch/separated operands'}: all pairs and RLE-only edges passed.", flush=True)


if __name__ == "__main__":
    main()
