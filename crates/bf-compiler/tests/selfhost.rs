//! The complete selfhost source is intentionally kept out of quick unit runs.
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use bf_compiler::{self as bfc, ContinuationOptimizationOptions};

#[test]
#[ignore = "lowers the complete selfhost compiler twice; run with --release --ignored"]
fn source_inline_preserves_selfhost_cir_bytes_and_generated_programs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let concatenated = Command::new("bash")
        .arg("scripts/concat-stage2-compiler.sh")
        .arg("cir")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(concatenated.status.success());
    let compiler_source = String::from_utf8(concatenated.stdout).unwrap();
    type CompilerCase = (&'static str, &'static [u8], Option<&'static [u8]>);
    let cases: [CompilerCase; 4] = [
        (
            "void main(){cell n=input();while(n){output(n);n-=1;}}",
            &[3],
            Some(&[3, 2, 1]),
        ),
        (
            include_str!("../../../selfhost/stage2/examples/stage8_snapshots.bfc"),
            &[],
            Some(&[1, 9]),
        ),
        (
            "cell down(cell n){if(n){return down(n-1)+1;}return 0;}void main(){output(down(input()));}",
            &[4],
            Some(&[4]),
        ),
        ("cell bad(cell n){if(n){return 1;}}void main(){}", &[], None),
    ];
    let mut baseline: Vec<(Vec<u8>, u64, bool)> = Vec::new();
    for inline_functions in [false, true] {
        let start = Instant::now();
        let (compiler, _) = bfc::lower_source_with_options(
            &compiler_source,
            ContinuationOptimizationOptions {
                inline_functions,
                ..Default::default()
            },
        )
        .unwrap();
        eprintln!(
            "selfhost inline={inline_functions} lower_ms={} functions={} blocks={}",
            start.elapsed().as_millis(),
            compiler.functions().len(),
            compiler.continuations().len()
        );
        for (index, (source, input, expected)) in cases.iter().enumerate() {
            let mut emitted = Vec::new();
            let stats = bfc::run_continuations_with_io(
                &compiler,
                &mut source.as_bytes(),
                &mut emitted,
                Default::default(),
                |_| {},
            )
            .unwrap();
            if inline_functions {
                assert_eq!(
                    (&emitted, stats.input_operations, stats.aborted),
                    (&baseline[index].0, baseline[index].1, baseline[index].2),
                    "selfhost case {index}"
                );
            } else {
                baseline.push((emitted.clone(), stats.input_operations, stats.aborted));
            }
            eprintln!(
                "selfhost inline={inline_functions} case={index} calls={} CIR_visits={} output_bytes={}",
                stats.calls,
                stats.executed_continuations,
                emitted.len()
            );
            let Some(expected) = expected else {
                assert!(String::from_utf8_lossy(&emitted).contains("BFC_STAGE12_ERROR"));
                continue;
            };
            let wire = bfc::SelfhostCirProgram::decode(&emitted).unwrap();
            let product = bfc::lower_selfhost_cir(&wire).unwrap();
            for region_emission in [false, true] {
                let bf = bfc::optimize_bf(
                    &bfc::lower_continuations_with_codegen_options(
                        &product,
                        bfc::AbiCodegenOptions {
                            region_emission,
                            ..Default::default()
                        },
                    )
                    .unwrap(),
                )
                .to_source();
                let result = bf_interpreter::run_with_options(
                    bf.as_bytes(),
                    input,
                    bf_interpreter::RunOptions {
                        collect_stats: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                assert_eq!(&result.output, expected);
                eprintln!(
                    "selfhost product case={index} regions={region_emission} bytes={} raw={} RLE={}",
                    bf.len(),
                    result.stats.executed_instructions,
                    result.stats.executed_rle_instructions
                );
            }
        }
    }
}

#[test]
#[ignore = "runs the full selfhost suite through source lowering; use --release --ignored"]
fn full_selfhost_suite_runs_with_cir_inline_default() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let concatenated = Command::new("bash")
        .arg("scripts/concat-stage2-compiler.sh")
        .arg("test")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(concatenated.status.success());
    let source = String::from_utf8(concatenated.stdout).unwrap();
    let started = Instant::now();
    let program = bfc::lower_source(&source).unwrap();
    eprintln!(
        "selfhost suite lower_ms={} functions={} blocks={}",
        started.elapsed().as_millis(),
        program.functions().len(),
        program.continuations().len()
    );
    let mut output = Vec::new();
    let stats = bfc::run_continuations_with_io(
        &program,
        &mut &[][..],
        &mut output,
        Default::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(output, b"ok\n");
    assert!(!stats.aborted);
    eprintln!(
        "selfhost suite CIR_visits={} Calls={} frame_instructions={}",
        stats.executed_continuations, stats.calls, stats.executed_frame_instructions
    );
}
