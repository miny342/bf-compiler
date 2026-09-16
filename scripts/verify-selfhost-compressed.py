#!/usr/bin/env python3
"""Check reversible selfhost BF encoding without expanding long runs on disk."""
import argparse
import itertools
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
HEADER = b"@BFCRLE1;"


def runs(data):
    compressed = data.startswith(HEADER)
    if compressed:
        data = data[len(HEADER):]
    pattern = rb"([+<>-])([0-9]*)|([\[\].,])" if compressed else rb"([+<>-])()|([\[\].,])"
    end = 0
    for match in re.finditer(pattern, data):
        assert match.start() == end, (end, data[end:end + 30])
        end = match.end()
        command = match[1] or match[3]
        count = int(match[2] or b"1")
        assert count > 0
        yield command, count
    assert end == len(data)


def normalized(data):
    return [(command, sum(count for _, count in group))
            for command, group in itertools.groupby(runs(data), key=lambda item: item[0])]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    args = parser.parse_args()
    compiler = str(args.compiler.resolve())
    interpreter = str(args.interpreter.resolve())
    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="selfhost-rle-", dir=ROOT / "tmp") as directory:
        work = Path(directory)

        def run(command, data=b""):
            result = subprocess.run(command, input=data, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, check=True)
            return result.stdout

        sources = {}
        for entry in ("main", "compressed"):
            source = run([str(ROOT / "scripts/concat-stage2-compiler.sh"), entry])
            path = work / f"{entry}.bfc"
            path.write_bytes(source)
            sources[entry] = path

        # Execute the compressed compiler as BF, not just through the Rust IR VM.
        compiler_bf = work / "compiler.bf"
        compiler_bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf",
                                     str(sources["compressed"])]))
        for example in sorted((ROOT / "selfhost/stage2/examples").glob("*.bfc")):
            plain = run([compiler, "--run-ir", str(sources["main"])], example.read_bytes())
            compressed = run([interpreter, "--unlimited-tape", "--no-progress",
                              str(compiler_bf)], example.read_bytes())
            assert compressed.startswith(HEADER), example
            assert normalized(plain) == normalized(compressed), example
            print(f"{example.name}: {len(plain)} -> {len(compressed)} bytes", flush=True)

        # Test the immediate serializer directly, including exact Add run counts.
        # All cell counts, decimal boundaries, 256/65536 and maximum 24-bit count.
        harness = sources["compressed"].read_text().split("void main() {")[0]
        harness += """void main() {
            output('@'); output('B'); output('F'); output('C'); output('R');
            output('L'); output('E'); output('1'); output(';');
            cell n;
            emit_repeat_character('+', n); output('.');
            n = 1;
            while (n != 0) {
                emit_repeat_character('+', n); output('.');
                emit_repeat_character('-', n); output('.');
                emit_repeat_character('>', n); output('.');
                emit_repeat_character('<', n); output('.');
                n += 1;
            }
            emit_repeat_256('>'); output('.');
            emit_repeat_65536('<'); output('.');
            WideValue count;
            count.low = 255; count.mid = 255; count.high = 255;
            emit_repeat_wide('>', count); output('.');
        }
        """
        path = work / "counts.bfc"
        boundaries = (0, 1, 9, 10, 99, 100, 255, 256, 999, 1000,
                      65535, 65536, 99999, 100000, 999999, 1000000,
                      9999999, 10000000)
        extra = ""
        for count in boundaries:
            extra += (f"count.low={count % 256}; count.mid={(count // 256) % 256}; "
                      f"count.high={count // 65536}; emit_repeat_wide('<',count); output('.');")
        harness = harness.rsplit("}", 1)[0] + extra + "}"
        path.write_text(harness)
        data = run([compiler, "--run-ir", str(path)])
        counts_bf = work / "counts.bf"
        counts_bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf", str(path)]))
        assert run([interpreter, "--unlimited-tape", "--no-progress", str(counts_bf)]) == data
        expected = [(b".", 1)]
        for count in range(1, 256):
            for command in (b"+", b"-", b">", b"<"):
                expected.extend([(command, count), (b".", 1)])
        for command, count in ((b">", 256), (b"<", 65536), (b">", 16777215)):
            expected.extend([(command, count), (b".", 1)])
        for count in boundaries:
            if count:
                expected.extend([(b"<", count), (b".", 1)])
            else:
                expected[-1] = (b".", expected[-1][1] + 1)
        assert normalized(data) == expected
        print("All repeat counts and expanded example instruction streams match.")


if __name__ == "__main__":
    main()
