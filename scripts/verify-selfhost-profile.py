#!/usr/bin/env python3
"""Generate selfhost DBG2 BF through --run-ir, then check semantics and attribution."""
import argparse
from contextlib import nullcontext
import json
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
MARKERS = re.compile(rb"@BFCDBG2;|@ENDDBG;|@F[0-9a-fA-F]{4}:[0-9a-fA-F]+;|@C[0-9a-fA-F]{4};|@P[0-4];")


def run(command, data=b""):
    result = subprocess.run(command, input=data, capture_output=True, timeout=180)
    if result.returncode:
        raise RuntimeError(f"{command}: {result.stderr.decode(errors='replace')}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=Path)
    parser.add_argument("--interpreter", required=True, type=Path)
    parser.add_argument("--benchmark", action="store_true", help="compare sample overhead on a hot loop")
    parser.add_argument("--bf-bootstrap", action="store_true", help="compare both compilers running as BF on hello and functions")
    parser.add_argument("--output-dir", type=Path, help="keep artifacts in a new directory")
    args = parser.parse_args()
    compiler, interpreter = str(args.compiler.resolve()), str(args.interpreter.resolve())
    (ROOT / "tmp").mkdir(exist_ok=True)
    if args.output_dir:
        args.output_dir.mkdir(parents=True, exist_ok=False)
    directory_context = (nullcontext(str(args.output_dir)) if args.output_dir else
                         tempfile.TemporaryDirectory(prefix="selfhost-profile-", dir=ROOT / "tmp"))
    with directory_context as directory:
        work = Path(directory)
        sources = {}
        for entry in ("compressed", "profile"):
            source = run([str(ROOT / "scripts/concat-stage2-compiler.sh"), entry]).stdout
            path = work / f"{entry}.bfc"
            path.write_bytes(source)
            sources[entry] = path

        def generate(source):
            outputs, elapsed = {}, {}
            for entry in ("compressed", "profile"):
                start = time.perf_counter()
                # Compiler inlining dominates repeated CLI startup on this large source.
                # Disabling it affects the host IR runner, not the selfhost BF backend.
                outputs[entry] = run([compiler, "--run-ir", "--disable-function-inline", str(sources[entry])], source).stdout
                elapsed[entry] = time.perf_counter() - start
            assert outputs["profile"].startswith(b"@BFCRLE2;@BFCDBG2;")
            assert MARKERS.sub(b"", outputs["profile"]) == outputs["compressed"]
            return outputs, elapsed

        def execute(path, data=b"", mode=None):
            command = [interpreter, "--unlimited-tape", "--no-progress"]
            if mode:
                command += ["--accept-embedded-profile", "--profile-format", "json"]
                if mode != "default":
                    command += ["--profile-mode", mode]
            else:
                command += ["--stats", "--timings"]
            result = run(command + [str(path)], data)
            if mode:
                report = json.loads(result.stderr)
                path.with_suffix(f".{mode}.profile.json").write_text(json.dumps(report, indent=2) + "\n")
                assert report["version"] == 2
                assert report["artifact"]["hash_kind"] == "encoded_source"
                sites = report["profile"]["sites"]
                assert sum(s["static_bf_instructions"] for s in sites) == report["artifact"]["instruction_count"]
                root = next(s for s in sites if s["id"] == 0)
                assert root["inclusive_static_bf_instructions"] == report["artifact"]["instruction_count"]
                if mode == "default":
                    assert report["profile"]["sampling_interval_ns"] == 1000000
                return result.stdout, report
            return result.stdout, {key: int(value) for key, value in re.findall(rb"^(\w+)=(\d+)$", result.stderr, re.M)}

        def check(name, source, data=b"", expected=None):
            outputs, generation = generate(source)
            paths = {}
            for entry, bf in outputs.items():
                paths[entry] = work / f"{name}-{entry}.bf"
                paths[entry].write_bytes(bf)
            baseline_output, baseline = execute(paths["compressed"], data)
            if expected is not None:
                assert baseline_output == expected, name
            # Comments alone must not alter execution or optimization, even without opt-in.
            ignored_output, ignored = execute(paths["profile"], data)
            assert ignored_output == baseline_output
            for key in baseline:
                if not key.endswith(b"_ns"):
                    assert ignored[key] == baseline[key], (name, key)
            reports = {}
            for mode in ("counters", "default"):
                actual_output, report = execute(paths["profile"], data, mode)
                assert actual_output == baseline_output, (name, mode)
                stats = report["run_stats"]
                for key in ("executed_instructions", "executed_rle_instructions", "max_pointer"):
                    assert stats[key] == baseline[key.encode()], (name, mode, key)
                for key, value in stats["optimization"].items():
                    baseline_key = "native_operations" if key == "executed_native_operations" else key
                    if baseline_key.encode() in baseline:
                        assert value == baseline[baseline_key.encode()], (name, mode, key)
                reports[mode] = report
            sites = reports["counters"]["profile"]["sites"]
            assert any(s["kind"] == "function" and s["label"] == "main" for s in sites)
            print(f"{name}: {len(outputs['compressed'])} -> {len(outputs['profile'])} bytes; "
                  f"generate plain/profile {generation['compressed']:.3f}/{generation['profile']:.3f}s", flush=True)
            return paths, reports

        for example in sorted((ROOT / "selfhost/stage2/examples").glob("*.bfc")):
            # Keep every dynamic index in bounds when checking against Rust IR.
            data = (b"\x02\x01\x02\x02" if example.stem == "stage11_dynamic_projection"
                    else b"\x03\x02\x01")
            expected = run([compiler, "--run-ir", str(example)], data).stdout
            check(example.stem, example.read_bytes(), data, expected)

        page_source = (b"void main(){cell n=input();" + b"if(n){n-=1;}" * 140 + b"output(n);}")
        _, page_reports = check("dispatch-pages", page_source, b"\xff", bytes([115]))
        assert any(s["kind"] == "continuation" and int(s["attributes"]["continuation_id"]) >= 256
                   and s["counters"]["fast_operations"] > 0
                   for s in page_reports["counters"]["profile"]["sites"])

        # Balanced PCs must survive calls/returns across pages, recursion and
        # low-byte zero entries while keeping DBG2 function attribution valid.
        recursive_source = ("".join(
            f"cell f{i}(cell n){{if(n==0){{return {i % 256};}}"
            f"return f{(i + 1) % 300}(n-1);}}" for i in range(300))
            + "void main(){output(f0(255));output(f255(1));output(f299(2));}").encode()
        _, recursive_reports = check("dispatch-calls", recursive_source,
                                     expected=bytes((255, 0, 1)))
        recursive_sites = recursive_reports["counters"]["profile"]["sites"]
        executed_parents = {s["parent"] for s in recursive_sites
                            if s["kind"] == "continuation" and s["counters"]["fast_operations"] > 0}
        executed_functions = {s["label"] for s in recursive_sites
                              if s["kind"] == "function" and s["id"] in executed_parents}
        assert {"f0", "f255", "f256", "f299", "main"} <= executed_functions

        hot_source = b"""
cell cold(cell n) { while (n != 0) { n -= 1; } return n; }
cell hot(cell n) {
    cell total;
    while (n != 0) {
        cell j = 200;
        while (j != 0) {
            cell k = 10;
            while (k != 0) { total += k; k -= 1; }
            j -= 1;
        }
        n -= 1;
    }
    return total;
}
void main() { cell n = input(); if (n < 250) { output(hot(n)); } else { output(cold(n)); } }
"""
        paths, reports = check("hot-cold", hot_source, b"\x02")
        sites = reports["counters"]["profile"]["sites"]
        cold = next(s for s in sites if s["kind"] == "function" and s["label"] == "cold")
        parents = {s["id"]: s["parent"] for s in sites}
        for site in sites:
            ancestor = site["id"]
            while ancestor is not None and ancestor != cold["id"]:
                ancestor = parents[ancestor]
            if ancestor == cold["id"]:
                assert site["counters"]["fast_operations"] == 0, site
        for key in ("abi.dispatch.countdown", "abi.frame.compare", "abi.call", "abi.return"):
            assert any(s["stable_key"] == key and s["counters"]["fast_operations"] > 0 for s in sites), key

        if args.benchmark:
            times = {"plain": [], "ignored": [], "sample": []}
            for _ in range(5):
                for mode, path in [("plain", paths["compressed"]), ("sample", paths["profile"]), ("ignored", paths["profile"])]:
                    _, report = execute(path, b"\xc8", "default" if mode == "sample" else None)
                    elapsed = report["phase_timings_ns"]["execute"] if mode == "sample" else report[b"execute_ns"]
                    times[mode].append(elapsed / 1e9)
            medians = {k: statistics.median(v) for k, v in times.items()}
            print(f"execute medians (5 runs): {json.dumps(medians)}; "
                  f"sample overhead {(medians['sample'] / medians['plain'] - 1) * 100:.2f}%", flush=True)
            (work / "benchmark.json").write_text(json.dumps({"runs_seconds": times, "medians_seconds": medians}, indent=2) + "\n")

        if args.bf_bootstrap:
            bootstrap = {}
            for entry in ("compressed", "profile"):
                bf = run([compiler, "--unlimited-tape", "--compressed-bf", "--disable-function-inline", str(sources[entry])]).stdout
                path = work / f"{entry}-compiler.bf"
                path.write_bytes(bf)
                for example in ("hello", "stage5_functions"):
                    source = (ROOT / f"selfhost/stage2/examples/{example}.bfc").read_bytes()
                    elapsed = []
                    for _ in range(3):
                        generated, stats = execute(path, source)
                        assert generated == (work / f"{example}-{entry}.bf").read_bytes()
                        elapsed.append(stats[b"execute_ns"] / 1e9)
                    bootstrap[f"{entry}/{example}"] = {"runs_seconds": elapsed, "median_seconds": statistics.median(elapsed)}
            (work / "bootstrap.json").write_text(json.dumps(bootstrap, indent=2) + "\n")
            print("BF bootstrap matches --run-ir byte for byte (hello, functions)", flush=True)
            for example in ("hello", "stage5_functions"):
                plain = bootstrap[f"compressed/{example}"]["median_seconds"]
                profiled = bootstrap[f"profile/{example}"]["median_seconds"]
                print(f"BF generation {example}: {plain:.6f} -> {profiled:.6f}s ({(profiled / plain - 1) * 100:.2f}%)", flush=True)


if __name__ == "__main__":
    main()
