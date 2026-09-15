#!/usr/bin/env python3
"""Check stage2 width boundaries with small inputs, never compiler self-input."""
import argparse
from pathlib import Path
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
    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="stage2-limits-", dir=ROOT / "tmp") as directory:
        work = Path(directory)

        def run(command, data=b""):
            result = subprocess.run(command, input=data, capture_output=True, timeout=120)
            assert result.returncode == 0, result.stderr.decode(errors="replace")
            return result.stdout

        def execute(bf):
            path = work / "program.bf"
            path.write_bytes(bf)
            return run([interpreter, "--unlimited-tape", "--no-progress", str(path)])

        sources = {}
        for entry in ("main", "compressed", "cir"):
            source = work / f"{entry}.bfc"
            source.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), entry]))
            sources[entry] = source

        # Calls and returns across both the function-ID and continuation-ID byte boundary.
        many = ("".join(f"cell f{i}(){{return {i % 256};}}" for i in range(300))
                + "void main(){output(f255());output(f256());output(f299());}").encode()
        expected = bytes((255, 0, 43))
        for entry, source in sources.items():
            result = run([compiler, "--run-ir", str(source)], many)
            assert b"BFC_STAGE12_ERROR" not in result, (entry, result[-100:])
            if entry == "cir":
                cir = work / "many.cir"
                cir.write_bytes(result)
                result = run([compiler, "--cir-input", str(cir), "--unlimited-tape",
                              "--compressed-bf"])
            assert execute(result) == expected, entry
            print(f"{entry}: 301 functions and dispatch across 255/256 passed.", flush=True)

        compiler_bf = work / "compiler.bf"
        compiler_bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf",
                                    str(sources["compressed"])]))
        bf_command = [interpreter, "--unlimited-tape", "--no-progress", str(compiler_bf)]
        assert execute(run(bf_command, many)) == expected
        print("BF compiler: 301-function input passed.", flush=True)

        frame_edge = b"void main(){cell[238] data;data[237]=65;output(data[237]);}"
        assert execute(run(bf_command, frame_edge)) == b"A"
        print("255-cell frame (16 header + 239 data/temporary cells) passed.", flush=True)

        invalid = [
            (b"cell[256] data;cell[256] copy(){return data;}void main(){}", b"ID"),
            (b"cell[256][256] data;cell[256][256] copy(){return data;}void main(){}", b"ID"),
            (b"cell[240] data;cell[240] copy(){return data;}void main(){}", b"ID"),
            (("void main(" + ",".join(f"cell p{i}" for i in range(256)) + "){}").encode(), b"GZ"),
            (b"void main(){cell[240] data;}", b"EN"),
        ]
        for data, code in invalid:
            for entry, source in sources.items():
                prefix = HEADER if entry == "compressed" else b""
                assert run([compiler, "--run-ir", str(source)], data) == (
                    prefix + b"BFC_STAGE12_ERROR:" + code + b"\n"), (entry, code)
            assert run(bf_command, data) == HEADER + b"BFC_STAGE12_ERROR:" + code + b"\n", code
        print("Return-size truncation, main parameters and frame limits rejected in IR/BF.", flush=True)

        prelude = sources["compressed"].read_text().split("void main() {")[0]
        helper = work / "helper.bfc"
        helper.write_text(prelude + """void main() {
            cell which = input();
            if (which == 0) {
                WideValue value; value.high = 1;
                value = wide_multiply_length(value, 0, 1);
            } else if (which == 1) {
                NodeId id; id.page = 255; id.slot = 255;
                id = next_continuation_dispatch_id(id);
            } else {
                NodeId id; id.bank = ARENA_LAST_BANK; id.page = ARENA_LAST_PAGE;
                id.slot = 1; id = arena_advance(id, 255);
            }
            output('X');
        }""")
        helper_bf = work / "helper.bf"
        helper_bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf", str(helper)]))
        for which, code in enumerate((b"IB", b"IC", b"BP")):
            expected_error = b"BFC_STAGE12_ERROR:" + code + b"\n"
            assert run([compiler, "--run-ir", str(helper)], bytes((which,))) == expected_error
            assert run([interpreter, "--unlimited-tape", "--no-progress", str(helper_bf)],
                       bytes((which,))) == expected_error
        print("24-bit multiply, 16-bit dispatch and arena overflow checks passed.", flush=True)

        # Generate arithmetic BF directly: no large array portal or self-input needed.
        helper.write_text(prelude + """void main() {
            reset_bf_optimizer(); emit_constant(20,0); emit_constant(21,0);
            emit_logical_offset_steps(20,21,0,255);
            emit_move_to(20); emit_bf_character('.');
            emit_move_to(21); emit_bf_character('.'); flush_bf_optimizer();
        }""")
        high_only = HEADER + run([compiler, "--run-ir", str(helper)])
        assert len(high_only) < 128, len(high_only)
        assert execute(high_only) == bytes((0, 255))
        offsets = [(0, 0), (255, 0), (0, 255), (255, 255), (250, 128)]
        amounts = [(0, 0), (1, 0), (255, 0), (0, 1), (0, 255), (255, 255)]
        body = "void main(){reset_bf_optimizer();"
        expected_offsets = bytearray()
        for low, high in offsets:
            for add_low, add_high in amounts:
                body += (f"emit_constant(20,{low});emit_constant(21,{high});"
                         f"emit_logical_offset_steps(20,21,{add_low},{add_high});"
                         "emit_move_to(20);emit_bf_character('.');"
                         "emit_move_to(21);emit_bf_character('.');")
                value = (low + 256 * high + add_low + 256 * add_high) % 65536
                expected_offsets.extend(value.to_bytes(2, "little"))
        body += "flush_bf_optimizer();}"
        helper.write_text(prelude + body)
        generated = HEADER + run([compiler, "--run-ir", str(helper)])
        assert execute(generated) == expected_offsets
        print("16-bit offset additions including carry and wrap passed.", flush=True)


if __name__ == "__main__":
    main()
