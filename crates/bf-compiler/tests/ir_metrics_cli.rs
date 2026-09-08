use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use bf_compiler::{
    SelfhostCirContinuation, SelfhostCirFunction, SelfhostCirInstruction, SelfhostCirProgram,
    SelfhostCirReturnType, SelfhostCirTerminator,
};
use serde_json::Value;

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

#[test]
fn phase_portal_metrics_use_explicit_identity_and_activation_regions() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tmp/ir-metrics-phase-portal-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("input.bfc"),
        include_str!("../../../scripts/selfhost-2c/fixtures/phase-portal-metrics.bfc"),
    )
    .unwrap();
    fs::write(
        root.join("phase-config.json"),
        include_str!("../../../scripts/selfhost-2c/fixtures/phase-portal-metrics-source.json"),
    )
    .unwrap();

    let result = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            "metrics.json",
            "--ir-phase-config",
            "phase-config.json",
            "--ir-artifact-id",
            "fixture-source",
            "--ir-progress-interval",
            "86400s",
            "input.bfc",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout.len(), 8);

    let report: Value =
        serde_json::from_str(&fs::read_to_string(root.join("metrics.json")).unwrap()).unwrap();
    assert_eq!(report["format"], "bfc-continuation-ir-metrics-v2");
    assert_eq!(report["artifact_identity"], "fixture-source");
    assert_eq!(report["phase_config"]["artifact"]["kind"], "source");
    assert_eq!(report["accounting"]["ok"], true);
    assert!(report["run"]["aggregate_loads"].as_u64().unwrap() > 0);
    assert!(report["run"]["aggregate_stores"].as_u64().unwrap() > 0);
    assert_eq!(
        report["phase_metrics"]["portal"]["total_requests"],
        report["run"]["array_loads"].as_u64().unwrap()
            + report["run"]["array_stores"].as_u64().unwrap()
            + report["run"]["aggregate_loads"].as_u64().unwrap()
            + report["run"]["aggregate_stores"].as_u64().unwrap()
    );

    let phase_metrics = &report["phase_metrics"];
    assert!(
        phase_metrics["phase_names"]
            .as_array()
            .unwrap()
            .iter()
            .any(|phase| phase == "unknown")
    );
    assert!(
        phase_metrics["phase_names"]
            .as_array()
            .unwrap()
            .iter()
            .any(|phase| phase == "recursive")
    );
    let requests = phase_metrics["portal"]["requests"].as_array().unwrap();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| request["function_name"].is_string())
    );
    assert!(
        requests
            .iter()
            .any(|request| request["region"]["kind"] == "global")
    );
    let recursive_frame_activations = requests
        .iter()
        .filter(|request| request["phase"] == "recursive" && request["region"]["kind"] == "frame")
        .filter_map(|request| request["region"]["activation_id"].as_u64())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(recursive_frame_activations.len() >= 2);
    assert!(
        phase_metrics["portal"]["by_phase"]
            .as_array()
            .unwrap()
            .iter()
            .any(|phase| {
                phase["start_chunk_revisits"]["8"]["requests"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
            })
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cir_phase_metrics_require_and_preserve_explicit_function_ids() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tmp/ir-metrics-cir-phase-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("input.cir"), cir_fixture()).unwrap();
    fs::write(
        root.join("phase-config.json"),
        include_str!("../../../scripts/selfhost-2c/fixtures/phase-portal-metrics-cir.json"),
    )
    .unwrap();

    let result = run_bfc(
        &root,
        &[
            "--cir-input",
            "input.cir",
            "--run-ir",
            "--ir-metrics",
            "metrics.json",
            "--ir-phase-config",
            "phase-config.json",
            "--ir-artifact-id",
            "fixture-cir",
            "--ir-progress-interval",
            "86400s",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"A");
    let report: Value =
        serde_json::from_str(&fs::read_to_string(root.join("metrics.json")).unwrap()).unwrap();
    assert_eq!(report["format"], "bfc-continuation-ir-metrics-v2");
    assert_eq!(report["source_kind"], "cir");
    assert_eq!(report["phase_config"]["artifact"]["id"], "fixture-cir");
    assert_eq!(
        report["phase_metrics"]["phase_boundaries"][0]["function_name"],
        Value::Null
    );
    assert!(
        report["phase_metrics"]["continuations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["phase"] == "cir_main")
    );

    fs::remove_dir_all(root).unwrap();
}
