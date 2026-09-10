use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bf_compiler::{
    SelfhostCirContinuation, SelfhostCirFunction, SelfhostCirInstruction, SelfhostCirProgram,
    SelfhostCirReturnType, SelfhostCirTerminator,
};
use serde_json::{Value, json};

fn run_bfc(root: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bfc"))
        .current_dir(root)
        .arg("--disable-local-control-flow")
        .args(arguments)
        .output()
        .expect("failed to execute bfc")
}

fn run_bfc_with_stdin(root: &Path, arguments: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bfc"))
        .current_dir(root)
        .arg("--disable-local-control-flow")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to execute bfc");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
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
fn compressed_bf_cli_preserves_source_cir_and_profile_artifacts() {
    use bf_interpreter::{ProfileMode, ProfileOptions, RunOptions, run_with_options};
    let root = test_root().with_file_name(format!("compressed-bf-cli-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.bfc"), "void main(){output('A');}").unwrap();
    fs::write(root.join("main.cir"), cir_fixture()).unwrap();
    for input in [vec!["main.bfc"], vec!["--cir-input", "main.cir"]] {
        for unbounded in [false, true] {
            let mut arguments = input.clone();
            if unbounded {
                arguments.push("--unlimited-tape");
            }
            let plain = run_bfc(&root, &arguments);
            assert!(plain.status.success(), "{:?}", plain.stderr);
            assert!(
                !plain
                    .stdout
                    .starts_with(bf_profiling::rle::HEADER.as_bytes())
            );
            arguments.push("--compressed-bf");
            let compressed = run_bfc(&root, &arguments);
            assert!(compressed.status.success(), "{:?}", compressed.stderr);
            assert!(compressed.stdout.len() < plain.stdout.len());
            assert_eq!(
                bf_profiling::bf_identity(&compressed.stdout),
                bf_profiling::bf_identity(&plain.stdout)
            );
            assert_eq!(bf_interpreter::run(&compressed.stdout, b"").unwrap(), b"A");
            for granularity in ["abi", "continuation", "instruction"] {
                let mut args = arguments.clone();
                args.extend([
                    "--profile-map-output",
                    "map.json",
                    "--profile-granularity",
                    granularity,
                    "--embed-profile",
                ]);
                let output = run_bfc(&root, &args);
                assert!(output.status.success(), "{:?}", output.stderr);
                let sidecar = bf_profiling::ProfileMap::from_json(
                    &fs::read_to_string(root.join("map.json")).unwrap(),
                )
                .unwrap();
                sidecar.validate_for_source(&output.stdout).unwrap();
                let embedded = bf_profiling::embedded_profile_map(&output.stdout)
                    .unwrap()
                    .unwrap();
                assert_eq!(sidecar, embedded);
                args.retain(|arg| *arg != "--compressed-bf");
                let plain_profile = run_bfc(&root, &args);
                assert!(plain_profile.status.success());
                let plain_map = bf_profiling::ProfileMap::from_json(
                    &fs::read_to_string(root.join("map.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(sidecar, plain_map);
                assert!(output.stdout.len() < plain_profile.stdout.len());
                for source in [&output.stdout, &plain_profile.stdout] {
                    let result = run_with_options(
                        source,
                        b"",
                        RunOptions {
                            unbounded_tape: unbounded,
                            profile: Some(ProfileOptions {
                                map: sidecar.clone(),
                                mode: ProfileMode::Counters,
                            }),
                            ..RunOptions::default()
                        },
                    )
                    .unwrap();
                    assert_eq!(result.output, b"A");
                }
            }
        }
    }
    let invalid = run_bfc(&root, &["--run-ir", "--compressed-bf", "main.bfc"]);
    assert!(!invalid.status.success());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_control_flow_options_bind_identity_and_preserve_execution() {
    let root = test_root().with_file_name(format!(
        "ir-metrics-local-control-flow-{}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("loop.bfc"),
        // Keep this a CFG-restoration test, not a direct scalar-loop test.
        "void main(){cell n=input();while(n != 0){n-=1;output(n+0);}output(n);}",
    )
    .unwrap();
    let baseline = run_bfc_with_stdin(
        &root,
        &["--run-ir", "--ir-metrics", "baseline.json", "loop.bfc"],
        &[3],
    );
    let candidate = run_bfc_with_stdin(
        &root,
        &[
            "--enable-local-control-flow",
            "--run-ir",
            "--ir-metrics",
            "candidate.json",
            "loop.bfc",
        ],
        &[3],
    );
    assert!(baseline.status.success(), "{:?}", baseline.stderr);
    assert!(candidate.status.success(), "{:?}", candidate.stderr);
    assert_eq!(baseline.stdout, [2, 1, 0, 0]);
    assert_eq!(candidate.stdout, baseline.stdout);
    let read = |name| serde_json::from_slice::<Value>(&fs::read(root.join(name)).unwrap()).unwrap();
    let before = read("baseline.json");
    let after = read("candidate.json");
    assert_ne!(before["artifact_identity"], after["artifact_identity"]);
    assert!(
        after["optimization"]["local_structure"]["loops"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        after["run"]["executed_continuations"].as_u64().unwrap()
            < before["run"]["executed_continuations"].as_u64().unwrap()
    );
    assert_eq!(after["accounting"]["ok"], true);
    assert_eq!(
        after["artifact_identity_definition"]["version"],
        "bfc-ir-artifact-v3"
    );
    let options = &after["measurement_options"]["lowering_options"];
    assert_eq!(options["structure_local_control_flow"], true);
    let id = after["artifact_identity"].as_str().unwrap();
    let mut config = json!({"format": "bfc-ir-phase-config-v1",
        "artifact": {"kind": "source", "id": id, "identity_version": "bfc-ir-artifact-v3",
            "lowering_options": options},
        "chunk_cells": [8, 16], "phases": [{"name": "main", "function_name": "main"}]});
    fs::write(root.join("phase.json"), config.to_string()).unwrap();
    let measured = run_bfc_with_stdin(
        &root,
        &[
            "--enable-local-control-flow",
            "--run-ir",
            "--ir-metrics",
            "phase-metrics.json",
            "--ir-phase-config",
            "phase.json",
            "--ir-artifact-id",
            id,
            "loop.bfc",
        ],
        &[3],
    );
    assert!(measured.status.success(), "{:?}", measured.stderr);
    assert_eq!(measured.stdout, baseline.stdout);
    assert_eq!(read("phase-metrics.json")["accounting"]["ok"], true);
    let stale_option = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            "rejected.json",
            "--ir-phase-config",
            "phase.json",
            "--ir-artifact-id",
            id,
            "loop.bfc",
        ],
    );
    assert!(!stale_option.status.success());
    assert!(String::from_utf8_lossy(&stale_option.stderr).contains("actual artifact identity"));
    config["artifact"]["id"] = before["artifact_identity"].clone();
    fs::write(root.join("phase.json"), config.to_string()).unwrap();
    let forged_option = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            "rejected.json",
            "--ir-phase-config",
            "phase.json",
            "--ir-artifact-id",
            before["artifact_identity"].as_str().unwrap(),
            "loop.bfc",
        ],
    );
    assert!(!forged_option.status.success());
    assert!(String::from_utf8_lossy(&forged_option.stderr).contains("lowering_options"));
    config["artifact"]["id"] = after["artifact_identity"].clone();
    config["artifact"]["identity_version"] = json!("bfc-ir-artifact-v2");
    fs::write(root.join("phase.json"), config.to_string()).unwrap();
    let stale_version = run_bfc(
        &root,
        &[
            "--enable-local-control-flow",
            "--run-ir",
            "--ir-metrics",
            "rejected.json",
            "--ir-phase-config",
            "phase.json",
            "--ir-artifact-id",
            id,
            "loop.bfc",
        ],
    );
    assert!(!stale_version.status.success());
    assert!(
        String::from_utf8_lossy(&stale_version.stderr).contains("identity_version"),
        "{}",
        String::from_utf8_lossy(&stale_version.stderr)
    );

    fs::write(root.join("input.cir"), cir_fixture()).unwrap();
    for variant in ["baseline", "candidate"] {
        let enabled = if variant == "candidate" {
            "--enable-local-control-flow"
        } else {
            "--disable-local-control-flow"
        };
        let run = run_bfc(
            &root,
            &[
                enabled,
                "--cir-input",
                "input.cir",
                "--run-ir",
                "--ir-metrics",
                &format!("cir-{variant}.json"),
            ],
        );
        assert!(run.status.success(), "{:?}", run.stderr);
        assert_eq!(run.stdout, b"A");
    }
    assert_ne!(
        read("cir-baseline.json")["artifact_identity"],
        read("cir-candidate.json")["artifact_identity"]
    );
    assert_eq!(
        read("cir-candidate.json")["measurement_options"]["lowering_options"]["structure_local_control_flow"],
        true
    );
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

    let phase_config = root.join("phase-config-input.json");
    fs::write(&phase_config, b"config sentinel").unwrap();
    let phase_config_before = fs::read(&phase_config).unwrap();
    let rejected_config_relative = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            "phase-config-input.json",
            "--ir-phase-config",
            "phase-config-input.json",
            "--ir-artifact-id",
            "stale",
            "input.bfc",
        ],
    );
    assert!(!rejected_config_relative.status.success());
    assert!(
        String::from_utf8_lossy(&rejected_config_relative.stderr)
            .contains("--ir-metrics cannot overwrite the phase config input")
    );
    assert_eq!(fs::read(&phase_config).unwrap(), phase_config_before);

    let phase_config_absolute = fs::canonicalize(&phase_config).unwrap();
    let rejected_config_absolute = run_bfc(
        &root,
        &[
            "--run-ir",
            "--ir-metrics",
            phase_config_absolute.to_str().unwrap(),
            "--ir-phase-config",
            "phase-config-input.json",
            "--ir-artifact-id",
            "stale",
            "input.bfc",
        ],
    );
    assert!(!rejected_config_absolute.status.success());
    assert_eq!(fs::read(&phase_config).unwrap(), phase_config_before);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&phase_config, root.join("phase-config-link.json")).unwrap();
        let rejected_config_symlink = run_bfc(
            &root,
            &[
                "--run-ir",
                "--ir-metrics",
                "phase-config-link.json",
                "--ir-phase-config",
                "phase-config-input.json",
                "--ir-artifact-id",
                "stale",
                "input.bfc",
            ],
        );
        assert!(!rejected_config_symlink.status.success());
        assert_eq!(fs::read(&phase_config).unwrap(), phase_config_before);
    }

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
            "13545dc270735fe8ccad2283abb6f31f6b9d20cc6fe2c9304ce5aee9aeb40784",
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
        "13545dc270735fe8ccad2283abb6f31f6b9d20cc6fe2c9304ce5aee9aeb40784"
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
                phase["start_chunk_revisits"]["8"]["requests"]
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
        "../../../scripts/selfhost-2c/fixtures/phase-portal-metrics-cir.json"
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
        include_str!("../../../scripts/selfhost-2c/fixtures/phase-portal-phase-boundary.bfc"),
    )
    .unwrap();
    fs::write(
        root.join("phase-config.json"),
        include_str!(
            "../../../scripts/selfhost-2c/fixtures/phase-portal-phase-boundary-source.json"
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
            "50599a97ced099221690da1eb215bb965268d9695e8d5ee16bc76f207f3118fd",
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
