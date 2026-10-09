#!/usr/bin/env python3
"""Check selfhost zero-low offset transfers and carry-preserving fallbacks."""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RLE_ONLY = ["--disable-clear", "--disable-scan", "--disable-transfer",
            "--disable-countdown", "--disable-compare", "--disable-remote-transfer"]

PROGRAM = """
cell before;
cell after;
cell[2][4][256] nested;
cell[3][129] partial;
struct Tail { cell prefix; cell[256] bytes; }
Tail[16] tails;
cell calls;
cell[255][256] arena;
cell index_value(cell i) { calls += 1; return i; }
void main() {
    cell[32] local;
    before = 91; after = 92;
    while (input()) {
        cell p = input(); cell i = input(); cell v = input();
        cell r = input(); cell c = input(); cell l = input();
        // The offset low stays zero through page*256, including across a call.
        arena[p][index_value(i)] = v;
        output(arena[p][i]);
        arena[p][i] += 17; output(arena[p][i]);
        arena[p][i] -= 39; output(arena[p][i]);
        // Two preceding whole-page strides must keep the zero fact.
        nested[r][c][i] = v; output(nested[r][c][i]);
        // Stride 129 makes low unknown; adding i may carry and must fall back.
        cell j = i; if (j > 128) { j = 128; }
        partial[2][j] = v; output(partial[2][j]);
        // Stride 257 and a nonzero constant field offset also require carry.
        tails[c].prefix = 83;
        tails[c].bytes[i] = v; output(tails[c].bytes[i]);
        output(tails[c].prefix);
        // A one-dimensional local array uses the same lowering proof.
        local[l] = v; output(local[l]);
        output(p); output(i); output(v); output(before); output(after);
    }
    output(calls);
}
"""


def case_data(cases):
    data, expected = bytearray(), bytearray()
    for p, i in cases:
        v = (p * 19 + i * 7) & 255
        data.extend((1, p, i, v, p & 1, p & 3, i & 31))
        expected.extend((v if p < 255 else 0,
                         (v + 17) & 255 if p < 255 else 0,
                         (v - 22) & 255 if p < 255 else 0,
                         v, v, v, 83, v, p, i, v, 91, 92))
    data.append(0)
    expected.append(len(cases) & 255)
    return bytes(data), bytes(expected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    parser.add_argument("--selfhost-compiler", type=Path,
                        help="also check byte-identical generation by this stage2 BF")
    parser.add_argument("--enable-nibble-transfer", action="store_true")
    args = parser.parse_args()
    compiler, interpreter = str(args.compiler.resolve()), str(args.interpreter.resolve())
    flags = ["--enable-nibble-transfer"] if args.enable_nibble_transfer else []

    def run(command, data=b""):
        result = subprocess.run(command, input=data, capture_output=True, timeout=300, cwd=ROOT)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        return result.stdout

    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="selfhost-offsets-", dir=ROOT / "tmp") as directory:
        work = Path(directory)
        program = work / "program.bfc"
        program.write_text(PROGRAM)
        cases = [(0, i) for i in range(256)] + [(p, 255) for p in range(255)]
        cases += [(p, i) for p in (1, 15, 16, 127, 128, 254)
                  for i in (0, 1, 15, 16, 127, 128, 254, 255)]
        data, expected = case_data(cases)
        small_data, small_expected = case_data([(0, 0), (1, 3), (254, 255)])
        # Selfhost BF returns zero/ignores writes outside each aggregate. The
        # IR VM errors; public CIR instead exposes one padded global region,
        # so per-array invalid indices are checked only on selfhost BF here.
        bounds_data, bounds_expected = case_data([(255, 1)])
        assert run([compiler, "--run-ir", str(program)], data) == expected
        bf = work / "program.bf"
        cir = work / "program.cir"
        for entry, output in [("profile", bf), ("cir", cir)]:
            source = work / (entry + "-compiler.bfc")
            source.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), entry, *flags]))
            generated = run([compiler, "--run-ir", "--no-ir-transitions", str(source)],
                            PROGRAM.encode())
            assert generated.startswith(b"@BFCRLE2;@BFCDBG2;" if entry == "profile" else b"BFCIR\0\x02\n")
            output.write_bytes(generated)
            if entry == "profile" and args.selfhost_compiler:
                bootstrapped = run([interpreter, "--unlimited-tape", "--no-progress",
                                    str(args.selfhost_compiler.resolve())], PROGRAM.encode())
                assert bootstrapped == generated, "BF/IR selfhost compiler mismatch"
            print(f"{entry}: generated", flush=True)
        assert run([compiler, "--cir-input", str(cir), "--run-ir"], data) == expected
        variants = [bf]
        for name, codegen in [("default", []), ("triple", ["--enable-nibble-transfer",
                             "--enable-inplace-compare", "--enable-anchor-bank"])]:
            generated = work / ("cir-" + name + ".bf")
            generated.write_bytes(run([compiler, "--cir-input", str(cir), "--unlimited-tape",
                                       "--compressed-bf", *codegen]))
            variants.append(generated)
        for artifact in variants:
            command = [interpreter, "--unlimited-tape", "--no-progress"]
            assert run([*command, str(artifact)], data) == expected, artifact.name
            if artifact == bf:
                assert run([*command, str(artifact)], bounds_data) == bounds_expected
            # A few records suffice to exercise all proof/fallback paths under
            # actual RLE-only execution without timing a large slow bootstrap.
            assert run([*command, *RLE_ONLY, str(artifact)], small_data) == small_expected
            print(f"{artifact.name}: {len(cases)} all-on records and 3 RLE-only records matched", flush=True)
        print("Offsets: page boundaries, all byte indices, bounds, nested strides, field offsets, "
              "local arrays, calls, and input preservation passed.", flush=True)


if __name__ == "__main__":
    main()
