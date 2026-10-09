//! Experimental call regions must preserve activation state and shared IDs.
use super::*;
use std::collections::BTreeSet;

fn verify_case(name: &str, source: &str, cases: &[(u8, &[u8])]) {
    let root = test_root().with_file_name(format!("direct-regions-{name}-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.bfc"), source).unwrap();
    for binding_mode in ["1", "2"] {
        for dynamic in [false, true] {
            for triple in [false, true] {
                let compile =
                    |depth: &str, code_limit: &str, direct_return: &str, resume_limit: &str| {
                        let mut command = Command::new(env!("CARGO_BIN_EXE_bfc"));
                        command
                            .current_dir(&root)
                            .env("BFC_EVAL_DIRECT_REGION", depth)
                            .env("BFC_EVAL_DIRECT_DYNAMIC", "1")
                            .env("BFC_EVAL_DIRECT_RETURN", direct_return)
                            .env("BFC_EVAL_DIRECT_RAW_LIMIT", "1048576")
                            .env("BFC_EVAL_DIRECT_CODE_LIMIT", code_limit)
                            .env("BFC_EVAL_DIRECT_RESUME_CODE_LIMIT", resume_limit)
                            .env("BFC_EVAL_DIRECT_BINDINGS", binding_mode)
                            .args([
                                "--disable-function-inline",
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
                        let output = command.arg("main.bfc").output().unwrap();
                        assert!(output.status.success(), "{:?}", output.stderr);
                        let map = bf_profiling::ProfileMap::from_json(
                            &fs::read_to_string(root.join("map.json")).unwrap(),
                        )
                        .unwrap();
                        map.validate_for_source(&output.stdout).unwrap();
                        let dispatch: BTreeSet<_> = map
                            .sites
                            .iter()
                            .filter(|s| s.stable_key.starts_with("abi.dispatch.case."))
                            .map(|s| s.stable_key.clone())
                            .collect();
                        // --cir-output selects CIR output instead of BF generation.
                        let cir_output =
                            command.args(["--cir-output", "cir.json"]).output().unwrap();
                        assert!(cir_output.status.success(), "{:?}", cir_output.stderr);
                        (
                            output.stdout,
                            dispatch,
                            fs::read(root.join("cir.json")).unwrap(),
                        )
                    };
                let (baseline, dispatch, cir) = compile("0", "512", "0", "512");
                for code_limit in ["0", "512", "65536"] {
                    for direct_return in ["0", "1"] {
                        for resume_limit in ["0", "512", "65536"] {
                            let (bf, candidate_dispatch, candidate_cir) =
                                compile("2", code_limit, direct_return, resume_limit);
                            assert_eq!(candidate_dispatch, dispatch, "{name}: dispatch targets");
                            assert_eq!(candidate_cir, cir, "{name}: CIR changed");
                            if code_limit == "0" {
                                assert_eq!(bf, baseline, "{name}: rejected trial changed BF");
                            } else if code_limit == "65536" {
                                assert_ne!(bf, baseline, "{name}: test did not exercise expansion");
                            }
                            for &(input, expected) in cases {
                                let mut native_counts = None;
                                for rle_only in [false, true] {
                                    let result = bf_interpreter::run_with_options(
                                        &bf,
                                        &[input],
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
                                        "{name}: dynamic={dynamic}, triple={triple}, cap={code_limit}, input={input}, RLE-only={rle_only}"
                                    );
                                    let counts = (
                                        result.stats.executed_instructions,
                                        result.stats.executed_rle_instructions,
                                    );
                                    if let Some(expected) = native_counts {
                                        assert_eq!(
                                            counts, expected,
                                            "{name}: native/RLE counter mismatch"
                                        );
                                    } else {
                                        native_counts = Some(counts);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn direct_regions_preserve_recursive_scalar_activations() {
    verify_case(
        "recursive",
        "cell g; cell leaf(cell x){g+=1;return x+g;} cell f(cell n){if(n==0){return leaf(3);} cell keep=leaf(n);return f(n-1)+keep;} void main(){output(f(input()));output(g);}",
        &[(0, &[4, 1]), (3, &[19, 4])],
    );
}

#[test]
fn direct_regions_preserve_recursive_aggregate_returns() {
    verify_case(
        "aggregate",
        "struct P{cell a;cell b;cell c;} cell g; P leaf(cell x){P p;p.a=x+g;p.b=x+1;p.c=x+2;if(x){return p;}p.a=9;return p;} P rec(cell n){if(n==0){return leaf(3);}P saved=leaf(n);P q=rec(n-1);q.a+=saved.b;return q;} void main(){g=1;P p=rec(input());output(p.a);output(p.b);output(p.c);}",
        &[(0, &[4, 4, 5]), (3, &[13, 4, 5])],
    );
}

#[test]
fn direct_regions_preserve_portal_yields_and_alternate_return_contexts() {
    verify_case(
        "portal",
        "cell[4] a; cell g;cell f(cell n){if(n){return a[n];}g+=1;return g;}void main(){a[1]=65;a[3]=67;cell n=input();output(f(n));output(f(0));output(f(n));}",
        &[(1, &[65, 1, 65]), (3, &[67, 1, 67])],
    );
}

#[test]
fn direct_regions_preserve_abort_without_resuming_the_caller() {
    verify_case(
        "abort",
        "cell g;cell f(cell n){g+=1;if(n){return g;}abort();return 0;}void main(){output(f(input()));output(99);}",
        &[(0, &[]), (1, &[1, 99])],
    );
}

#[test]
fn direct_regions_deliver_fixed_scalar_and_aggregate_results_once() {
    verify_case(
        "fixed-results",
        "struct P{cell a;cell b;cell c;} cell g; P make(cell x){g+=1;P p;p.a=x+g;p.b=x+1;p.c=x+2;return p;} cell read(){return g;} void zero(){g+=1;} void main(){g=1;P p=make(input());output(read());zero();P q=make(0);output(p.a);output(p.b);output(p.c);output(q.a);output(q.b);output(q.c);output(read());}",
        &[
            (0, &[2, 2, 1, 2, 4, 1, 2, 4]),
            (255, &[2, 1, 0, 1, 4, 1, 2, 4]),
        ],
    );
}

#[test]
fn direct_bindings_preserve_live_duplicate_arguments() {
    verify_case(
        "live-duplicate",
        "cell g;cell echo(cell x){output(x);g+=1;return x;}cell twice(cell a,cell b){g+=1;output(a);return b;}void main(){g=1;cell x=input();output(echo(x));output(x);output(twice(x,x));output(x);output(g);}",
        &[
            (0, &[0, 0, 0, 0, 0, 0, 3]),
            (255, &[255, 255, 255, 255, 255, 255, 3]),
        ],
    );
}

#[test]
fn direct_bindings_preserve_global_argument_snapshots() {
    verify_case(
        "global-snapshot",
        "cell g;cell stable(cell x){output(g);return x;}cell mutate(cell x){g+=1;return x;}void main(){g=input();output(stable(g));output(mutate(g));output(g);}",
        &[(0, &[0, 0, 0, 1]), (255, &[255, 255, 255, 0])],
    );
}

#[test]
fn direct_bindings_materialize_updates_before_branches_and_loops() {
    verify_case(
        "updates",
        "cell g;cell change(cell x,cell k){g+=1;x+=1;if(k){x+=k;}else{x+=2;}while(k){x+=1;k-=1;}return x;}void main(){cell x=input();output(change(x,0));output(change(x,2));output(x);output(g);}",
        &[(0, &[3, 5, 0, 2]), (255, &[2, 4, 255, 2])],
    );
}

#[test]
fn direct_bindings_consume_dead_arguments_after_the_last_alias() {
    verify_case(
        "dead-duplicate",
        "cell g;cell both(cell a,cell b){g+=1;output(a);a+=1;return b;}void main(){cell x=input();output(both(x,x));output(g);}",
        &[(0, &[0, 0, 1]), (255, &[255, 255, 1])],
    );
}

#[test]
fn direct_bindings_cover_allocated_argument_aliases() {
    // Source lowering snapshots duplicate arguments into separate virtual
    // cells. Handwritten allocated CIR can pass the very same physical cell.
    // Isolate the experimental env knobs in a child test process.
    if std::env::var("BFC_TEST_RAW_BINDINGS_CHILD").as_deref() != Ok("1") {
        for mode in ["1", "2"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "direct_regions::direct_bindings_cover_allocated_argument_aliases",
                ])
                .env("BFC_TEST_RAW_BINDINGS_CHILD", "1")
                .env("BFC_EVAL_DIRECT_BINDINGS", mode)
                .env("BFC_EVAL_DIRECT_REGION", "2")
                .env("BFC_EVAL_DIRECT_DYNAMIC", "1")
                .env("BFC_EVAL_DIRECT_RETURN", "1")
                .env("BFC_EVAL_DIRECT_RAW_LIMIT", "1048576")
                .env("BFC_EVAL_DIRECT_CODE_LIMIT", "65536")
                .env("BFC_EVAL_DIRECT_RESUME_CODE_LIMIT", "65536")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "mode={mode}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    use bf_compiler::{
        AbiCodegenOptions, Address as A, Continuation as C, ContinuationId as Id,
        ContinuationProgram, FrameInstruction as I, FrameSlot as S, FunctionDescriptor,
        FunctionId as F, GlobalDescriptor, GlobalId, ProfileGranularity, Terminator as T,
        ValueOperand as V, ValueType, lower_continuations_with_profile_and_codegen_options,
        optimize_annotated_bf,
    };
    let id = |n| Id::new(n).unwrap();
    let slot = |n| A::Frame(S::new(n));
    let global = A::Global(GlobalId::new(0));
    for distinct_sources in [false, true] {
        let parameters = vec![S::new(0), S::new(1)];
        let mut caller = vec![I::Input { dst: slot(0) }];
        if distinct_sources {
            caller.push(I::Set {
                dst: slot(1),
                value: 17,
            });
        }
        let mut body = vec![I::AddConst {
            dst: global,
            value: 1,
        }];
        body.push(I::AddConst {
            dst: slot(0),
            value: 1,
        });
        body.push(I::Output { src: slot(0) });
        let program = ContinuationProgram::new_with_globals(
            F::new(0),
            vec![GlobalDescriptor::cell(GlobalId::new(0))],
            vec![
                FunctionDescriptor::new(F::new(0), vec![], 2, ValueType::Void, id(1)),
                FunctionDescriptor::new(F::new(1), parameters, 2, ValueType::Cell, id(2)),
            ],
            vec![
                C::new(
                    id(1),
                    F::new(0),
                    caller,
                    T::Call {
                        callee: F::new(1),
                        arguments: vec![
                            V::Cell(slot(0)),
                            V::Cell(slot(usize::from(distinct_sources))),
                        ],
                        return_to: id(3),
                    },
                ),
                C::new(
                    id(2),
                    F::new(1),
                    body,
                    T::Return {
                        value: Some(V::Cell(slot(1))),
                    },
                ),
                C::new(
                    id(3),
                    F::new(0),
                    vec![I::Output { src: A::AbiValue }, I::Output { src: global }],
                    T::Halt,
                ),
            ],
        )
        .unwrap();
        for static_frames in [false, true] {
            let bf = lower_continuations_with_profile_and_codegen_options(
                &program,
                ProfileGranularity::Abi,
                AbiCodegenOptions {
                    static_frames,
                    ..Default::default()
                },
            )
            .unwrap();
            let artifact = optimize_annotated_bf(&bf).profile_artifact(true);
            for input in 0u8..=255 {
                let expected = vec![
                    input.wrapping_add(1),
                    if distinct_sources { 17 } else { input },
                    1,
                ];
                let mut native_counts = None;
                for rle_only in [false, true] {
                    let result = bf_interpreter::run_with_options(
                        artifact.source.as_bytes(),
                        &[input],
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
                    assert_eq!(result.output, expected);
                    let counts = (
                        result.stats.executed_instructions,
                        result.stats.executed_rle_instructions,
                    );
                    if let Some(expected) = native_counts {
                        assert_eq!(counts, expected);
                    } else {
                        native_counts = Some(counts);
                    }
                }
            }
        }
    }
}
