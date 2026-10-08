use super::*;

#[test]
fn transport_flags_are_opt_in_for_dynamic_source_cir_and_all_output_formats() {
    use SelfhostCirInstruction as I;
    use bf_compiler::{
        SelfhostCirArrayOp, SelfhostCirBinaryOp, SelfhostCirGlobalOp, SelfhostCirStorage,
    };
    use bf_interpreter::{RunOptions, run_with_options};

    let root = test_root().with_file_name(format!("backend-flags-cli-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("main.bfc"),
        "cell[256] values; cell global; void main(){cell i=input(); \
         global=input(); cell saved=global; global=input(); values[i]=saved; \
         output(values[i]); output(saved); output(global); cell rhs=global; output(saved < rhs);}",
    )
    .unwrap();
    let cir = SelfhostCirProgram::new(
        257,
        0,
        vec![SelfhostCirFunction {
            id: 0,
            entry: 1,
            frame_cells: 6,
            return_type: SelfhostCirReturnType::Void,
            parameters: vec![],
        }],
        vec![SelfhostCirContinuation {
            id: 1,
            function: 0,
            instructions: vec![
                I::Input { destination: 1 },
                I::Copy {
                    destination: 5,
                    source: 1,
                },
                I::Input { destination: 0 },
                I::Global {
                    op: SelfhostCirGlobalOp::Store,
                    data: 0,
                    address: 256,
                },
                I::Global {
                    op: SelfhostCirGlobalOp::Copy,
                    data: 3,
                    address: 256,
                },
                I::Input { destination: 4 },
                I::Global {
                    op: SelfhostCirGlobalOp::Store,
                    data: 4,
                    address: 256,
                },
                I::Set {
                    destination: 2,
                    value: 0,
                },
                // CIR array stores consume data and offset slots.
                I::Copy {
                    destination: 0,
                    source: 3,
                },
                I::Array {
                    op: SelfhostCirArrayOp::Store,
                    data: 0,
                    offset_low: 1,
                    offset_high: 2,
                    base: 0,
                    cells: 256,
                    storage: SelfhostCirStorage::Global,
                },
                I::Copy {
                    destination: 1,
                    source: 5,
                },
                I::Array {
                    op: SelfhostCirArrayOp::Load,
                    data: 0,
                    offset_low: 1,
                    offset_high: 2,
                    base: 0,
                    cells: 256,
                    storage: SelfhostCirStorage::Global,
                },
                I::Output { source: 0 },
                I::Output { source: 3 },
                // CIR global stores consume their data slot; read it back.
                I::Global {
                    op: SelfhostCirGlobalOp::Copy,
                    data: 4,
                    address: 256,
                },
                I::Output { source: 4 },
                I::Copy {
                    destination: 0,
                    source: 3,
                },
                I::Copy {
                    destination: 1,
                    source: 4,
                },
                I::Binary {
                    op: SelfhostCirBinaryOp::Less,
                    destination: 0,
                    source: 1,
                },
                I::Output { source: 0 },
            ],
            terminator: SelfhostCirTerminator::Halt,
        }],
    )
    .unwrap();
    fs::write(root.join("main.cir"), cir.encode().unwrap()).unwrap();
    for input in [vec!["main.bfc"], vec!["--cir-input", "main.cir"]] {
        for unbounded in [false, true] {
            let mut identities = Vec::new();
            for flags in 0..8 {
                let nibble = flags & 1 != 0;
                let mut arguments = input.clone();
                // These flags specialize dynamic frame/global crossings.
                // Fixed contexts are covered separately below.
                arguments.push("--disable-static-frames");
                if unbounded {
                    arguments.push("--unlimited-tape");
                }
                if nibble {
                    arguments.push("--enable-nibble-transfer");
                }
                if flags & 2 != 0 {
                    arguments.push("--enable-inplace-compare");
                }
                if flags & 4 != 0 {
                    arguments.push("--enable-anchor-bank");
                }
                let plain = run_bfc(&root, &arguments);
                assert!(plain.status.success(), "{:?}", plain.stderr);
                let identity = bf_profiling::bf_identity(&plain.stdout);
                identities.push(identity);
                for compressed in [false, true] {
                    let mut args = arguments.clone();
                    if compressed {
                        args.push("--compressed-bf");
                    }
                    let ordinary = run_bfc(&root, &args);
                    assert!(ordinary.status.success(), "{:?}", ordinary.stderr);
                    assert_eq!(bf_profiling::bf_identity(&ordinary.stdout), identity);
                    args.extend(["--profile-map-output", "map.json", "--embed-profile"]);
                    let profiled = run_bfc(&root, &args);
                    assert!(profiled.status.success(), "{:?}", profiled.stderr);
                    assert_eq!(bf_profiling::bf_identity(&profiled.stdout), identity);
                    let map_json = fs::read_to_string(root.join("map.json")).unwrap();
                    assert_eq!(map_json.contains("abi.portal.route.decompose"), nibble);
                    let map = bf_profiling::ProfileMap::from_json(&map_json).unwrap();
                    map.validate_for_source(&profiled.stdout).unwrap();
                    assert_eq!(
                        bf_profiling::embedded_profile_map(&profiled.stdout).unwrap(),
                        Some(map)
                    );
                    for value in [0, 1, 15, 16, 127, 128, 254, 255] {
                        let mut logical_counts = None;
                        for disable_remote_transfer in [false, true] {
                            let run = run_with_options(
                                &profiled.stdout,
                                &[value, value, 255 - value],
                                RunOptions {
                                    unbounded_tape: unbounded,
                                    collect_stats: true,
                                    disable_remote_transfer,
                                    disable_compare: disable_remote_transfer,
                                    ..RunOptions::default()
                                },
                            )
                            .unwrap();
                            assert_eq!(
                                run.output,
                                [value, value, 255 - value, u8::from(value < 255 - value)],
                                "{args:?}, RT disabled={disable_remote_transfer}"
                            );
                            let counts = (
                                run.stats.executed_instructions,
                                run.stats.executed_rle_instructions,
                            );
                            if let Some(reference) = logical_counts {
                                assert_eq!(
                                    counts, reference,
                                    "native recognition must preserve logical counts: {args:?}"
                                );
                            }
                            logical_counts = Some(counts);
                        }
                    }
                }
            }
            for flags in 0..8 {
                for bit in [1, 2, 4] {
                    // Binary CIR keeps its flat frame as one aggregate;
                    // private scalar guards cannot split that storage.
                    if bit == 2 && input[0] == "--cir-input" {
                        assert_eq!(identities[flags], identities[flags ^ bit]);
                        continue;
                    }
                    assert_ne!(
                        identities[flags],
                        identities[flags ^ bit],
                        "each flag must change generated BF independently: {input:?}, flags={flags}, bit={bit}"
                    );
                }
            }
        }
    }
    for flag in [
        "--enable-nibble-transfer",
        "--enable-inplace-compare",
        "--enable-anchor-bank",
    ] {
        let invalid = run_bfc(&root, &["--run-ir", flag, "main.bfc"]);
        assert!(!invalid.status.success());
        assert!(
            String::from_utf8_lossy(&invalid.stderr).contains("Brainfuck code-generation options")
        );
    }
    for input in [vec!["main.bfc"], vec!["--cir-input", "main.cir"]] {
        let mut arguments = input;
        arguments.extend([
            "--experimental-static-frames",
            "--enable-anchor-bank",
            "--enable-nibble-transfer",
            "--enable-inplace-compare",
            "--compressed-bf",
            "--profile-map-output",
            "combined.json",
        ]);
        let combined = run_bfc(&root, &arguments);
        assert!(combined.status.success(), "{:?}", combined.stderr);
        let map = bf_profiling::ProfileMap::from_json(
            &fs::read_to_string(root.join("combined.json")).unwrap(),
        )
        .unwrap();
        map.validate_for_source(&combined.stdout).unwrap();
        for value in [0, 1, 127, 255] {
            let run = run_with_options(
                &combined.stdout,
                &[value, value, 255 - value],
                RunOptions::default(),
            )
            .unwrap();
            assert_eq!(
                run.output,
                [value, value, 255 - value, u8::from(value < 255 - value)]
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fixed_contexts_are_default_for_source_cir_and_preserve_ir_mode() {
    let root = test_root().with_file_name(format!("fixed-default-cli-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.bfc"),
        "cell[256] g; cell leaf(cell i) { g[i] = i; return g[i]; } void recurse(cell n) { cell keep = leaf(n); if (n) { recurse(n - 1); } output(keep); } void main() { recurse(input()); }"
    ).unwrap();
    use bf_compiler::{SelfhostCirGlobalOp, SelfhostCirInstruction as I};
    let cir = SelfhostCirProgram::new(
        1,
        0,
        vec![
            SelfhostCirFunction {
                id: 0,
                entry: 1,
                frame_cells: 1,
                return_type: SelfhostCirReturnType::Void,
                parameters: vec![],
            },
            SelfhostCirFunction {
                id: 1,
                entry: 2,
                frame_cells: 1,
                return_type: SelfhostCirReturnType::Void,
                parameters: vec![],
            },
        ],
        vec![
            SelfhostCirContinuation {
                id: 1,
                function: 0,
                instructions: vec![
                    I::Input { destination: 0 },
                    I::Global {
                        op: SelfhostCirGlobalOp::Store,
                        data: 0,
                        address: 0,
                    },
                ],
                terminator: SelfhostCirTerminator::Call {
                    callee: 1,
                    arguments: vec![],
                    return_to: 3,
                },
            },
            SelfhostCirContinuation {
                id: 2,
                function: 1,
                instructions: vec![
                    I::Global {
                        op: SelfhostCirGlobalOp::Copy,
                        data: 0,
                        address: 0,
                    },
                    I::Output { source: 0 },
                ],
                terminator: SelfhostCirTerminator::ReturnVoid,
            },
            SelfhostCirContinuation {
                id: 3,
                function: 0,
                instructions: vec![],
                terminator: SelfhostCirTerminator::Halt,
            },
        ],
    )
    .unwrap();
    fs::write(root.join("main.cir"), cir.encode().unwrap()).unwrap();
    for input in [
        vec!["--disable-function-inline", "main.bfc"],
        vec!["--cir-input", "main.cir"],
    ] {
        for compressed in [false, true] {
            let mut default_identity = None;
            for flag in [
                None,
                Some("--enable-static-frames"),
                Some("--experimental-static-frames"),
                Some("--disable-static-frames"),
            ] {
                let mut args = input.clone();
                if let Some(flag) = flag {
                    args.push(flag);
                }
                if compressed {
                    args.push("--compressed-bf");
                }
                args.extend(["--profile-map-output", "map.json", "--embed-profile"]);
                let bf = run_bfc(&root, &args);
                assert!(bf.status.success(), "{:?}", bf.stderr);
                let identity = bf_profiling::bf_identity(&bf.stdout);
                let map = bf_profiling::ProfileMap::from_json(
                    &fs::read_to_string(root.join("map.json")).unwrap(),
                )
                .unwrap();
                map.validate_for_source(&bf.stdout).unwrap();
                assert_eq!(
                    bf_profiling::embedded_profile_map(&bf.stdout).unwrap(),
                    Some(map.clone())
                );
                let initialization = map
                    .sites
                    .iter()
                    .find(|s| s.stable_key == "abi.initialization")
                    .unwrap();
                if flag == Some("--disable-static-frames") {
                    assert_ne!(Some(identity), default_identity);
                    assert!(!initialization.attributes.contains_key("static_functions"));
                } else {
                    assert!(
                        initialization.attributes["static_functions"]
                            .parse::<usize>()
                            .unwrap()
                            > 0
                    );
                    if let Some(reference) = default_identity {
                        assert_eq!(identity, reference);
                    }
                    default_identity = Some(identity);
                }
                for n in [0, 1, 3] {
                    let result = bf_interpreter::run_with_options(
                        &bf.stdout,
                        &[n],
                        bf_interpreter::RunOptions::default(),
                    )
                    .unwrap();
                    assert_eq!(
                        result.output,
                        if input[0] == "--cir-input" {
                            vec![n]
                        } else {
                            (0..=n).collect::<Vec<_>>()
                        },
                        "{args:?}"
                    );
                }
            }
        }
        let mut args = input.clone();
        args.push("--run-ir");
        let ir = run_bfc_with_stdin(&root, &args, &[3]);
        assert!(ir.status.success(), "{:?}", ir.stderr);
        assert_eq!(
            ir.stdout,
            if input[0] == "--cir-input" {
                vec![3]
            } else {
                vec![0, 1, 2, 3]
            }
        );
        for flag in [
            "--enable-static-frames",
            "--disable-static-frames",
            "--experimental-static-frames",
        ] {
            let mut args = args.clone();
            args.push(flag);
            let invalid = run_bfc(&root, &args);
            assert!(!invalid.status.success());
            assert!(
                String::from_utf8_lossy(&invalid.stderr)
                    .contains("Brainfuck code-generation options")
            );
        }
    }
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
        // Source loops are structured before either CFG option runs.
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
    assert_eq!(after["optimization"]["local_structure"]["loops"], 0);
    assert_eq!(before["run"]["executed_continuations"], 1);
    assert_eq!(after["run"]["executed_continuations"], 1);
    assert_eq!(after["accounting"]["ok"], true);
    assert_eq!(
        after["artifact_identity_definition"]["version"],
        "bfc-ir-artifact-v6"
    );
    let options = &after["measurement_options"]["lowering_options"];
    assert_eq!(options["structure_local_control_flow"], true);
    let id = after["artifact_identity"].as_str().unwrap();
    let mut config = json!({"format": "bfc-ir-phase-config-v1",
        "artifact": {"kind": "source", "id": id, "identity_version": "bfc-ir-artifact-v6",
            "lowering_options": options},
        "chunk_cells": [16], "phases": [{"name": "main", "function_name": "main"}]});
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
    config["artifact"]["identity_version"] = json!("bfc-ir-artifact-v4");
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
fn cir_inline_and_region_defaults_match_explicit_flags_and_preserve_execution() {
    let root = test_root().with_file_name(format!("cir-region-defaults-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.bfc"), "void mark(cell n){if(n){output(120);return;}output(121);}void worker(cell n){while(n){output(65);mark(n);output(66);mark(n);output(67);n-=1;}}void main(){worker(2);}").unwrap();
    let run = |flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bfc"))
            .current_dir(&root)
            .args(flags)
            .arg("main.bfc")
            .output()
            .unwrap()
    };
    let default = run(&[]);
    assert!(default.status.success(), "{:?}", default.stderr);
    let enabled = run(&["--enable-function-inline", "--enable-region-emission"]);
    assert!(enabled.status.success(), "{:?}", enabled.stderr);
    assert_eq!(default.stdout, enabled.stdout);
    for inline in ["--disable-function-inline", "--enable-function-inline"] {
        let fused = run(&[inline]);
        let ordinary = run(&[inline, "--disable-region-emission"]);
        for output in [&fused, &ordinary] {
            assert!(output.status.success(), "{:?}", output.stderr);
            assert_eq!(
                bf_interpreter::run(&output.stdout, &[]).unwrap(),
                b"AxBxCAxBxC"
            );
        }
        if inline == "--disable-function-inline" {
            assert_ne!(fused.stdout, ordinary.stdout);
        }
    }
    let baseline = run(&[
        "--disable-function-inline",
        "--run-ir",
        "--ir-metrics",
        "before.json",
    ]);
    let inlined = run(&["--run-ir", "--ir-metrics", "after.json"]);
    assert!(baseline.status.success());
    assert!(inlined.status.success());
    assert_eq!(baseline.stdout, inlined.stdout);
    let before: Value =
        serde_json::from_slice(&fs::read(root.join("before.json")).unwrap()).unwrap();
    let after: Value = serde_json::from_slice(&fs::read(root.join("after.json")).unwrap()).unwrap();
    assert_ne!(before["artifact_identity"], after["artifact_identity"]);
    assert_eq!(
        after["measurement_options"]["lowering_options"]["inline_functions"],
        true
    );
    assert!(before["run"]["calls"].as_u64().unwrap() > 0);
    assert_eq!(after["run"]["calls"], 0);
}
