#!/usr/bin/env python3
"""Exercise the stage2 streaming BF optimizer in both IR and generated BF."""
import argparse
from pathlib import Path
import random
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
HEADER = b"@BFCRLE1;"


def expand(data):
    if not data.startswith(HEADER):
        return data
    return re.sub(rb"([+<>-])([0-9]+)",
                  lambda m: m[1] * int(m[2]), data[len(HEADER):])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    args = parser.parse_args()
    compiler = str(args.compiler.resolve())
    interpreter = str(args.interpreter.resolve())
    source_dir = ROOT / "selfhost/stage2/compiler"
    # Use the production arithmetic helpers without the unrelated AST arena.
    arena = (source_dir / "06_arena.bfc").read_text()
    helpers = arena[arena.index("struct WideValue {"):arena.index("WideValue wide_multiply_length(")]
    common = helpers + (source_dir / "09_bf_optimizer.bfc").read_text()
    common += (source_dir / "09_bf_serialization.bfc").read_text()
    common += """
        macro compiler_output(value) { output(value); }
        void fail(cell site_first, cell site_second) { output('E'); abort(); }
        void main() {
            if (COMPRESSED_BF_OUTPUT) {
                output('@'); output('B'); output('F'); output('C'); output('R');
                output('L'); output('E'); output('1'); output(';');
            }
            cell character = input();
            while (character != 0) {
                if (character == '!') {
                    flush_bf_optimizer(); output('!'); reset_bf_optimizer();
                } else if (character == 'R') {
                    cell command = input();
                    WideValue count;
                    count.low = input(); count.mid = input(); count.high = input();
                    emit_repeat_wide(command, count);
                } else {
                    emit_bf_character(character);
                }
                character = input();
            }
            flush_bf_optimizer();
        }
    """
    cases = [
        (b">>><<", b">"), (b"+><-", b""), (b">+-<", b""),
        (b"+++[-]++[+]", b"[-]"), (b"[-]+++[-],", b","),
        (b"++.,.", b"++.,."), (b"[>+<-]", b"[>+<-]"),
        (b"[+>><<-]", b"[]"), (b"[>+<-[-]]", b"[>+<[-]]"),
        (b"[->+<]>.", b"[->+<]>."),
        (b"[" * 40 + b"-" + b"]" * 40, b"[" * 40 + b"-" + b"]" * 40),
        (b">+" * 40 + b"-<" * 40, None),  # Suffix eviction and ring wraparound.
        (b"R>\x00\x00\x01R<\xff\xff\x00", b">"),
        (b"R+\x01\xff\xffR-\x00\x01\x00", b"+"),
        (b"R>\xff\xff\xffR>\x01\x00\x00R<\x01\x00\x00", None),
    ]
    for value in range(256):
        addition = b"+" * value if value <= 128 else b"-" * (256 - value)
        cases.append((b"+" * value, addition))
        cases.append((b"[" + b"+" * value + b"]",
                      b"[-]" if value % 2 else b"[" + addition + b"]"))
    rng = random.Random(42)
    programs = []
    for _ in range(50):
        program = b""
        for _ in range(40):
            program += rng.choice([b"+" * rng.randrange(256), b">+<", b">><<",
                                   b"[-]", b"[---]", b"[->+<]", b",", b"."])
        programs.append(program + b".")
    cases.extend((program, None) for program in programs)
    payload = b"!".join(case[0] for case in cases) + b"!"

    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="stage2-bf-opt-", dir=ROOT / "tmp") as directory:
        work = Path(directory)

        def run(command, data=b""):
            result = subprocess.run(command, input=data, capture_output=True, timeout=120)
            assert result.returncode == 0, result.stderr.decode(errors="replace")
            return result.stdout

        outputs = []
        for compressed in (0, 1):
            source = work / f"optimizer-{compressed}.bfc"
            source.write_text(f"const cell COMPRESSED_BF_OUTPUT = {compressed};\n" + common)
            result = run([compiler, "--run-ir", str(source)], payload)
            bf = work / f"optimizer-{compressed}.bf"
            bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf", str(source)]))
            assert run([interpreter, "--unlimited-tape", "--no-progress", str(bf)], payload) == result
            # The overflow case is intentionally kept compressed while checking it.
            parts = result.removeprefix(HEADER).split(b"!")
            assert parts[-1] == b"" and len(parts) == len(cases) + 1
            if compressed:
                assert parts[14] == b">16777215", parts[14]
            outputs.append([expand(HEADER + part) if compressed else part for part in parts[:-1]])
        assert outputs[0] == outputs[1]
        for (_, expected), actual in zip(cases, outputs[0]):
            if expected is not None:
                assert actual == expected, (expected, actual)
        # Termination, I/O (including EOF), transfers, and nested cell updates.
        for number, (original, optimized) in enumerate(zip(programs, outputs[0][-len(programs):])):
            before = work / "before.bf"
            after = work / "after.bf"
            before.write_bytes(original)
            after.write_bytes(optimized)
            data = bytes(rng.randrange(256) for _ in range(number % 20))
            command = [interpreter, "--no-progress"]
            assert run(command + [str(before)], data) == run(command + [str(after)], data)
    print(f"{len(cases)} optimizer cases passed in IR and BF, plain and compressed; 50 semantic comparisons passed.")


if __name__ == "__main__":
    main()
