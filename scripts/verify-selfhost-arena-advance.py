#!/usr/bin/env python3
"""Check arena advance/read/write across all byte additions and bank boundaries."""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RLE_ONLY = ["--disable-clear", "--disable-scan", "--disable-transfer",
            "--disable-countdown", "--disable-compare", "--disable-remote-transfer"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    parser.add_argument("--selfhost-compiler", type=Path)
    args = parser.parse_args()
    compiler = str(args.compiler.resolve())
    interpreter = str(args.interpreter.resolve())
    arena = (ROOT / "selfhost/stage2/compiler/06_arena.bfc").read_text()
    last_bank = int(re.search(r"ARENA_LAST_BANK = (\d+)", arena)[1])
    last_page = int(re.search(r"ARENA_LAST_PAGE = (\d+)", arena)[1])
    start = arena.index("macro arena_advance_in_place(")
    end = arena.index("\ncell arena_cell_read(", start)
    program = (f"const cell ARENA_LAST_BANK={last_bank};"
               f"const cell ARENA_LAST_PAGE={last_page};"
               "struct NodeId{cell bank;cell page;cell slot;}"
               "void fail(cell a,cell b){output(250);output(a);output(b);abort();}"
               + arena[start:end] + """
// Trace the actual wrapper address without allocating the million-cell arena.
cell arena_cell_read(NodeId p){output(p.bank);output(p.page);return p.slot;}
void arena_cell_write(NodeId p,cell v){output(p.bank);output(p.page);output(p.slot);output(v);}
void main(){cell mode=input();while(mode){
    NodeId p;p.bank=input();p.page=input();p.slot=input();cell amount=input();
    if(mode==2){output(arena_read(p,amount));}
    else if(mode==3){arena_write(p,amount,177);}
    else {
        NodeId q=arena_advance(p,amount);
        output(q.bank);output(q.page);output(q.slot);
        output(p.bank);output(p.page);output(p.slot);
        output(arena_read(p,amount));arena_write(p,amount,177);
        output(p.bank);output(p.page);output(p.slot);
    }
    mode=input();
}}
""")
    span = (last_page + 1) * 256

    def fixture(cases):
        data, expected = bytearray(), bytearray()
        for bank, page, slot, amount in cases:
            total = bank * span + page * 256 + slot + amount
            result_bank, remainder = divmod(total, span)
            assert result_bank <= last_bank
            result_page, result_slot = divmod(remainder, 256)
            data.extend((1, bank, page, slot, amount))
            expected.extend((result_bank, result_page, result_slot, bank, page, slot,
                             result_bank, result_page, result_slot,
                             result_bank, result_page, result_slot, 177, bank, page, slot))
        data.append(0)
        return bytes(data), bytes(expected)

    cases = []
    for slot in range(256):
        for amount in range(256):
            bank = (slot + amount) % (last_bank + 1)
            page = (slot * 37 + amount * 13) % (last_page + 1)
            if bank == last_bank and page == last_page and slot + amount >= 256:
                bank -= 1
            cases.append((bank, page, slot, amount))
    cases += [(b, last_page, 255, 1) for b in range(last_bank)]
    cases += [(last_bank, last_page, 255, 0), (0, 0, 0, 255)]
    data, expected = fixture(cases)
    small_data, small_expected = fixture([(0, 0, 0, 0), (0, 0, 255, 1),
                                          (0, last_page, 255, 255),
                                          (last_bank, last_page, 255, 0)])

    def run(command, input_data=b""):
        result = subprocess.run(command, input=input_data, capture_output=True,
                                cwd=ROOT, check=True, timeout=180)
        return result.stdout

    (ROOT / "tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="selfhost-advance-", dir=ROOT / "tmp") as directory:
        work = Path(directory)
        source = work / "program.bfc"
        source.write_text(program)
        assert run([compiler, "--run-ir", str(source)], data) == expected
        variants = []
        for entry in ["profile", "cir"]:
            driver = work / (entry + "-compiler.bfc")
            driver.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), entry]))
            generated = run([compiler, "--run-ir", "--disable-function-inline", str(driver)],
                            program.encode())
            path = work / ("program.bf" if entry == "profile" else "program.cir")
            path.write_bytes(generated)
            if entry == "profile":
                variants.append(path)
                if args.selfhost_compiler:
                    assert generated == run([interpreter, "--unlimited-tape", "--no-progress",
                                             str(args.selfhost_compiler.resolve())], program.encode())
            else:
                assert run([compiler, "--cir-input", str(path), "--run-ir"], data) == expected
                for name, flags in [("default", []), ("triple", ["--enable-nibble-transfer",
                                     "--enable-inplace-compare", "--enable-anchor-bank"])]:
                    bf = work / (name + ".bf")
                    bf.write_bytes(run([compiler, "--cir-input", str(path), "--unlimited-tape",
                                        "--compressed-bf", *flags]))
                    variants.append(bf)
        for bf in variants:
            command = [interpreter, "--unlimited-tape", "--no-progress"]
            assert run([*command, str(bf)], data) == expected, bf.name
            assert run([*command, *RLE_ONLY, str(bf)], small_data) == small_expected, bf.name
            for slot, amount in [(255, 1), (128, 128)]:
                for mode in (1, 2, 3):
                    invalid = bytes((mode, last_bank, last_page, slot, amount, 0))
                    assert run([*command, str(bf)], invalid) == bytes((250, ord("B"), ord("P")))
            print(f"{bf.name}: all 65,536 additions, bank carries, source preservation, "
                  "read/write wrapper traces, overflow aborts and RLE-only edges passed.", flush=True)


if __name__ == "__main__":
    main()
