use super::*;

#[test]
fn immediate_aggregate_returns_preserve_subranges_snapshots_and_reused_frames() {
    let main = FunctionId::new(0);
    let wrapper = FunctionId::new(1);
    let producer = FunctionId::new(2);
    let region = AggregateRegion::Frame(crate::FrameAggregateId::new(0));
    let element = |array, index| Address::ArrayElement { array, index };
    let mut input = Vec::new();
    for value in 0..=255u8 {
        input.extend([
            1,
            value,
            value.wrapping_mul(7),
            value.wrapping_sub(1),
            value ^ 173,
        ]);
    }
    input.push(0);
    for shape in 0..5 {
        let (mut body, offset) = if shape == 0 {
            (
                vec![FrameInstruction::AggregateCopy {
                    src: AggregateRegion::Outbox,
                    dst: region,
                    cells: 4,
                }],
                1,
            )
        } else {
            (
                (0..3)
                    .map(|index| FrameInstruction::Copy {
                        src: element(AggregateRegion::Outbox, index + 1),
                        dst: element(region, index + 2),
                    })
                    .collect(),
                2,
            )
        };
        match shape {
            0 | 1 => body.push(FrameInstruction::Copy {
                src: element(region, 2),
                dst: element(region, 2),
            }),
            // The source changes after the snapshot: this cannot be forwarded.
            2 => body.push(FrameInstruction::Set {
                dst: element(AggregateRegion::Outbox, 2),
                value: 77,
            }),
            // A hole in the returned range must retain its prior zero.
            3 => {
                body[1] = FrameInstruction::Copy {
                    src: element(AggregateRegion::Outbox, 2),
                    dst: element(region, 2),
                }
            }
            // Same-region copies have sequential, overlapping semantics.
            _ => body.extend((2..5).map(|index| FrameInstruction::Copy {
                src: element(region, index - 1),
                dst: element(region, index),
            })),
        }
        let program = ContinuationProgram::new(
            main,
            vec![
                FunctionDescriptor::new_aggregates(
                    main,
                    vec![],
                    1,
                    vec![],
                    3,
                    ValueType::Void,
                    id(1),
                ),
                FunctionDescriptor::new_aggregates(
                    wrapper,
                    vec![],
                    0,
                    vec![crate::FrameAggregateDescriptor::new(
                        crate::FrameAggregateId::new(0),
                        6,
                    )],
                    4,
                    ValueType::Aggregate { cells: 3 },
                    id(4),
                ),
                FunctionDescriptor::new_aggregates(
                    producer,
                    vec![],
                    0,
                    vec![crate::FrameAggregateDescriptor::new(
                        crate::FrameAggregateId::new(0),
                        6,
                    )],
                    0,
                    ValueType::Aggregate { cells: 4 },
                    id(6),
                ),
            ],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![FrameInstruction::Input {
                        dst: Address::Frame(FrameSlot::new(0)),
                    }],
                    Terminator::Branch {
                        condition: Address::Frame(FrameSlot::new(0)),
                        then_target: id(2),
                        else_target: id(7),
                    },
                ),
                Continuation::new(
                    id(2),
                    main,
                    vec![],
                    Terminator::Call {
                        callee: wrapper,
                        arguments: vec![],
                        return_to: id(3),
                    },
                ),
                Continuation::new(
                    id(3),
                    main,
                    (0..3)
                        .map(|index| FrameInstruction::Output {
                            src: element(AggregateRegion::Outbox, index),
                        })
                        .collect(),
                    Terminator::Goto { target: id(1) },
                ),
                Continuation::new(
                    id(4),
                    wrapper,
                    vec![],
                    Terminator::Call {
                        callee: producer,
                        arguments: vec![],
                        return_to: id(5),
                    },
                ),
                Continuation::new(
                    id(5),
                    wrapper,
                    body,
                    Terminator::Return {
                        value: Some(ValueOperand::Aggregate {
                            region,
                            offset,
                            cells: 3,
                        }),
                    },
                ),
                Continuation::new(
                    id(6),
                    producer,
                    (1..5)
                        .map(|index| FrameInstruction::Input {
                            dst: element(region, index),
                        })
                        .collect(),
                    Terminator::Return {
                        value: Some(ValueOperand::Aggregate {
                            region,
                            offset: 1,
                            cells: 4,
                        }),
                    },
                ),
                Continuation::new(id(7), main, vec![], Terminator::Halt),
            ],
        )
        .unwrap();
        let mut expected = Vec::new();
        crate::run_continuations_with_io(
            &program,
            &mut &input[..],
            &mut expected,
            Default::default(),
            |_| {},
        )
        .unwrap();
        for options in [
            AbiCodegenOptions::default(),
            AbiCodegenOptions {
                region_emission: false,
                ..Default::default()
            },
            AbiCodegenOptions {
                nibble_transfer: true,
                inplace_compare: true,
                anchor_bank: true,
                ..Default::default()
            },
            AbiCodegenOptions {
                static_frames: true,
                ..Default::default()
            },
        ] {
            let bf = lower_continuations_with_codegen_options(&program, options).unwrap();
            let mut compressed = Vec::new();
            optimize_bf(&bf)
                .write_compressed_source(&mut compressed)
                .unwrap();
            for disabled in [false, true] {
                let result = bf_interpreter::run_with_options(
                    &compressed,
                    &input,
                    bf_interpreter::RunOptions {
                        disable_clear: disabled,
                        disable_scan: disabled,
                        disable_transfer: disabled,
                        disable_countdown: disabled,
                        disable_remote_transfer: disabled,
                        disable_compare: disabled,
                        ..Default::default()
                    },
                )
                .unwrap();
                assert_eq!(
                    result.output, expected,
                    "shape={shape}, options={options:?}, rle_only={disabled}"
                );
            }
        }
    }
}

#[test]
fn binary_cir_aggregate_return_forwarding_keeps_outbox_subranges() {
    use crate::{
        SelfhostCirContinuation as C, SelfhostCirFunction as F, SelfhostCirInstruction as I,
        SelfhostCirProgram as P, SelfhostCirReturnType as R, SelfhostCirTerminator as T,
    };
    let functions = [
        (0, 1, 1, R::Void),
        (1, 4, 5, R::Aggregate(3)),
        (2, 6, 5, R::Aggregate(4)),
    ]
    .into_iter()
    .map(|(id, entry, frame_cells, return_type)| F {
        id,
        entry,
        frame_cells,
        return_type,
        parameters: vec![],
    })
    .collect();
    let source = P::new(
        0,
        0,
        functions,
        vec![
            C {
                id: 1,
                function: 0,
                instructions: vec![I::Input { destination: 0 }],
                terminator: T::Branch {
                    condition: 0,
                    then_target: 2,
                    else_target: 7,
                },
            },
            C {
                id: 2,
                function: 0,
                instructions: vec![],
                terminator: T::Call {
                    callee: 1,
                    arguments: vec![],
                    return_to: 3,
                },
            },
            C {
                id: 3,
                function: 0,
                instructions: (0..3)
                    .flat_map(|index| {
                        [
                            I::CopyOutbox {
                                destination: 0,
                                index,
                            },
                            I::Output { source: 0 },
                        ]
                    })
                    .collect(),
                terminator: T::Goto { target: 1 },
            },
            C {
                id: 4,
                function: 1,
                instructions: vec![],
                terminator: T::Call {
                    callee: 2,
                    arguments: vec![],
                    return_to: 5,
                },
            },
            C {
                id: 5,
                function: 1,
                instructions: (0..3)
                    .map(|index| I::CopyOutbox {
                        destination: index + 2,
                        index: index + 1,
                    })
                    .collect(),
                terminator: T::ReturnAggregate {
                    source: 2,
                    cells: 3,
                },
            },
            C {
                id: 6,
                function: 2,
                instructions: (1..5).map(|destination| I::Input { destination }).collect(),
                terminator: T::ReturnAggregate {
                    source: 1,
                    cells: 4,
                },
            },
            C {
                id: 7,
                function: 0,
                instructions: vec![],
                terminator: T::Halt,
            },
        ],
    )
    .unwrap();
    let decoded = P::decode(&source.encode().unwrap()).unwrap();
    let program = crate::lower_selfhost_cir(&decoded).unwrap();
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for value in 0..=255u8 {
        let returned = [value.wrapping_mul(7), value.wrapping_sub(1), value ^ 173];
        input.extend([1, value]);
        input.extend(returned);
        expected.extend(returned);
    }
    input.push(0);
    for options in [
        AbiCodegenOptions::default(),
        AbiCodegenOptions {
            nibble_transfer: true,
            inplace_compare: true,
            anchor_bank: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            static_frames: true,
            ..Default::default()
        },
    ] {
        let bf = lower_continuations_with_codegen_options(&program, options).unwrap();
        let mut compressed = Vec::new();
        optimize_bf(&bf)
            .write_compressed_source(&mut compressed)
            .unwrap();
        assert_eq!(
            run(&compressed, &input).unwrap(),
            expected,
            "options={options:?}"
        );
    }
}

#[test]
fn direct_recursion_and_caller_locals_survive_with_sixteen_cell_chunks() {
    let program = recursive_countdown_program();
    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), &[4, b'L']);
    }
}

#[test]
fn mutual_recursion_supports_different_frame_sizes() {
    let main = FunctionId::new(0);
    let small = FunctionId::new(1);
    let large = FunctionId::new(2);
    let main_entry = id(1);
    let main_resume = id(2);
    let small_entry = id(3);
    let small_call = id(4);
    let small_base = id(5);
    let small_resume = id(6);
    let large_entry = id(7);
    let large_call = id(8);
    let large_base = id(9);
    let large_resume = id(10);

    let functions = vec![
        FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(
            small,
            vec![FrameSlot::new(0)],
            3,
            crate::ValueType::Cell,
            small_entry,
        ),
        FunctionDescriptor::new(
            large,
            vec![FrameSlot::new(0)],
            12,
            crate::ValueType::Cell,
            large_entry,
        ),
    ];

    let split_and_branch = |function, entry, then_target, else_target, condition, argument| {
        Continuation::new(
            entry,
            function,
            vec![FrameInstruction::Transfer {
                src: Address::Frame(FrameSlot::new(0)),
                targets: vec![
                    FrameTransferTarget {
                        dst: Address::Frame(condition),
                        factor: 1,
                    },
                    FrameTransferTarget {
                        dst: Address::Frame(argument),
                        factor: 1,
                    },
                ],
            }],
            Terminator::Branch {
                condition: Address::Frame(condition),
                then_target,
                else_target,
            },
        )
    };
    let decrement_and_call = |function, continuation, argument, callee, return_to| {
        Continuation::new(
            continuation,
            function,
            vec![FrameInstruction::AddConst {
                dst: Address::Frame(argument),
                value: 255,
            }],
            Terminator::Call {
                callee,
                arguments: vec![ValueOperand::Cell(Address::Frame(argument))],
                return_to,
            },
        )
    };
    let increment_and_return = |function, continuation| {
        Continuation::new(
            continuation,
            function,
            vec![FrameInstruction::AddConst {
                dst: Address::AbiValue,
                value: 1,
            }],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::AbiValue)),
            },
        )
    };

    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![FrameInstruction::Set {
                dst: Address::Frame(FrameSlot::new(0)),
                value: 5,
            }],
            Terminator::Call {
                callee: small,
                arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                return_to: main_resume,
            },
        ),
        Continuation::new(
            main_resume,
            main,
            vec![FrameInstruction::Output {
                src: Address::AbiValue,
            }],
            Terminator::Halt,
        ),
        split_and_branch(
            small,
            small_entry,
            small_call,
            small_base,
            FrameSlot::new(1),
            FrameSlot::new(2),
        ),
        decrement_and_call(small, small_call, FrameSlot::new(2), large, small_resume),
        Continuation::new(
            small_base,
            small,
            vec![],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))),
            },
        ),
        increment_and_return(small, small_resume),
        split_and_branch(
            large,
            large_entry,
            large_call,
            large_base,
            FrameSlot::new(10),
            FrameSlot::new(11),
        ),
        decrement_and_call(large, large_call, FrameSlot::new(11), small, large_resume),
        Continuation::new(
            large_base,
            large,
            vec![],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(11)))),
            },
        ),
        increment_and_return(large, large_resume),
    ];
    let program = ContinuationProgram::new(main, functions, continuations).unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), &[5]);
    }
}

#[test]
fn zero_slot_void_call_clears_the_callers_stale_abi_value() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let main_entry = id(1);
    let main_resume = id(2);
    let callee_entry = id(3);
    let functions = vec![
        FunctionDescriptor::new(main, vec![], 0, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(callee, vec![], 0, crate::ValueType::Void, callee_entry),
    ];
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![FrameInstruction::Set {
                dst: Address::AbiValue,
                value: b'X',
            }],
            Terminator::Call {
                callee,
                arguments: vec![],
                return_to: main_resume,
            },
        ),
        Continuation::new(
            main_resume,
            main,
            vec![FrameInstruction::Output {
                src: Address::AbiValue,
            }],
            Terminator::Halt,
        ),
        Continuation::new(
            callee_entry,
            callee,
            vec![],
            Terminator::Return { value: None },
        ),
    ];
    let program = ContinuationProgram::new(main, functions, continuations).unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), &[0]);
    }
}

#[test]
fn scalar_parameter_and_return_cross_the_d16_value_chunk_boundary() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let main_entry = id(1);
    let main_resume = id(2);
    let callee_entry = id(3);
    let boundary_slot = FrameSlot::new(17);
    let functions = vec![
        FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(
            callee,
            vec![boundary_slot],
            18,
            crate::ValueType::Cell,
            callee_entry,
        ),
    ];
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![FrameInstruction::Set {
                dst: Address::Frame(FrameSlot::new(0)),
                value: b'Q',
            }],
            Terminator::Call {
                callee,
                arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                return_to: main_resume,
            },
        ),
        Continuation::new(
            main_resume,
            main,
            vec![FrameInstruction::Output {
                src: Address::AbiValue,
            }],
            Terminator::Halt,
        ),
        Continuation::new(
            callee_entry,
            callee,
            vec![],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::Frame(boundary_slot))),
            },
        ),
    ];
    let program = ContinuationProgram::new(main, functions, continuations).unwrap();

    assert_eq!(execute_continuations(&program, 16), b"Q");
}

#[test]
fn returned_frame_data_is_zero_when_the_same_frame_is_reused() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let main_entry = id(1);
    let main_after_first_call = id(2);
    let main_after_second_call = id(3);
    let callee_entry = id(4);
    let callee_dirty = id(5);
    let callee_return = id(6);
    let parameter = FrameSlot::new(0);
    let high_local = FrameSlot::new(16);
    let functions = vec![
        FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(
            callee,
            vec![parameter],
            17,
            crate::ValueType::Cell,
            callee_entry,
        ),
    ];
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![FrameInstruction::Set {
                dst: Address::Frame(FrameSlot::new(0)),
                value: 1,
            }],
            Terminator::Call {
                callee,
                arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                return_to: main_after_first_call,
            },
        ),
        Continuation::new(
            main_after_first_call,
            main,
            vec![FrameInstruction::Set {
                dst: Address::Frame(FrameSlot::new(0)),
                value: 0,
            }],
            Terminator::Call {
                callee,
                arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                return_to: main_after_second_call,
            },
        ),
        Continuation::new(
            main_after_second_call,
            main,
            vec![FrameInstruction::Output {
                src: Address::AbiValue,
            }],
            Terminator::Halt,
        ),
        Continuation::new(
            callee_entry,
            callee,
            vec![],
            Terminator::Branch {
                condition: Address::Frame(parameter),
                then_target: callee_dirty,
                else_target: callee_return,
            },
        ),
        Continuation::new(
            callee_dirty,
            callee,
            vec![FrameInstruction::Set {
                dst: Address::Frame(high_local),
                value: b'X',
            }],
            Terminator::Goto {
                target: callee_return,
            },
        ),
        Continuation::new(
            callee_return,
            callee,
            vec![],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::Frame(high_local))),
            },
        ),
    ];
    let program = ContinuationProgram::new(main, functions, continuations).unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), &[0]);
    }
}

#[test]
fn one_caller_address_can_be_copied_to_multiple_parameters() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let main_entry = id(1);
    let main_resume = id(2);
    let callee_entry = id(3);
    let lhs = FrameSlot::new(0);
    let rhs = FrameSlot::new(1);
    let sum = FrameSlot::new(2);
    let functions = vec![
        FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(
            callee,
            vec![lhs, rhs],
            3,
            crate::ValueType::Cell,
            callee_entry,
        ),
    ];
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![FrameInstruction::Set {
                dst: Address::Frame(FrameSlot::new(0)),
                value: 21,
            }],
            Terminator::Call {
                callee,
                arguments: vec![
                    ValueOperand::Cell(Address::Frame(FrameSlot::new(0))),
                    ValueOperand::Cell(Address::Frame(FrameSlot::new(0))),
                ],
                return_to: main_resume,
            },
        ),
        Continuation::new(
            main_resume,
            main,
            vec![FrameInstruction::Output {
                src: Address::AbiValue,
            }],
            Terminator::Halt,
        ),
        Continuation::new(
            callee_entry,
            callee,
            vec![
                FrameInstruction::Transfer {
                    src: Address::Frame(lhs),
                    targets: vec![FrameTransferTarget {
                        dst: Address::Frame(sum),
                        factor: 1,
                    }],
                },
                FrameInstruction::Transfer {
                    src: Address::Frame(rhs),
                    targets: vec![FrameTransferTarget {
                        dst: Address::Frame(sum),
                        factor: 1,
                    }],
                },
            ],
            Terminator::Return {
                value: Some(ValueOperand::Cell(Address::Frame(sum))),
            },
        ),
    ];
    let program = ContinuationProgram::new(main, functions, continuations).unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), &[42]);
    }
}

#[test]
fn aggregate_subranges_cross_call_and_return_in_sixteen_cell_chunks() {
    let main = FunctionId::new(0);
    let helper = FunctionId::new(1);
    let source = crate::FrameAggregateId::new(0);
    let parameter = crate::FrameAggregateId::new(0);
    let functions = vec![
        FunctionDescriptor::new_aggregates(
            main,
            vec![],
            0,
            vec![crate::FrameAggregateDescriptor::new(source, 5)],
            3,
            ValueType::Void,
            id(1),
        ),
        FunctionDescriptor::new_aggregates(
            helper,
            vec![ParameterLocation::Aggregate(parameter)],
            0,
            vec![crate::FrameAggregateDescriptor::new(parameter, 3)],
            0,
            ValueType::Aggregate { cells: 3 },
            id(3),
        ),
    ];
    let continuations = vec![
        Continuation::new(
            id(1),
            main,
            (*b"ABC")
                .into_iter()
                .enumerate()
                .map(|(index, value)| FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: AggregateRegion::Frame(source),
                        index: index + 1,
                    },
                    value,
                })
                .collect(),
            Terminator::Call {
                callee: helper,
                arguments: vec![ValueOperand::Aggregate {
                    region: AggregateRegion::Frame(source),
                    offset: 1,
                    cells: 3,
                }],
                return_to: id(2),
            },
        ),
        Continuation::new(
            id(2),
            main,
            (0..3)
                .map(|index| FrameInstruction::Output {
                    src: Address::ArrayElement {
                        array: AggregateRegion::Outbox,
                        index,
                    },
                })
                .collect(),
            Terminator::Abort,
        ),
        Continuation::new(
            id(3),
            helper,
            vec![],
            Terminator::Return {
                value: Some(ValueOperand::aggregate(
                    AggregateRegion::Frame(parameter),
                    3,
                )),
            },
        ),
    ];
    let program = ContinuationProgram::new(main, functions, continuations).unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"ABC");
    }
}

#[test]
fn direct_global_scalar_and_array_arguments_normalize_to_the_caller() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let scalar = crate::GlobalId::new(0);
    let array = crate::GlobalId::new(1);
    let parameter_array = crate::FrameArrayId::new(0);
    let main_entry = id(1);
    let main_resume = id(2);
    let callee_entry = id(3);
    let functions = vec![
        FunctionDescriptor::new(main, vec![], 0, ValueType::Void, main_entry),
        FunctionDescriptor::new_typed(
            callee,
            vec![
                ParameterLocation::Cell(FrameSlot::new(0)),
                ParameterLocation::Array(parameter_array),
            ],
            1,
            vec![crate::FrameArrayDescriptor::new(parameter_array, 2)],
            0,
            ValueType::Void,
            callee_entry,
        ),
    ];
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Global(scalar),
                    value: b'Q',
                },
                FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: ArrayRegion::Global(array),
                        index: 0,
                    },
                    value: b'A',
                },
                FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: ArrayRegion::Global(array),
                        index: 1,
                    },
                    value: b'B',
                },
            ],
            Terminator::Call {
                callee,
                arguments: vec![
                    ValueOperand::Cell(Address::Global(scalar)),
                    ValueOperand::Array(ArrayRegion::Global(array)),
                ],
                return_to: main_resume,
            },
        ),
        Continuation::new(main_resume, main, vec![], Terminator::Halt),
        Continuation::new(
            callee_entry,
            callee,
            vec![
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(0)),
                },
                FrameInstruction::Output {
                    src: Address::ArrayElement {
                        array: ArrayRegion::Frame(parameter_array),
                        index: 0,
                    },
                },
                FrameInstruction::Output {
                    src: Address::ArrayElement {
                        array: ArrayRegion::Frame(parameter_array),
                        index: 1,
                    },
                },
            ],
            Terminator::Return { value: None },
        ),
    ];
    let program = ContinuationProgram::new_with_globals(
        main,
        vec![
            crate::GlobalDescriptor::cell(scalar),
            crate::GlobalDescriptor::array(array, 2),
        ],
        functions,
        continuations,
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"QAB");
    }
}
