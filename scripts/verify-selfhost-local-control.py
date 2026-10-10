#!/usr/bin/env python3
"""Check selfhost local control, continuation fallbacks and CIR compatibility."""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RLE_ONLY = ["--disable-clear", "--disable-scan", "--disable-transfer",
            "--disable-countdown", "--disable-compare", "--disable-remote-transfer"]
PROFILE = re.compile(rb"@BFCDBG2;|@ENDDBG;|@F[0-9a-fA-F]{4}:[0-9a-fA-F]*;|@C[0-9a-fA-F]{4};|@P[0-4];")


def expand(data):
    data = PROFILE.sub(b"", data)
    assert data.startswith(b"@BFCRLE2;")
    data = data[9:]
    result, end = bytearray(), 0
    for match in re.finditer(rb"([+<>-])([0-9a-fA-F]*)|([\[\].,])", data):
        assert match.start() == end
        end = match.end()
        count = int(match[2] or b"1", 16)
        assert count > 0 and len(result) + count <= 40_000_000
        result.extend((match[1] or match[3]) * count)
    assert end == len(data)
    return bytes(result)


def fixtures():
    values = [bytes([x]) for x in (0, 1, 2, 15, 127, 128, 254, 255)]
    # The last field requires byte-identical BF when local control is disabled.
    return [
        ("repeat", "void main(){cell n=input();while(n){output(n);n-=1;}}", values, False),
        ("noncanonical-byte-if", "void main(){cell n=input();if(n){output(n);}else{output(65);}output(n);if(n){if(n){output(n);}else{output(66);}}else{output(67);}cell after=n+1;output(after);}", [bytes([x]) for x in range(256)], False),
        ("noncanonical-byte-while", "void main(){cell n=input();cell count;while(n){count+=1;n=0;}output(count);output(n);}", [bytes([x]) for x in range(256)], False),
        ("noncanonical-reused-temp", "void main(){cell n=input();cell flag;if(n){flag=255;}else{flag=128;}while(flag){if(n){output(flag);}else{output(1);}flag=0;}if(n){output(2);}else{output(3);}output(n);}", [bytes([x]) for x in range(256)], False),
        ("nested", "void main(){cell n=input();cell total;while(n){cell j=3;while(j){if(j==2){total+=n;}else{total+=j;}j-=1;}n-=1;}output(total);}", values, False),
        ("branches", "void main(){cell again=input();while(again){cell n=input();if(n<128){if(n!=0){output(n+1);}else{output(17);}}else{output(n-1);}again=input();}}",
         [b"".join(bytes((1, x)) for x in range(256)) + b"\0"], False),
        ("input-condition", "void main(){while(input()){output(65);}output(input());}",
         [b"\0Z", b"\1\xff\2\0Z"], False),
        ("empty-loop", "void main(){while(input());output(65);}", [b"\0", b"\1\xff\0"], False),
        ("static-places", "struct S{cell x;cell[2] a;}S g;void main(){S s;cell n=input();while(n){g.x+=n;s.a[1]=g.x;output(s.a[1]);n-=1;}}", [b"\3"], False),
        ("fallback-call", "cell f(cell n){if(n){return n+1;}return 3;}void main(){cell n=input();while(n){output(f(n));n-=1;}}", [b"\3"], True),
        ("fallback-short-circuit", "void main(){cell n=input();while(n&&input()){output(n);n-=1;}output(input());}",
         [b"\0Z", b"\3\1\1\0Z"], True),
        ("fallback-portal", "cell[256] a;void main(){cell n=input();while(n){a[n]=n;output(a[n]);n-=1;}}", [b"\3"], True),
        ("fallback-return", "cell f(cell n){while(n){if(n==2){return n;}n-=1;}return 0;}void main(){output(f(input()));}",
         [b"\0", b"\1", b"\3"], True),
        ("fallback-abort", "void main(){cell n=input();while(n){output(n);abort();}output(65);}", [b"\0", b"\3"], True),
        ("fallback-aggregate", "void main(){cell n=input();while(n){cell[2] a=\"AB\";cell[2] b;b=a;output(b[1]);n-=1;}}", [b"\2"], True),
        ("slot-limit", "void main(){cell[237] p;cell n=input();while(n){output(n);n-=1;}output(p[236]);}", [b"\2"], True),
        ("else-slot-limit", "void main(){cell[237] p;cell n=input();if(n){p[0]=65;}else{p[0]=66;}output(p[0]);}", [b"\0", b"\1"], True),
        ("dispatch-pages", "void touch(){cell unused;}void main(){cell n=input();" + "if(n){touch();n-=1;}" * 140 + "output(n);}", [b"\xff"], True),
        ("goto-live-then", "void f(cell n){if(n){output(65);}else{return;}output(66);}void main(){f(input());}", values, True),
        ("goto-live-else", "void f(cell n){if(n){return;}else{output(65);}output(66);}void main(){f(input());}", values, True),
        ("goto-both-return", "cell f(cell n){if(n){return 65;}else{return 66;}}void main(){output(f(input()));}", values, True),
        ("goto-empty-else", "cell test(){return input();}void main(){if(test()){output(65);}else{}output(66);}", values, True),
        ("goto-empty-then", "cell test(){return input();}void main(){if(test()){}else{output(65);}output(66);}", values, True),
        ("goto-nonempty-join", "cell test(){return input();}void main(){if(test()){output(65);}else{output(66);}output(67);}", values, True),
        ("goto-nested-joins", "void touch(){cell unused;}void main(){cell n=input();if(n==1){touch();output(65);}else if(n==2){touch();output(66);}else if(n==3){touch();output(67);}else{touch();output(68);}output(69);}", values, True),
        ("goto-empty-while-entry", "cell test(){return input();}void main(){while(test()){output(65);}output(66);}", [b"\0", b"\3\2\1\0"], True),
        ("goto-nonempty-while-entry", "cell test(){return input();}void main(){output(66);while(test()){output(65);}output(67);}", [b"\0", b"\3\2\1\0"], True),
        ("goto-global-init-before-while", "cell g=input();cell test(){return input();}void main(){while(test()){output(g);}output(input());}", [b"A\0Z", b"A\1\2\0Z"], True),
        ("goto-while-after-join", "void touch(){cell unused;}cell test(){return input();}void main(){if(input()){touch();}while(test()){output(65);}output(66);}", [b"\0\0", b"\1\3\2\1\0"], True),
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    args = parser.parse_args()
    compiler, interpreter = str(args.compiler.resolve()), str(args.interpreter.resolve())
    (ROOT / "tmp").mkdir(exist_ok=True)

    def run(command, data=b""):
        result = subprocess.run(command, input=data, capture_output=True, timeout=180, cwd=ROOT)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        return result

    with tempfile.TemporaryDirectory(prefix="selfhost-local-control-", dir=ROOT / "tmp") as directory:
        work = Path(directory)
        source = work / "compiler.bfc"
        text = run([str(ROOT / "scripts/concat-stage2-compiler.sh"), "profile"]).stdout
        source.write_bytes(text)
        disabled = work / "disabled.bfc"
        assert text.count(b"const cell SELFHOST_LOCAL_CFG = 1;") == 1
        disabled.write_bytes(text.replace(b"const cell SELFHOST_LOCAL_CFG = 1;",
                                          b"const cell SELFHOST_LOCAL_CFG = 0;"))
        cir_source = work / "cir-compiler.bfc"
        cir_source.write_bytes(run([str(ROOT / "scripts/concat-stage2-compiler.sh"), "cir"]).stdout)
        compiler_bf = work / "compiler.bf"
        compiler_bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf",
                                     "--disable-function-inline", str(source)]).stdout)
        cir_compiler_bf = work / "cir-compiler.bf"
        cir_compiler_bf.write_bytes(run([compiler, "--unlimited-tape", "--compressed-bf",
                                         "--disable-function-inline", str(cir_source)]).stdout)
        executions = 0
        for name, program, inputs, fallback in fixtures():
            payload = program.encode()
            path = work / "program.bfc"
            path.write_bytes(payload)
            generated = run([interpreter, "--unlimited-tape", "--no-progress",
                             str(compiler_bf)], payload).stdout
            expected_bf = run([compiler, "--run-ir", "--disable-function-inline", str(source)], payload).stdout
            assert b"BFC_STAGE12_ERROR" not in generated, (name, generated[-100:])
            assert generated == expected_bf, (name, "BF and IR compiler outputs differ")
            artifact = work / "program.bf"
            artifact.write_bytes(expand(generated))
            disabled_bf = None
            if fallback or name == "repeat":
                disabled_bf = run([compiler, "--run-ir", "--disable-function-inline", str(disabled)], payload).stdout
                if fallback:
                    assert generated == disabled_bf, (name, "fallback changed BF")
            for data in inputs:
                expected = run([compiler, "--run-ir", str(path)], data).stdout
                logical = None
                for flags in (RLE_ONLY, []):
                    result = run([interpreter, "--unlimited-tape", "--no-progress", "--stats",
                                  *flags, str(artifact)], data)
                    assert result.stdout == expected, (name, data.hex())
                    stats = {key.decode(): int(value) for key, value in
                             re.findall(rb"^([a-z_]+)=(\d+)$", result.stderr, re.M)}
                    current = stats["executed_instructions"], stats["executed_rle_instructions"]
                    if logical is None:
                        logical = current
                    assert current == logical, (name, "logical counters differ")
                    executions += 1
                if name == "repeat" and data == b"\xff":
                    baseline = work / "baseline.bf"
                    baseline.write_bytes(expand(disabled_bf))
                    result = run([interpreter, "--unlimited-tape", "--no-progress", "--stats",
                                  *RLE_ONLY, str(baseline)], data)
                    assert result.stdout == expected
                    before = int(re.search(rb"^executed_rle_instructions=(\d+)$", result.stderr, re.M)[1])
                    assert logical[1] < before, ("local loop did not reduce dispatch cost", logical[1], before)
            # Import local control directly, with Rust CFG reconstruction off.
            cir = work / "program.cir"
            wire = run([compiler, "--run-ir", "--disable-function-inline", str(cir_source)], payload).stdout
            assert wire.startswith(b"BFCIR\0\x02\n"), (name, "missing v2 header")
            assert run([interpreter, "--unlimited-tape", "--no-progress",
                        str(cir_compiler_bf)], payload).stdout == wire, (name, "BF/IR CIR mismatch")
            cir.write_bytes(wire)
            cir_bf = work / "cir.bf"
            cir_bf.write_bytes(run([compiler, "--cir-input", str(cir), "--unlimited-tape",
                                   "--compressed-bf", "--disable-local-control-flow"]).stdout)
            for data in inputs:
                expected = run([compiler, "--run-ir", str(path)], data).stdout
                assert run([interpreter, "--unlimited-tape", "--no-progress",
                            str(cir_bf)], data).stdout == expected, (name, "CIR execution")
            print(f"{name}: BF/IR output, RLE-only/all-on semantics and fallback checks passed", flush=True)
        print(f"Local control: {executions} program executions and CIR compatibility passed.")


if __name__ == "__main__":
    main()
