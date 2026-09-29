use super::*;
use crate::FrameSlot;

fn execute(source: &str, input: &[u8]) -> (Vec<u8>, ContinuationRunStats) {
    let program = crate::lower_source_with_options(
        source,
        crate::ContinuationOptimizationOptions {
            inline_functions: false,
            ..Default::default()
        },
    )
    .unwrap()
    .0;
    let mut input = input;
    let mut output = Vec::new();
    let stats = run_continuations_with_io(
        &program,
        &mut input,
        &mut output,
        ContinuationRunOptions {
            collect_transitions: true,
            ..ContinuationRunOptions::default()
        },
        |_| {},
    )
    .unwrap();
    (output, stats)
}

#[test]
fn executes_cells_calls_and_control_flow() {
    let source = "cell marker; cell twice(cell value) { cell result = value + value; marker += 1; return result; } void main() { cell value = input(); while (value != 0) { output(twice(value)); value = value - 1; } }";
    let (output, stats) = execute(source, &[3]);
    assert_eq!(output, [6, 4, 2]);
    assert_eq!(stats.calls, 3);
    assert_eq!(stats.returns, 3);
    assert!(stats.executed_frame_instructions > 0);
    assert!(stats.max_call_depth >= 2);
    let program = crate::lower_source_with_options(
        source,
        crate::ContinuationOptimizationOptions {
            inline_functions: false,
            ..Default::default()
        },
    )
    .unwrap()
    .0;
    stats.validate_transition_accounting(&program).unwrap();
    assert!(
        stats
            .terminal_counts()
            .iter()
            .any(|(_, kind, _)| *kind == ContinuationTerminatorKind::Halt)
    );
}

#[test]
fn executes_dynamic_aggregate_access_and_return() {
    let source = "struct Pair { cell a; cell b; } Pair choose(Pair[2] values, cell index) { return values[index]; } void main() { Pair[2] values; values[1].a = 'O'; values[1].b = 'K'; Pair result = choose(values, 1); output(result.a); output(result.b); }";
    let (output, stats) = execute(source, &[]);
    assert_eq!(output, b"OK");
    assert!(stats.aggregate_loads > 0);
    assert_eq!(stats.calls, 1);
    let program = crate::lower_source_with_options(
        source,
        crate::ContinuationOptimizationOptions {
            inline_functions: false,
            ..Default::default()
        },
    )
    .unwrap()
    .0;
    stats.validate_transition_accounting(&program).unwrap();
}

#[test]
fn eof_input_is_zero() {
    let (output, stats) = execute("void main() { output(input()); output(input()); }", &[]);
    assert_eq!(output, [0, 0]);
    assert_eq!(stats.input_operations, 2);
}

#[test]
fn optionally_collects_hot_continuation_transitions() {
    // A scalar call keeps dispatcher edges even with structured HIR loops.
    let (program, _) = crate::lower_source_with_options(
        "cell marker; cell decrement(cell value) { cell result = value - 1; marker += 1; return result; } void main() { cell value = input(); while (value != 0) { output(value); value = decrement(value); } }",
        crate::ContinuationOptimizationOptions {
            structure_local_control_flow: false,
            ..Default::default()
        },
    )
    .unwrap();
    let mut input = &[2][..];
    let mut output = Vec::new();
    let stats = run_continuations_with_io(
        &program,
        &mut input,
        &mut output,
        ContinuationRunOptions {
            progress_interval: None,
            collect_transitions: true,
            ..ContinuationRunOptions::default()
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(output, [2, 1]);
    assert!(!stats.hottest_transitions(10).is_empty());
}

#[test]
fn transition_collection_does_not_change_execution_counters() {
    let source = "cell marker; cell decrement(cell value) { cell result = value - 1; marker += 1; return result; } void main() { cell value = input(); while (value != 0) { output(value); value = decrement(value); } }";
    let (program, _) = crate::lower_source_with_options(
        source,
        crate::ContinuationOptimizationOptions {
            structure_local_control_flow: false,
            ..Default::default()
        },
    )
    .unwrap();
    let run = |collect_transitions| {
        let mut input = &[2][..];
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                progress_interval: None,
                collect_transitions,
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        (output, stats)
    };
    let (output_on, stats_on) = run(true);
    let (output_off, stats_off) = run(false);
    assert_eq!(output_on, output_off);
    assert_eq!(
        stats_on.executed_continuations,
        stats_off.executed_continuations
    );
    assert_eq!(
        stats_on.executed_frame_instructions,
        stats_off.executed_frame_instructions
    );
    assert_eq!(stats_on.loop_iterations, stats_off.loop_iterations);
    assert_eq!(stats_on.calls, stats_off.calls);
    assert_eq!(stats_on.returns, stats_off.returns);
    assert!(stats_on.transitions_collected());
    assert!(!stats_off.transitions_collected());
    assert!(!stats_on.transition_counts().is_empty());
    stats_on.validate_transition_accounting(&program).unwrap();
    assert!(stats_off.validate_transition_accounting(&program).is_err());
}

#[test]
fn transition_metrics_record_portals_and_abort_terminals() {
    let source = "struct Pair { cell a; cell b; } Pair choose(Pair[2] values, cell index) { return values[index]; } void main() { Pair[2] values; values[1].a = 'O'; values[1].b = 'K'; Pair result = choose(values, input()); output(result.a); output(result.b); }";
    let program = crate::lower_source_with_options(
        source,
        crate::ContinuationOptimizationOptions {
            inline_functions: false,
            ..Default::default()
        },
    )
    .unwrap()
    .0;
    let mut input = &[1][..];
    let mut output = Vec::new();
    let stats = run_continuations_with_io(
        &program,
        &mut input,
        &mut output,
        ContinuationRunOptions {
            progress_interval: None,
            collect_transitions: true,
            ..ContinuationRunOptions::default()
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(output, b"OK");
    assert!(stats.aggregate_loads > 0);
    stats.validate_transition_accounting(&program).unwrap();

    let program = crate::lower_source("void main() { abort(); }").unwrap();
    let mut input = &[][..];
    let mut output = Vec::new();
    let stats = run_continuations_with_io(
        &program,
        &mut input,
        &mut output,
        ContinuationRunOptions {
            progress_interval: None,
            collect_transitions: true,
            ..ContinuationRunOptions::default()
        },
        |_| {},
    )
    .unwrap();
    assert!(stats.aborted);
    assert!(
        stats
            .terminal_counts()
            .iter()
            .any(|(_, kind, _)| *kind == ContinuationTerminatorKind::Abort)
    );
    stats.validate_transition_accounting(&program).unwrap();
}

#[test]
fn phase_metrics_are_opt_in_and_phase_boundaries_return_after_calls() {
    let source = "cell recurse(cell depth) { if (depth == 0) { return 7; } return recurse(depth - 1); } void main() { output(recurse(2)); }";
    let program = crate::lower_source_with_options(
        source,
        crate::ContinuationOptimizationOptions {
            inline_functions: false,
            ..Default::default()
        },
    )
    .unwrap()
    .0;
    let main = program
        .functions()
        .iter()
        .find(|function| function.name() == Some("main"))
        .unwrap();
    let recurse = program
        .functions()
        .iter()
        .find(|function| function.name() == Some("recurse"))
        .unwrap();
    let phase_config = ContinuationPhaseConfig {
        artifact_kind: "source".into(),
        artifact_id: "phase-test".into(),
        boundaries: vec![
            ContinuationPhaseBoundary {
                phase: "main".into(),
                function: main.id(),
            },
            ContinuationPhaseBoundary {
                phase: "recursive".into(),
                function: recurse.id(),
            },
        ],
        chunk_cells: vec![16],
    };
    let run = |phase_config| {
        let mut input = &[][..];
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                collect_transitions: true,
                phase_config,
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        (output, stats)
    };
    let (output_on, stats_on) = run(Some(phase_config));
    let (output_off, stats_off) = run(None);
    assert_eq!(output_on, output_off);
    assert_eq!(
        stats_on.executed_continuations,
        stats_off.executed_continuations
    );
    assert_eq!(
        stats_on.executed_frame_instructions,
        stats_off.executed_frame_instructions
    );
    assert_eq!(stats_on.calls, stats_off.calls);
    assert_eq!(stats_on.returns, stats_off.returns);
    assert!(stats_on.phase_metrics_json(&program).is_some());
    assert!(stats_off.phase_metrics_json(&program).is_none());
    let metrics = stats_on.phase_metrics_json(&program).unwrap();
    assert!(metrics["phase_names"].as_array().unwrap().len() >= 3);
    assert!(
        metrics["continuations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["phase"] == "recursive")
    );
    assert!(
        metrics["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["phase"] == "main" && row["kind"] == "halt")
    );
}

#[test]
fn phase_metrics_record_array_load_store_region_and_offset() {
    let cid = |value| ContinuationId::new(value).unwrap();
    let main = FunctionDescriptor::new_typed(
        FunctionId::new(0),
        vec![],
        2,
        vec![],
        0,
        ValueType::Void,
        cid(1),
    );
    let program = ContinuationProgram::new_with_globals(
        FunctionId::new(0),
        vec![GlobalDescriptor::array(GlobalId::new(0), 4)],
        vec![main],
        vec![
            Continuation::new(
                cid(1),
                FunctionId::new(0),
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: 2,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(1)),
                        value: 65,
                    },
                ],
                Terminator::ArrayStore {
                    array: AggregateRegion::Global(GlobalId::new(0)),
                    index: Address::Frame(FrameSlot::new(0)),
                    value: Address::Frame(FrameSlot::new(1)),
                    return_to: cid(2),
                },
            ),
            Continuation::new(
                cid(2),
                FunctionId::new(0),
                vec![],
                Terminator::ArrayLoad {
                    array: AggregateRegion::Global(GlobalId::new(0)),
                    index: Address::Frame(FrameSlot::new(0)),
                    destination: Address::Frame(FrameSlot::new(1)),
                    return_to: cid(3),
                },
            ),
            Continuation::new(cid(3), FunctionId::new(0), vec![], Terminator::Halt),
        ],
    )
    .unwrap();
    let mut input = &[][..];
    let mut output = Vec::new();
    let stats = run_continuations_with_io(
        &program,
        &mut input,
        &mut output,
        ContinuationRunOptions {
            collect_transitions: true,
            phase_config: Some(ContinuationPhaseConfig {
                artifact_kind: "fixture".into(),
                artifact_id: "array".into(),
                boundaries: vec![ContinuationPhaseBoundary {
                    phase: "main".into(),
                    function: FunctionId::new(0),
                }],
                chunk_cells: vec![16],
            }),
            ..ContinuationRunOptions::default()
        },
        |_| {},
    )
    .unwrap();
    let metrics = stats.phase_metrics_json(&program).unwrap();
    let requests = metrics["portal"]["requests"].as_array().unwrap();
    assert!(requests.iter().any(|row| row["operation"] == "array_store"));
    assert!(requests.iter().any(|row| row["operation"] == "array_load"));
    assert!(requests.iter().all(|row| row["region"]["kind"] == "global"));
    assert_eq!(stats.array_loads, 1);
    assert_eq!(stats.array_stores, 1);
}

#[test]
fn phase_metrics_attribute_abort_to_active_phase() {
    let program = crate::lower_source("void main() { abort(); }").unwrap();
    let main = program
        .functions()
        .iter()
        .find(|function| function.name() == Some("main"))
        .unwrap();
    let mut input = &[][..];
    let mut output = Vec::new();
    let stats = run_continuations_with_io(
        &program,
        &mut input,
        &mut output,
        ContinuationRunOptions {
            phase_config: Some(ContinuationPhaseConfig {
                artifact_kind: "source".into(),
                artifact_id: "abort".into(),
                boundaries: vec![ContinuationPhaseBoundary {
                    phase: "main".into(),
                    function: main.id(),
                }],
                chunk_cells: vec![16],
            }),
            ..ContinuationRunOptions::default()
        },
        |_| {},
    )
    .unwrap();
    let metrics = stats.phase_metrics_json(&program).unwrap();
    assert!(
        metrics["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["phase"] == "main" && row["kind"] == "abort")
    );
}
