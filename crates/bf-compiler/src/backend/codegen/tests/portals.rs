use super::*;
use crate::Program;
use crate::cell::continuation_adapter::adapt_flat_program;

#[test]
fn portal_ids_avoid_user_ids_and_report_exhaustion() {
    let source = "void main() { cell[2] values; cell i = 1; values[i] = 7; output(values[i]); }";
    let program = crate::lower_source(source).unwrap();
    let user_ids = program
        .continuations()
        .iter()
        .map(|continuation| continuation.id().get())
        .collect::<HashSet<_>>();
    let plan = PortalPlan::new(&program).unwrap();
    let mut hidden = Vec::new();
    hidden.extend(plan.accessors.iter().map(|accessor| accessor.id.get()));
    hidden.extend(plan.ordered_sites.iter().map(|site| site.resume.get()));
    assert!(hidden.iter().all(|id| !user_ids.contains(id)));
    assert_eq!(
        hidden.iter().copied().collect::<HashSet<_>>().len(),
        hidden.len()
    );

    let mut exhausted = (1..=u16::MAX).collect::<HashSet<_>>();
    assert_eq!(
        allocate_hidden_id(&mut exhausted),
        Err(AbiCodegenError::ContinuationIdsExhausted)
    );
}

#[test]
fn version_one_dynamic_multi_cell_access_uses_16_bit_offsets() {
    let main = FunctionId::new(0);
    let root = crate::FrameAggregateId::new(0);
    let low = FrameSlot::new(0);
    let high = FrameSlot::new(1);
    let function = FunctionDescriptor::new_aggregates(
        main,
        vec![],
        2,
        vec![crate::FrameAggregateDescriptor::new(root, 300)],
        0,
        ValueType::Void,
        id(1),
    );
    let offset = LogicalOffset::new(Address::Frame(low), Address::Frame(high));
    let continuations = vec![
        Continuation::new(
            id(1),
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: AggregateRegion::Frame(root),
                        index: 254,
                    },
                    value: 10,
                },
                FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: AggregateRegion::Frame(root),
                        index: 255,
                    },
                    value: 20,
                },
                FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: AggregateRegion::Frame(root),
                        index: 256,
                    },
                    value: 30,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(low),
                    value: 254,
                },
            ],
            Terminator::AggregateLoad {
                source: AggregateRegion::Frame(root),
                offset,
                destination: ValueOperand::Aggregate {
                    region: AggregateRegion::Frame(root),
                    offset: 255,
                    cells: 3,
                },
                cells: 3,
                return_to: id(2),
            },
        ),
        Continuation::new(
            id(2),
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(low),
                    value: 0,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(high),
                    value: 1,
                },
            ],
            Terminator::AggregateStore {
                destination: AggregateRegion::Frame(root),
                offset,
                source: ValueOperand::Aggregate {
                    region: AggregateRegion::Frame(root),
                    offset: 255,
                    cells: 3,
                },
                cells: 3,
                return_to: id(3),
            },
        ),
        Continuation::new(
            id(3),
            main,
            [254, 255, 256, 257, 258]
                .into_iter()
                .map(|index| FrameInstruction::Output {
                    src: Address::ArrayElement {
                        array: AggregateRegion::Frame(root),
                        index,
                    },
                })
                .collect(),
            Terminator::Halt,
        ),
    ];
    let program = ContinuationProgram::new(main, vec![function], continuations).unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(
            execute_continuations(&program, chunk_cells),
            &[10, 10, 10, 20, 30]
        );
    }
}

#[test]
fn nibble_countdown_matches_binary_decomposition_for_every_byte() {
    let program = adapt_flat_program(&Program::new(1, vec![]).unwrap()).unwrap();
    let config = AbiConfig::new(16).unwrap();
    let layouts = build_layouts(&program, config).unwrap();
    let static_layout = StaticLayout::new(config, program.globals()).unwrap();
    let portal = PortalPlan::new(&program).unwrap();
    let make_source = |countdown| {
        let mut emitter = AbiEmitter::new(
            &program,
            &layouts,
            &static_layout,
            &portal,
            config,
            ProfileGranularity::Abi,
        );
        let bits = emitter
            .abi_field_offsets([
                AbiField::Index,
                AbiField::Condition,
                AbiField::Restore,
                AbiField::Scratch0,
                AbiField::Scratch1,
                AbiField::Scratch2,
                AbiField::Scratch3,
                AbiField::NextPcLow,
            ])
            .unwrap();
        // Dirty destinations check that the operation replaces prior values.
        emitter.set_abi_field(AbiField::Index, 173).unwrap();
        emitter.set_abi_field(AbiField::Scratch1, 219).unwrap();
        let source = emitter.current_abi_offset(AbiField::PcLow).unwrap();
        emitter.move_to(source);
        emitter.emit_operation(AnnotatedBfOperation::Input);
        if countdown {
            emitter
                .split_abi_nibbles(AbiField::PcLow, AbiField::Index, AbiField::Scratch1)
                .unwrap();
        } else {
            let temporary = emitter.current_abi_offset(AbiField::Branch).unwrap();
            emitter
                .decompose_abi_byte(AbiField::PcLow, &bits, temporary)
                .unwrap();
            for index in 1..4 {
                emitter.move_static_value(bits[index], bits[0], 1 << index);
            }
            for index in 5..8 {
                emitter.move_static_value(bits[index], bits[4], 1 << (index - 4));
            }
        }
        for offset in [bits[0], bits[4], source] {
            emitter.move_to(offset);
            emitter.emit_operation(AnnotatedBfOperation::Output);
        }
        optimize_annotated_bf(&AnnotatedBfProgram::new(emitter.output, emitter.sites)).to_source()
    };
    let old = make_source(false);
    let new = make_source(true);
    let mut before = 0;
    let mut after = 0;
    for value in 0..=u8::MAX {
        let reference = run_with_stats(old.as_bytes(), &[value]).unwrap();
        let actual = run_with_stats(new.as_bytes(), &[value]).unwrap();
        assert_eq!(actual.output, [value % 16, value / 16, 0], "{value}");
        assert_eq!(actual.output, reference.output);
        before += reference.stats.optimization.executed_native_operations;
        after += actual.stats.optimization.executed_native_operations;
    }
    eprintln!(
        "nibble split: native operations {before} -> {after}; BF bytes {} -> {}",
        old.len(),
        new.len()
    );
    assert!(after * 2 < before);
}

#[test]
fn dynamic_portal_offset_preserves_high_quotient_bits() {
    let main = FunctionId::new(0);
    let root = crate::FrameAggregateId::new(0);
    let low = FrameSlot::new(0);
    let high = FrameSlot::new(1);
    let destination = FrameSlot::new(2);
    let function = FunctionDescriptor::new_aggregates(
        main,
        vec![],
        3,
        vec![crate::FrameAggregateDescriptor::new(root, 8192)],
        0,
        ValueType::Void,
        id(1),
    );
    let continuations = vec![
        Continuation::new(
            id(1),
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: AggregateRegion::Frame(root),
                        index: 8191,
                    },
                    value: b'Q',
                },
                FrameInstruction::Set {
                    dst: Address::Frame(low),
                    value: 255,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(high),
                    value: 31,
                },
            ],
            Terminator::AggregateLoad {
                source: AggregateRegion::Frame(root),
                offset: LogicalOffset::new(Address::Frame(low), Address::Frame(high)),
                destination: ValueOperand::Cell(Address::Frame(destination)),
                cells: 1,
                return_to: id(2),
            },
        ),
        Continuation::new(
            id(2),
            main,
            vec![FrameInstruction::Output {
                src: Address::Frame(destination),
            }],
            Terminator::Halt,
        ),
    ];
    let program = ContinuationProgram::new(main, vec![function], continuations).unwrap();

    let result = execute_continuations_with_stats(&program, 16);
    assert_eq!(result.output, b"Q");
    assert!(result.stats.optimization.executed_native_operations < 40_000);
}

#[test]
fn jumping_global_portal_restores_intermediate_payload_chunks() {
    let main = FunctionId::new(0);
    let global = crate::GlobalId::new(0);
    let low = FrameSlot::new(0);
    let high = FrameSlot::new(1);
    let value = FrameSlot::new(2);
    let destination = FrameSlot::new(3);
    let function = FunctionDescriptor::new(main, vec![], 4, ValueType::Void, id(1));
    let region = AggregateRegion::Global(global);
    let offset = LogicalOffset::new(Address::Frame(low), Address::Frame(high));
    let sentinels = [(0, b'A'), (15, b'B'), (16, b'C'), (255, b'D'), (4095, b'E')];
    let mut setup = sentinels
        .into_iter()
        .map(|(index, value)| FrameInstruction::Set {
            dst: Address::ArrayElement {
                array: region,
                index,
            },
            value,
        })
        .collect::<Vec<_>>();
    setup.extend([
        FrameInstruction::Set {
            dst: Address::Frame(low),
            value: 255,
        },
        FrameInstruction::Set {
            dst: Address::Frame(high),
            value: 31,
        },
        FrameInstruction::Set {
            dst: Address::Frame(value),
            value: b'Q',
        },
    ]);
    let continuations = vec![
        Continuation::new(
            id(1),
            main,
            setup,
            Terminator::AggregateStore {
                destination: region,
                offset,
                source: ValueOperand::Cell(Address::Frame(value)),
                cells: 1,
                return_to: id(2),
            },
        ),
        Continuation::new(
            id(2),
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(low),
                    value: 255,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(high),
                    value: 31,
                },
            ],
            Terminator::AggregateLoad {
                source: region,
                offset,
                destination: ValueOperand::Cell(Address::Frame(destination)),
                cells: 1,
                return_to: id(3),
            },
        ),
        Continuation::new(
            id(3),
            main,
            sentinels
                .into_iter()
                .map(|(index, _)| FrameInstruction::Output {
                    src: Address::ArrayElement {
                        array: region,
                        index,
                    },
                })
                .chain(std::iter::once(FrameInstruction::Output {
                    src: Address::Frame(destination),
                }))
                .collect(),
            Terminator::Halt,
        ),
    ];
    let program = ContinuationProgram::new_with_globals(
        main,
        vec![crate::GlobalDescriptor::aggregate(global, 8192)],
        vec![function],
        continuations,
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(
            execute_continuations(&program, chunk_cells),
            b"ABCDEQ",
            "D={chunk_cells}"
        );
    }
}

#[test]
fn version_zero_global_portal_reaches_index_255() {
    let main = FunctionId::new(0);
    let index = FrameSlot::new(0);
    let value = FrameSlot::new(1);
    let function = FunctionDescriptor::new(main, vec![], 2, ValueType::Void, id(1));
    let continuations = vec![
        Continuation::new(
            id(1),
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(index),
                    value: 255,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(value),
                    value: b'Z',
                },
            ],
            Terminator::ArrayStore {
                array: AggregateRegion::Global(crate::GlobalId::new(0)),
                index: Address::Frame(index),
                value: Address::Frame(value),
                return_to: id(2),
            },
        ),
        Continuation::new(
            id(2),
            main,
            vec![FrameInstruction::Output {
                src: Address::ArrayElement {
                    array: AggregateRegion::Global(crate::GlobalId::new(0)),
                    index: 255,
                },
            }],
            Terminator::Halt,
        ),
    ];
    let program = ContinuationProgram::new_with_globals(
        main,
        vec![crate::GlobalDescriptor::array(crate::GlobalId::new(0), 256)],
        vec![function],
        continuations,
    )
    .unwrap();
    let portal = PortalPlan::new(&program).unwrap();
    let encoding = DispatchEncoding::new(&program, &portal);
    assert_eq!(portal.ordered_sites[0].resume.get(), 5);
    assert_eq!(encoding.encode(id(1)), 5);
    assert_eq!(encoding.encode(portal.ordered_sites[0].resume), 1);
    {
        let chunk_cells = 16;
        let layouts = build_layouts(&program, AbiConfig::new(chunk_cells).unwrap()).unwrap();
        assert_eq!(layouts[&main].frame.route_chunks(), 1, "D={chunk_cells}");
        assert_eq!(execute_continuations(&program, chunk_cells), b"Z");
        let mut dense = program.continuations().to_vec();
        dense.extend(
            (3..=1024).map(|value| Continuation::new(id(value), main, vec![], Terminator::Halt)),
        );
        let dense = ContinuationProgram::new_with_globals(
            main,
            program.globals().to_vec(),
            program.functions().to_vec(),
            dense,
        )
        .unwrap();
        assert_eq!(execute_continuations(&dense, chunk_cells), b"Z");
    }
}

#[test]
fn version_one_source_accessor_plan_stays_compact() {
    let program = crate::lower_source(
        "struct Triple { cell a; cell b; cell c; } Triple[100] global_values; void main() { Triple[100] local_values; cell index = 85; global_values[index].b = 'G'; local_values[index] = global_values[index]; output(local_values[index].a); output(local_values[index].b); output(local_values[index].c); }",
    )
    .unwrap();
    let plan = PortalPlan::new(&program).unwrap();
    assert_eq!(plan.accessors.len(), 2);
    assert!(plan.ordered_sites.len() < 16);
    {
        let chunk_cells = 16;
        let generated =
            lower_continuations_with_config(&program, AbiConfig::new(chunk_cells).unwrap())
                .unwrap()
                .to_source();
        assert!(
            generated.len() < 5_000_000,
            "D={chunk_cells}: {} bytes, {} portal leaves",
            generated.len(),
            plan.ordered_sites.len()
        );
    }
}
