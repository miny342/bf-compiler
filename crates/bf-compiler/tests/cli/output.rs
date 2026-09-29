use super::*;

#[test]
fn source_and_cir_inputs_share_snapshot_and_execution_outputs() {
    let root = test_root().with_file_name(format!("cli-snapshots-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.bfc"), "void main(){output('A');}").unwrap();
    fs::write(root.join("main.cir"), cir_fixture()).unwrap();
    for (kind, input) in [
        ("source", vec!["main.bfc"]),
        ("cir", vec!["--cir-input", "main.cir"]),
    ] {
        let mut args = vec!["--cir-output", "snapshot.json"];
        args.extend_from_slice(&input);
        let snapshot = run_bfc(&root, &args);
        assert!(snapshot.status.success(), "{:?}", snapshot);
        assert!(snapshot.stdout.is_empty());
        let before: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("snapshot.json")).unwrap()).unwrap();

        let mut args = vec![
            "--run-ir",
            "--cir-output",
            "snapshot.json",
            "--ir-dump",
            "dump.json",
            "--ir-metrics",
            "metrics.json",
        ];
        args.extend_from_slice(&input);
        let run = run_bfc(&root, &args);
        assert!(run.status.success(), "{:?}", run);
        assert_eq!(run.stdout, b"A");
        let read = |name| -> serde_json::Value {
            serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
        };
        let snapshot = read("snapshot.json");
        let dump = read("dump.json");
        let metrics = read("metrics.json");
        assert_eq!(before, snapshot);
        assert_eq!(snapshot, dump);
        assert_eq!(snapshot["source_kind"], kind);
        assert_eq!(snapshot["artifact_identity"], metrics["artifact_identity"]);
        assert_eq!(metrics["source_kind"], kind);
        assert_eq!(metrics["run"]["output_bytes"], 1);
    }
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
