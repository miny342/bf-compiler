//! Shared execution helpers, differential measurements, and fixed fixtures.
use bf_interpreter::{ProfileMode, ProfileOptions, RunOptions, RunResult, run, run_with_stats};

use super::super::*;
use crate::backend::layout_plan::build_layouts;
use crate::cell::continuation_adapter::adapt_flat_program;
use crate::{Instruction, Program};

pub(super) fn execute_flat(
    instructions: Vec<Instruction>,
    cells: usize,
    config: AbiConfig,
) -> Vec<u8> {
    let flat = Program::new(cells, instructions).unwrap();
    let continuations = adapt_flat_program(&flat).unwrap();
    let bf = lower_continuations_with_config(&continuations, config)
        .unwrap()
        .to_source();
    run(bf.as_bytes(), b"").unwrap()
}

pub(super) fn id(value: u16) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}

pub(super) fn execute_continuations(program: &ContinuationProgram, chunk_cells: usize) -> Vec<u8> {
    execute_continuations_with_stats(program, chunk_cells).output
}

pub(super) fn execute_continuations_with_stats(
    program: &ContinuationProgram,
    chunk_cells: usize,
) -> RunResult {
    let source = lower_continuations_with_config(program, AbiConfig::new(chunk_cells).unwrap())
        .unwrap()
        .to_source();
    run_with_stats(source.as_bytes(), b"").unwrap()
}

pub(super) fn recursive_countdown_program() -> ContinuationProgram {
    let main = FunctionId::new(0);
    let countdown = FunctionId::new(1);
    let main_entry = id(1);
    let main_resume = id(2);
    let countdown_entry = id(3);
    let countdown_recurse = id(4);
    let countdown_base = id(5);
    let countdown_resume = id(6);

    let functions = vec![
        FunctionDescriptor::new(main, vec![], 2, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(
            countdown,
            vec![FrameSlot::new(0)],
            3,
            crate::ValueType::Cell,
            countdown_entry,
        ),
    ];
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 4,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(1)),
                    value: b'L',
                },
            ],
            Terminator::Call {
                callee: countdown,
                arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                return_to: main_resume,
            },
        ),
        Continuation::new(
            main_resume,
            main,
            vec![
                FrameInstruction::Output {
                    src: Address::AbiValue,
                },
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(1)),
                },
            ],
            Terminator::Halt,
        ),
        Continuation::new(
            countdown_entry,
            countdown,
            vec![FrameInstruction::Transfer {
                src: Address::Frame(FrameSlot::new(0)),
                targets: vec![
                    FrameTransferTarget {
                        dst: Address::Frame(FrameSlot::new(1)),
                        factor: 1,
                    },
                    FrameTransferTarget {
                        dst: Address::Frame(FrameSlot::new(2)),
                        factor: 1,
                    },
                ],
            }],
            Terminator::Branch {
                condition: Address::Frame(FrameSlot::new(1)),
                then_target: countdown_recurse,
                else_target: countdown_base,
            },
        ),
        Continuation::new(
            countdown_recurse,
            countdown,
            vec![FrameInstruction::AddConst {
                dst: Address::Frame(FrameSlot::new(2)),
                value: 255,
            }],
            Terminator::Call {
                callee: countdown,
                arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))],
                return_to: countdown_resume,
            },
        ),
        Continuation::new(
            countdown_base,
            countdown,
            vec![],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))),
            },
        ),
        Continuation::new(
            countdown_resume,
            countdown,
            vec![FrameInstruction::AddConst {
                dst: Address::AbiValue,
                value: 1,
            }],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::AbiValue)),
            },
        ),
    ];
    ContinuationProgram::new(main, functions, continuations).unwrap()
}

pub(super) struct Measurement {
    pub(super) output: Vec<u8>,
    pub(super) visits: HashMap<ContinuationId, u64>,
    pub(super) hidden_visits: u64,
    pub(super) bytes: usize,
    pub(super) instructions: u64,
    pub(super) rle_instructions: u64,
    pub(super) selector_instructions: u64,
    pub(super) selector_rle_instructions: u64,
    pub(super) semantic: crate::ContinuationRunStats,
}

pub(super) fn measure(program: &ContinuationProgram, input: &[u8], enabled: bool) -> Measurement {
    let mut expected = Vec::new();
    let semantic = crate::run_continuations_with_io(
        program,
        &mut &input[..],
        &mut expected,
        crate::ContinuationRunOptions {
            collect_transitions: true,
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    let annotated = lower_continuations_annotated_with_options(
        program,
        AbiConfig::default(),
        ProfileGranularity::Source,
        AbiCodegenOptions {
            region_emission: enabled,
            ..Default::default()
        },
    )
    .unwrap();
    let artifact = optimize_annotated_bf(&annotated).profile_artifact(false);
    let result = bf_interpreter::run_with_options(
        artifact.source.as_bytes(),
        input,
        RunOptions {
            collect_stats: true,
            profile: Some(ProfileOptions {
                map: artifact.map.clone(),
                mode: ProfileMode::Exact,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.output, expected, "region emission = {enabled}");
    // The ordinary optimizer and interpreter must agree with annotated output.
    let plain = optimize_bf(&annotated.into_plain()).to_source();
    assert_eq!(
        artifact.source, plain,
        "profiling must not change generated BF"
    );
    let unprofiled = bf_interpreter::run_with_options(
        plain.as_bytes(),
        input,
        RunOptions {
            collect_stats: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(unprofiled.output, expected);
    assert_eq!(
        unprofiled.stats.executed_instructions,
        result.stats.executed_instructions
    );
    assert_eq!(
        unprofiled.stats.executed_rle_instructions,
        result.stats.executed_rle_instructions
    );

    let plan = enabled.then(|| RegionPlan::new(program));
    let mut visits = HashMap::new();
    let mut terminals = HashMap::<ContinuationId, u64>::new();
    let mut hidden_visits = 0;
    let mut inputs = 0;
    let mut selector_instructions = 0;
    let mut selector_rle_instructions = 0;
    for site in &result.profile.as_ref().unwrap().sites {
        let key = &artifact
            .map
            .sites
            .iter()
            .find(|s| s.id == site.site)
            .unwrap()
            .stable_key;
        inputs += site.counters.input_operations;
        if key == "abi.region.select" || key.starts_with("abi.region.enter.") {
            selector_instructions += site.counters.raw_bf_instructions;
            selector_rle_instructions += site.counters.rle_instructions;
        }
        if let Some(id) = key.strip_prefix("abi.dispatch.enter.") {
            let id = ContinuationId::new(id.parse().unwrap()).unwrap();
            if program.continuation(id).is_some() {
                *visits.entry(id).or_default() += site.counters.loop_iterations;
                if plan.as_ref().is_none_or(|p| !p.regions.contains_key(&id)) {
                    *terminals.entry(id).or_default() += site.counters.loop_iterations;
                }
            } else {
                hidden_visits += site.counters.loop_iterations;
            }
        }
        if let Some(id) = key.strip_prefix("abi.region.enter.") {
            let id = ContinuationId::new(id.parse().unwrap()).unwrap();
            *terminals.entry(id).or_default() += site.counters.loop_iterations;
        }
    }
    assert_eq!(inputs, semantic.input_operations, "input consumption");
    for continuation in program.continuations() {
        if continuation.terminator().boundary() != crate::cir::ir::BoundaryKind::Soft {
            let id = continuation.id();
            assert_eq!(
                terminals.get(&id).copied().unwrap_or(0),
                semantic.continuation_count(id),
                "terminal at {id:?}, regions={enabled}"
            );
        }
    }
    Measurement {
        output: result.output,
        visits,
        hidden_visits,
        bytes: plain.len(),
        instructions: result.stats.executed_instructions,
        rle_instructions: result.stats.executed_rle_instructions,
        selector_instructions,
        selector_rle_instructions,
        semantic,
    }
}

pub(super) fn transport_fixture(
    padding: usize,
    nibble_transfer: bool,
) -> (CompiledProfileArtifact, usize) {
    // Keep each actual frame within the ABI limit. Additional live flag
    // chunks model suspended callers, not one oversized activation.
    let frame_padding = padding.min(256);
    let extra_chunks = (padding - frame_padding) / crate::DEFAULT_CHUNK_CELLS;
    let declaration = format!("cell[{frame_padding}] keep;");
    let first = "keep[0]";
    let last = format!("keep[{}]", frame_padding - 1);
    let source = format!(
        "cell[16] data; void main() {{ {declaration} \
         {first} = input(); {last} = input(); \
         cell i = input(); data[i] = input(); output(data[i]); \
         output({first}); output({last}); }}"
    );
    let program = crate::lower_source(&source).unwrap();
    let config = AbiConfig::default();
    let layouts = build_layouts(&program, config).unwrap();
    let static_layout = StaticLayout::new(config, program.globals()).unwrap();
    let portal = PortalPlan::new(&program).unwrap();
    let mut emitter = AbiEmitter::new(
        &program,
        &layouts,
        &static_layout,
        &portal,
        config,
        ProfileGranularity::Continuation,
    );
    emitter.nibble_transfer = nibble_transfer;
    emitter.initialize_main(false).unwrap();
    emitter.with_profile_site_infallible(
        "fixture",
        "fixture.stack",
        "live caller chunks",
        |emitter| {
            for chunk in 1..=extra_chunks {
                emitter.set((chunk * config.stride()) as isize, 1);
            }
            emitter.migrate_context((extra_chunks * config.stride()) as isize);
        },
    );
    let condition = emitter.current_abi_offset(AbiField::Active).unwrap();
    emitter.move_to(condition);
    emitter.emit_operation(AnnotatedBfOperation::Input);
    let body = emitter
        .capture(|emitter| {
            for route in 0..7 {
                let Location::Relative(offset) = emitter.route_location(route)? else {
                    unreachable!()
                };
                emitter.move_to(offset);
                emitter.emit_operation(AnnotatedBfOperation::Input);
            }
            emitter.move_to(0);
            emitter.with_profile_site(
                "abi",
                "abi.portal.router.global.0",
                "global portal router",
                |emitter| {
                    emitter.move_global_portal_request(AggregateRegion::Global(GlobalId::new(0)))
                },
            )?;
            // Observe every transferred request byte, then restore the zero-prefix
            // contract. No dispatch interprets these controlled values as real PCs.
            emitter.with_profile_site(
                "fixture",
                "fixture.check",
                "observe request and cleanup",
                |emitter| {
                    for field in [
                        AbiField::Index,
                        AbiField::Scratch0,
                        AbiField::Value,
                        AbiField::NextPcLow,
                        AbiField::NextPcHigh,
                        AbiField::ReturnPcLow,
                        AbiField::ReturnPcHigh,
                    ] {
                        let location = emitter.portal_field_location(
                            AggregateRegion::Global(GlobalId::new(0)),
                            field,
                            program.main(),
                        )?;
                        emitter.move_context_to_location(location);
                        emitter.emit_operation(AnnotatedBfOperation::Output);
                        emitter.clear_current();
                        emitter.move_location_to_context(location);
                    }
                    emitter.move_to(condition);
                    emitter.emit_operation(AnnotatedBfOperation::Input);
                    Ok(())
                },
            )
        })
        .unwrap();
    emitter.emit_loop(body);
    let annotated = AnnotatedBfProgram::new(emitter.output, emitter.sites);
    let mut plain = Vec::new();
    optimize_bf(&annotated.clone().into_plain())
        .write_compressed_source(&mut plain)
        .unwrap();
    let artifact = optimize_annotated_bf(&annotated).profile_artifact(true);
    assert_eq!(
        artifact.source.as_bytes(),
        plain,
        "profile labels must not change optimized BF"
    );
    (
        artifact,
        layouts[&program.main()].frame.frame_chunks() + extra_chunks,
    )
}
