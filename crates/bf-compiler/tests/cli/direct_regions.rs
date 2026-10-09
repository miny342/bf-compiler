//! Experimental call regions must preserve activation state and shared IDs.
use super::*;
use std::collections::BTreeSet;

fn verify_case(name: &str, source: &str, cases: &[(u8, &[u8])]) {
    let root = test_root().with_file_name(format!("direct-regions-{name}-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.bfc"), source).unwrap();
    for dynamic in [false, true] {
        for triple in [false, true] {
            let compile = |depth: &str, code_limit: &str| {
                let mut command = Command::new(env!("CARGO_BIN_EXE_bfc"));
                command
                    .current_dir(&root)
                    .env("BFC_EVAL_DIRECT_REGION", depth)
                    .env("BFC_EVAL_DIRECT_DYNAMIC", "1")
                    .env("BFC_EVAL_DIRECT_RETURN", "1")
                    .env("BFC_EVAL_DIRECT_RAW_LIMIT", "1048576")
                    .env("BFC_EVAL_DIRECT_CODE_LIMIT", code_limit)
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
                let cir_output = command.args(["--cir-output", "cir.json"]).output().unwrap();
                assert!(cir_output.status.success(), "{:?}", cir_output.stderr);
                (
                    output.stdout,
                    dispatch,
                    fs::read(root.join("cir.json")).unwrap(),
                )
            };
            let (baseline, dispatch, cir) = compile("0", "512");
            for code_limit in ["0", "512", "65536"] {
                let (bf, candidate_dispatch, candidate_cir) = compile("2", code_limit);
                assert_eq!(candidate_dispatch, dispatch, "{name}: dispatch targets");
                assert_eq!(candidate_cir, cir, "{name}: CIR changed");
                if code_limit == "0" {
                    assert_eq!(bf, baseline, "{name}: rejected trial changed BF");
                } else if code_limit == "65536" {
                    assert_ne!(bf, baseline, "{name}: test did not exercise expansion");
                }
                for &(input, expected) in cases {
                    for rle_only in [false, true] {
                        let result = bf_interpreter::run_with_options(
                            &bf,
                            &[input],
                            bf_interpreter::RunOptions {
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
