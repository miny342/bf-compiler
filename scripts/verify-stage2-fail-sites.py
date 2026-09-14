#!/usr/bin/env python3
"""Check unique stage2 fail IDs and short diagnostic paths; never self-input."""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
HEADER = b"@BFCRLE1;"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    args = parser.parse_args()
    compiler = str(args.compiler.resolve())
    interpreter = str(args.interpreter.resolve())
    source_dir = ROOT / "selfhost/stage2/compiler"
    sites = {}
    for path in sorted(source_dir.glob("*.bfc")):
        source = path.read_text()
        for match in re.finditer(r"\bfail\s*\(([^)]*)\)", source):
            location = f"{path.relative_to(ROOT)}:{source.count(chr(10), 0, match.start()) + 1}"
            if path.name == "01_common.bfc" and match[1] == "cell site_first, cell site_second":
                continue
            code = re.fullmatch(r"'([A-Z])', '([A-Z])'", match[1])
            assert code, f"fail needs two literal uppercase letters at {location}"
            key = code[1] + code[2]
            assert key not in sites, f"duplicate {key}: {sites.get(key)} and {location}"
            sites[key] = location
    assert sites
    print(f"{len(sites)} unique fail sites checked.", flush=True)

    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="stage2-fail-", dir=ROOT / "tmp") as directory:
        work = Path(directory)

        def run(command, data=b""):
            result = subprocess.run(command, input=data, capture_output=True, timeout=120)
            assert result.returncode == 0, result.stderr.decode(errors="replace")
            return result.stdout

        cases = [
            (b"/*", b"AB"),
            (b"void main(){cell value;if(value&1);}", b"AS"),
            (b"cell broken(cell value){if(value)return 1;}void main(){}", b"HB"),
        ]
        for entry in ("main", "compressed", "cir"):
            source = work / f"{entry}.bfc"
            source.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), entry]))
            prefix = HEADER if entry == "compressed" else b""
            bf = work / "compiler.bf"
            if entry == "compressed":
                bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf", str(source)]))
            for data, code in cases:
                expected = prefix + b"BFC_STAGE12_ERROR:" + code + b"\n"
                actual = run([compiler, "--run-ir", str(source)], data)
                assert actual == expected, (entry, code, actual)
                if entry == "compressed":
                    actual = run([interpreter, "--unlimited-tape", "--no-progress", str(bf)], data)
                    assert actual == expected, (entry, code, actual)
            print(f"{entry}: lexer and semantic diagnostics passed.", flush=True)

        # Exercise internal helper failures and abort without a large compilation.
        arena = (source_dir / "06_arena.bfc").read_text()
        helpers = arena[arena.index("struct WideValue {"):arena.index("WideValue wide_multiply_length(")]
        source = work / "internal.bfc"
        source.write_text((source_dir / "01_common.bfc").read_text() + helpers + """
            macro compiler_output(value) { output(value); }
            void main() {
                cell which = input();
                if (which == 0) { cell ignored = hex_digit_value('x'); }
                WideValue left;
                WideValue right;
                left.high = 255;
                if (which == 1) { right.high = 1; }
                else { left.low = 255; left.mid = 255; right.low = 1; }
                left = wide_add(left, right);
                output('X');
            }
        """)
        bf = work / "internal.bf"
        bf.write_bytes(run([compiler, "--compressed-bf", str(source)]))
        for which, code in enumerate((b"AA", b"BL", b"BM")):
            expected = b"BFC_STAGE12_ERROR:" + code + b"\n"
            assert run([compiler, "--run-ir", str(source)], bytes([which])) == expected
            assert run([interpreter, "--no-progress", str(bf)], bytes([which])) == expected
    print("Stage2 fail diagnostics and abort passed in IR and BF.")


if __name__ == "__main__":
    main()
