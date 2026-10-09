//! Source-only preallocation expansion keeps portal and recursive resumes shared.
use super::*;

fn verify(name: &str, source: &str, cases: &[(&[u8], &[u8])]) {
    let root = test_root().with_file_name(format!("closed-inline-{name}-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.bfc"), source).unwrap();
    for dynamic in [false, true] {
        for triple in [false, true] {
            for structured in [false, true] {
                let compile = |mode: &str, weight: &str, single: bool, prefix: bool| {
                    let mut command = Command::new(env!("CARGO_BIN_EXE_bfc"));
                    command
                        .current_dir(&root)
                        .env("BFC_EVAL_CLOSED_INLINE", mode)
                        .env("BFC_EVAL_CLOSED_INLINE_WEIGHT", weight)
                        .env("BFC_EVAL_DIRECT_REGION", "0")
                        .env("BFC_EVAL_SINGLE_TERMINAL", if single { "1" } else { "0" })
                        .env("BFC_EVAL_ENTRY_PREFIX", if prefix { "1" } else { "0" })
                        .env("BFC_EVAL_ENTRY_PREFIX_ARGUMENTS", "8")
                        .args([
                            "--disable-function-inline",
                            "--unlimited-tape",
                            "--compressed-bf",
                            "--profile-granularity",
                            "abi",
                            "--profile-map-output",
                            "map.json",
                        ]);
                    if dynamic {
                        command.arg("--disable-static-frames");
                    }
                    if triple {
                        command.args([
                            "--enable-nibble-transfer",
                            "--enable-inplace-compare",
                            "--enable-anchor-bank",
                        ]);
                    }
                    if !structured {
                        command.arg("--disable-local-control-flow");
                    }
                    let output = command.arg("main.bfc").output().unwrap();
                    assert!(output.status.success(), "{name}: {:?}", output.stderr);
                    let map = bf_profiling::ProfileMap::from_json(
                        &fs::read_to_string(root.join("map.json")).unwrap(),
                    )
                    .unwrap();
                    map.validate_for_source(&output.stdout).unwrap();
                    let entries = map
                        .sites
                        .iter()
                        .filter(|s| s.stable_key.starts_with("abi.dispatch.case."))
                        .count();
                    (output.stdout, entries)
                };
                let (baseline, entries) = compile("0", "128", false, false);
                let mut variants = Vec::new();
                for mode in ["1", "2", "3", "4"] {
                    for weight in ["0", "128"] {
                        variants.push((mode, weight, false, false));
                    }
                }
                variants.extend([
                    ("3", "128", true, false),
                    ("3", "128", false, true),
                    ("4", "128", true, true),
                ]);
                for (mode, weight, single, prefix) in variants {
                    let (bf, new_entries) = compile(mode, weight, single, prefix);
                    assert!(
                        new_entries <= entries,
                        "{name}: dispatch targets grew {entries}->{new_entries}"
                    );
                    if weight == "0" {
                        assert_eq!(bf, baseline, "{name}: disabled expansion changed BF");
                    }
                    for &(input, expected) in cases {
                        let mut counts = None;
                        for rle_only in [false, true] {
                            let result = bf_interpreter::run_with_options(
                                &bf,
                                input,
                                bf_interpreter::RunOptions {
                                    collect_stats: true,
                                    disable_clear: rle_only,
                                    disable_scan: rle_only,
                                    disable_transfer: rle_only,
                                    disable_countdown: rle_only,
                                    disable_remote_transfer: rle_only,
                                    disable_compare: rle_only,
                                    ..Default::default()
                                },
                            )
                            .unwrap();
                            assert_eq!(
                                result.output, expected,
                                "{name}: mode={mode}, weight={weight}, dynamic={dynamic}, triple={triple}, structured={structured}, input={input:?}, RLE-only={rle_only}"
                            );
                            let current = (
                                result.stats.executed_instructions,
                                result.stats.executed_rle_instructions,
                            );
                            if let Some(previous) = counts {
                                assert_eq!(
                                    current, previous,
                                    "{name}: native/RLE counter mismatch"
                                );
                            }
                            counts = Some(current);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn closed_inline_keeps_global_snapshots_and_live_arguments() {
    verify(
        "snapshot",
        "cell g; cell f(cell a,cell b) { cell tmp=a; a+=1; g=9; return tmp+a+b; } void main() { g=input(); cell n=input(); output(f(g,n)); output(g); output(n); }",
        &[(&[0, 3], &[4, 9, 3]), (&[255, 2], &[1, 9, 2])],
    );
}

#[test]
fn closed_inline_reinitializes_loop_activations() {
    verify(
        "loop",
        "cell step(cell x) { cell saved; while(x) { saved+=1; x-=1; } return saved; } void main() { cell a=input(); output(step(a)); output(step(2)); output(a); }",
        &[(&[0], &[0, 2, 0]), (&[3], &[3, 2, 3])],
    );
}

#[test]
fn closed_inline_preserves_aggregate_returns_and_portals() {
    verify(
        "aggregate",
        "struct Pair { cell a; cell b; } Pair leaf(cell x) { Pair p; p.a=x; p.b=x+1; return p; } Pair[2] arena; void main() { cell x=input(); cell p=input(); arena[p]=leaf(x); Pair q=arena[p]; output(q.a); output(q.b); output(x); }",
        &[(&[0, 0], &[0, 1, 0]), (&[255, 1], &[255, 0, 255])],
    );
}

#[test]
fn closed_inline_keeps_recursive_boundaries() {
    verify(
        "recursive",
        "cell leaf(cell x) { return x+1; } cell rec(cell n) { if(n) { return leaf(rec(n-1)); } return leaf(0); } void main() { output(rec(input())); }",
        &[(&[0], &[1]), (&[3], &[4])],
    );
}

#[test]
fn closed_inline_preserves_abort_and_input_order() {
    verify(
        "abort",
        "void stop(cell n) { if(n) { output(input()); abort(); } output(65); } void main() { stop(input()); output(input()); }",
        &[(&[0, 9], &[65, 9]), (&[1, 7, 9], &[7])],
    );
}

#[test]
fn closed_inline_forwards_portal_wrapper_results() {
    verify(
        "forward",
        "cell[256] arena; cell read(cell p) { return arena[p]; } cell wrapper(cell p) { return read(p+1); } void main() { cell p=input(); arena[p+1]=input(); output(wrapper(p)); output(p); }",
        &[(&[0, 3], &[3, 0]), (&[255, 255], &[255, 255])],
    );
}

#[test]
fn closed_inline_collapses_tail_recursive_cycle() {
    verify(
        "cycle",
        "cell a(cell n) { output(65); if(n) { return b(n-1); } return 7; } cell b(cell n) { output(66); if(n) { return a(n-1); } return 7; } void main() { output(a(input())); }",
        &[(&[0], &[65, 7]), (&[3], &[65, 66, 65, 66, 7])],
    );
}

#[test]
fn closed_inline_forwards_void_portal_wrapper() {
    verify(
        "void-forward",
        "cell[256] arena; void write(cell p,cell value) { arena[p]=value; } void wrapper(cell p,cell value) { write(p+1,value); } void main() { cell p=input(); wrapper(p,input()); output(arena[p+1]); output(p); }",
        &[(&[0, 3], &[3, 0]), (&[255, 255], &[255, 255])],
    );
}

#[test]
fn closed_inline_preserves_post_call_global_write() {
    verify(
        "post-global",
        "cell[256] arena; cell g; cell read(cell p) { return arena[p]; } cell wrapper(cell p) { cell value=read(p); g=7; return value; } void main() { arena[0]=input(); output(wrapper(0)); output(g); }",
        &[(&[0], &[0, 7]), (&[255], &[255, 7])],
    );
}

#[test]
fn entry_prefix_cancels_argument_arithmetic_at_byte_boundaries() {
    verify(
        "prefix-cancel",
        "cell[256] arena; cell read(cell p) { return arena[p+1]; } void main() { cell p=input(); arena[p]=input(); output(read(p-1)); output(p); }",
        &[(&[0, 7], &[7, 0]), (&[255, 255], &[255, 255])],
    );
}

#[test]
fn entry_prefix_reinitializes_each_hoisted_activation() {
    verify(
        "prefix-zero",
        "cell[256] arena; cell f(cell p) { cell t; t+=1; return arena[p]+t; } void main() { cell p=input(); arena[p]=input(); output(f(p)); output(f(p)); }",
        &[(&[0, 7], &[8, 8]), (&[255, 255], &[0, 0])],
    );
}

#[test]
fn non_tail_wrapper_keeps_recursive_suffix_values() {
    verify(
        "non-tail-cycle",
        "cell a(cell n) { if(n) { return b(n-1)+1; } return 0; } cell b(cell n) { if(n) { return a(n-1)+1; } return 0; } void main() { output(a(input())); }",
        &[(&[0], &[0]), (&[3], &[3])],
    );
}
