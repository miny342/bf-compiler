use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use bf_compiler::{
    SelfhostCirContinuation, SelfhostCirFunction, SelfhostCirInstruction, SelfhostCirProgram,
    SelfhostCirReturnType, SelfhostCirTerminator,
};

fn run_bfc(root: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bfc"))
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("failed to execute bfc")
}

fn test_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tmp/ir-metrics-cli-regression-{}",
        std::process::id()
    ))
}

fn cir_fixture() -> Vec<u8> {
    SelfhostCirProgram::new(
        0,
        0,
        vec![SelfhostCirFunction {
            id: 0,
            entry: 1,
            frame_cells: 1,
            return_type: SelfhostCirReturnType::Void,
            parameters: vec![],
        }],
        vec![SelfhostCirContinuation {
            id: 1,
            function: 0,
            instructions: vec![
                SelfhostCirInstruction::Set {
                    destination: 0,
                    value: b'A',
                },
                SelfhostCirInstruction::Output { source: 0 },
            ],
            terminator: SelfhostCirTerminator::Halt,
        }],
    )
    .unwrap()
    .encode()
    .unwrap()
}

#[test]
fn ir_metrics_rejects_source_and_cir_input_collisions_without_overwriting() {
    let root = test_root();
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let source = root.join("input.bfc");
    fs::write(&source, include_str!("fixtures/ir_metrics_input.bfc")).unwrap();
    let source_before = fs::read(&source).unwrap();

    let rejected_relative = run_bfc(
        &root,
        &["--run-ir", "--ir-metrics", "input.bfc", "input.bfc"],
    );
    assert!(!rejected_relative.status.success());
    assert!(
        String::from_utf8_lossy(&rejected_relative.stderr)
            .contains("--ir-metrics cannot overwrite an input source")
    );
    assert_eq!(fs::read(&source).unwrap(), source_before);

    let source_absolute = fs::canonicalize(&source).unwrap();
    let rejected_absolute = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            source_absolute.to_str().unwrap(),
            "input.bfc",
        ],
    );
    assert!(!rejected_absolute.status.success());
    assert_eq!(fs::read(&source).unwrap(), source_before);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&source, root.join("input-link.bfc")).unwrap();
        let rejected_symlink = run_bfc(
            &root,
            &["--run-ir", "--ir-metrics", "input-link.bfc", "input.bfc"],
        );
        assert!(!rejected_symlink.status.success());
        assert_eq!(fs::read(&source).unwrap(), source_before);
    }

    let metrics = root.join("source-metrics.json");
    assert!(source.exists());
    let accepted_source = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            "source-metrics.json",
            "input.bfc",
        ],
    );
    assert!(accepted_source.status.success());
    assert!(
        fs::read_to_string(metrics)
            .unwrap()
            .contains("bfc-continuation-ir-metrics-v1")
    );
    assert_eq!(fs::read(&source).unwrap(), source_before);

    let cir = root.join("input.cir");
    fs::write(&cir, cir_fixture()).unwrap();
    let cir_before = fs::read(&cir).unwrap();
    let cir_absolute = fs::canonicalize(&cir).unwrap();
    let rejected_cir = run_bfc(
        &root,
        &[
            "--cir-input",
            "input.cir",
            "--run-ir",
            "--ir-metrics",
            cir_absolute.to_str().unwrap(),
        ],
    );
    assert!(!rejected_cir.status.success());
    assert!(
        String::from_utf8_lossy(&rejected_cir.stderr)
            .contains("--ir-metrics cannot overwrite the CIR input")
    );
    assert_eq!(fs::read(&cir).unwrap(), cir_before);

    let cir_metrics = root.join("cir-metrics.json");
    let accepted_cir = run_bfc(
        &root,
        &[
            "--cir-input",
            "input.cir",
            "--run-ir",
            "--ir-metrics",
            "cir-metrics.json",
        ],
    );
    assert!(accepted_cir.status.success());
    assert!(
        fs::read_to_string(cir_metrics)
            .unwrap()
            .contains("bfc-continuation-ir-metrics-v1")
    );
    assert_eq!(fs::read(&cir).unwrap(), cir_before);

    fs::remove_dir_all(root).unwrap();
}
