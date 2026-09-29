use super::*;
use std::io::Write;

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
        include_str!("../../../../scripts/selfhost-2c/fixtures/phase-portal-metrics.bfc"),
    )
    .unwrap();
    fs::write(
        root.join("phase-config.json"),
        include_str!("../../../../scripts/selfhost-2c/fixtures/phase-portal-metrics-source.json"),
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
            "1e17ca41e768ac8d1a36a9f5b745b5680e847e4b8fa4090bd6deb7cb4f3e0431",
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
    assert_eq!(
        report["artifact_identity"],
        "1e17ca41e768ac8d1a36a9f5b745b5680e847e4b8fa4090bd6deb7cb4f3e0431"
    );
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
                phase["start_chunk_revisits"]["16"]["requests"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
            })
    );

    fs::OpenOptions::new()
        .append(true)
        .open(root.join("input.bfc"))
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let stale_source = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            "stale.json",
            "--ir-phase-config",
            "phase-config.json",
            "--ir-artifact-id",
            "13545dc270735fe8ccad2283abb6f31f6b9d20cc6fe2c9304ce5aee9aeb40784",
            "--ir-progress-interval",
            "86400s",
            "input.bfc",
        ],
    );
    assert!(!stale_source.status.success());
    assert!(
        String::from_utf8_lossy(&stale_source.stderr)
            .contains("does not match the actual artifact identity")
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
    let identity_run = run_bfc(
        &root,
        &[
            "--cir-input",
            "input.cir",
            "--run-ir",
            "--ir-metrics",
            "identity.json",
            "--ir-progress-interval",
            "86400s",
        ],
    );
    assert!(identity_run.status.success());
    let identity_report: Value =
        serde_json::from_str(&fs::read_to_string(root.join("identity.json")).unwrap()).unwrap();
    let artifact_id = identity_report["artifact_identity"].as_str().unwrap();
    let mut phase_config: Value = serde_json::from_str(include_str!(
        "../../../../scripts/selfhost-2c/fixtures/phase-portal-metrics-cir.json"
    ))
    .unwrap();
    phase_config["artifact"]["id"] = Value::String(artifact_id.to_owned());
    fs::write(
        root.join("phase-config.json"),
        serde_json::to_vec_pretty(&phase_config).unwrap(),
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
            artifact_id,
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
    assert_eq!(report["phase_config"]["artifact"]["id"], artifact_id);
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

    let mut wrong_options = phase_config.clone();
    wrong_options["artifact"]["lowering_options"]["inline_branch_successors"] = Value::Bool(false);
    fs::write(
        root.join("wrong-options.json"),
        serde_json::to_vec_pretty(&wrong_options).unwrap(),
    )
    .unwrap();
    let wrong_options_result = run_bfc(
        &root,
        &[
            "--cir-input",
            "input.cir",
            "--run-ir",
            "--ir-metrics",
            "wrong-options-metrics.json",
            "--ir-phase-config",
            "wrong-options.json",
            "--ir-artifact-id",
            artifact_id,
            "--ir-progress-interval",
            "86400s",
        ],
    );
    assert!(!wrong_options_result.status.success());
    assert!(
        String::from_utf8_lossy(&wrong_options_result.stderr)
            .contains("lowering_options do not match")
    );

    let stdin_result = run_bfc_with_stdin(
        &root,
        &[
            "--cir-input",
            "-",
            "--run-ir",
            "--ir-metrics",
            "stdin-metrics.json",
            "--ir-phase-config",
            "phase-config.json",
            "--ir-artifact-id",
            artifact_id,
            "--ir-progress-interval",
            "86400s",
        ],
        &cir_fixture(),
    );
    assert!(stdin_result.status.success());
    assert_eq!(stdin_result.stdout, b"A");

    fs::write(root.join("input.cir"), [cir_fixture(), vec![0xff]].concat()).unwrap();
    let stale_cir = run_bfc(
        &root,
        &[
            "--cir-input",
            "input.cir",
            "--run-ir",
            "--ir-metrics",
            "stale.json",
            "--ir-phase-config",
            "phase-config.json",
            "--ir-artifact-id",
            artifact_id,
            "--ir-progress-interval",
            "86400s",
        ],
    );
    assert!(!stale_cir.status.success());
    assert!(
        String::from_utf8_lossy(&stale_cir.stderr)
            .contains("does not match the actual artifact identity")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn phase_metrics_break_portal_adjacency_across_a_non_portal_phase() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tmp/ir-metrics-phase-boundary-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("input.bfc"),
        include_str!("../../../../scripts/selfhost-2c/fixtures/phase-portal-phase-boundary.bfc"),
    )
    .unwrap();
    fs::write(
        root.join("phase-config.json"),
        include_str!(
            "../../../../scripts/selfhost-2c/fixtures/phase-portal-phase-boundary-source.json"
        ),
    )
    .unwrap();

    let result = run_bfc_with_stdin(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            "metrics.json",
            "--ir-phase-config",
            "phase-config.json",
            "--ir-artifact-id",
            "0303eced1b8cbdceed0546e74d78d737ea130e66cdef630f6a29df578aedb97a",
            "--ir-progress-interval",
            "86400s",
            "input.bfc",
        ],
        &[0],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, &[0, 0, 0]);
    let report: Value =
        serde_json::from_str(&fs::read_to_string(root.join("metrics.json")).unwrap()).unwrap();
    let phase_a = report["phase_metrics"]["portal"]["by_phase"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["phase"] == "phase_a")
        .unwrap();
    assert_eq!(phase_a["requests"], 3);
    assert_eq!(phase_a["adjacent_pairs"], 1);
    assert_eq!(phase_a["adjacent_same_region"], 1);
    assert_eq!(phase_a["offset_delta_histogram"], json!({"0": 1}));
    let definition = report["phase_metrics"]["portal"]["definitions"]["phase_change"]
        .as_str()
        .unwrap();
    assert!(definition.contains("same phase label"));
    assert!(
        report["phase_metrics"]["phase_names"]
            .as_array()
            .unwrap()
            .iter()
            .any(|phase| phase == "unknown")
    );

    fs::remove_dir_all(root).unwrap();
}
