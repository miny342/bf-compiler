use super::*;

#[test]
fn preserving_truth_copies_reuse_guards_and_keep_destructive_branch_semantics() {
    use FrameInstruction as I;
    let f = FunctionId::new(0);
    let entry = id(1);
    let slot = |i| Address::Frame(FrameSlot::new(i));
    let body = vec![
        I::Input { dst: slot(7) },
        I::Loop {
            condition: slot(7),
            body: vec![
                I::Input { dst: slot(0) },
                I::Input { dst: slot(4) },
                I::Set {
                    dst: slot(1),
                    value: 255,
                },
                I::Copy {
                    src: slot(0),
                    dst: slot(1),
                },
                I::Branch {
                    condition: slot(1),
                    then_body: vec![
                        I::Output { src: slot(1) },
                        I::Output { src: slot(0) },
                        I::Copy {
                            src: slot(0),
                            dst: slot(2),
                        },
                        I::Branch {
                            condition: slot(2),
                            then_body: vec![
                                I::Output { src: slot(2) },
                                I::Set {
                                    dst: slot(2),
                                    value: 7,
                                },
                                I::AddConst {
                                    dst: slot(0),
                                    value: 1,
                                },
                            ],
                            else_body: vec![I::AddConst {
                                dst: slot(0),
                                value: 2,
                            }],
                        },
                    ],
                    else_body: vec![
                        I::Output { src: slot(1) },
                        I::Output { src: slot(0) },
                        I::Copy {
                            src: slot(4),
                            dst: slot(3),
                        },
                        I::Branch {
                            condition: slot(3),
                            then_body: vec![I::AddConst {
                                dst: slot(0),
                                value: 3,
                            }],
                            else_body: vec![I::AddConst {
                                dst: slot(0),
                                value: 4,
                            }],
                        },
                    ],
                },
                I::Output { src: slot(1) },
                I::Output { src: slot(2) },
                I::Output { src: slot(3) },
                I::Output { src: slot(0) },
                I::Output { src: slot(4) },
                // A self-copy must still consume its original condition.
                I::Copy {
                    src: slot(4),
                    dst: slot(4),
                },
                I::Branch {
                    condition: slot(4),
                    then_body: vec![
                        I::Output { src: slot(4) },
                        I::Set {
                            dst: slot(4),
                            value: 93,
                        },
                    ],
                    else_body: vec![],
                },
                I::Output { src: slot(4) },
                // Comparison guards must be clean for subsequent reuse.
                I::Compare {
                    left: slot(0),
                    right: slot(4),
                    dst: slot(6),
                    true_value: 1,
                    false_value: 0,
                },
                I::Output { src: slot(6) },
                I::Input { dst: slot(7) },
            ],
        },
    ];
    let program = |body| {
        ContinuationProgram::new(
            f,
            vec![FunctionDescriptor::new(
                f,
                vec![],
                8,
                ValueType::Void,
                entry,
            )],
            vec![Continuation::new(entry, f, body, Terminator::Halt)],
        )
        .unwrap()
    };
    let p = program(body.clone());
    let mut input = Vec::new();
    for value in 0..=255u8 {
        for other in [0, 1, 128, 255] {
            input.extend([1, value, other]);
        }
    }
    input.push(0);
    let mut expected = Vec::new();
    crate::run_continuations_with_io(
        &p,
        &mut input.as_slice(),
        &mut expected,
        Default::default(),
        |_| {},
    )
    .unwrap();
    let mut optimized_rle = 0;
    for options in [
        AbiCodegenOptions::default(),
        AbiCodegenOptions {
            inplace_compare: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            inplace_compare: true,
            nibble_transfer: true,
            anchor_bank: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            inplace_compare: true,
            static_frames: true,
            ..Default::default()
        },
    ] {
        let bf =
            crate::optimize_bf(&lower_continuations_with_codegen_options(&p, options).unwrap())
                .to_source();
        let result = run_with_stats(bf.as_bytes(), &input).unwrap();
        assert_eq!(result.output, expected, "options={options:?}");
        if options.inplace_compare && !options.nibble_transfer && !options.static_frames {
            optimized_rle = result.stats.executed_rle_instructions;
        }
    }
    // A zero adjustment prevents adjacency recognition but is removed by BF
    // optimization. This retains the old preserving-copy route for comparison.
    fn separate(body: Vec<I>) -> Vec<I> {
        let mut output = Vec::new();
        let mut iter = body.into_iter().peekable();
        while let Some(i) = iter.next() {
            let barrier = match (&i, iter.peek()) {
                (I::Copy { dst, .. }, Some(I::Branch { condition, .. })) if dst == condition => {
                    Some(*dst)
                }
                _ => None,
            };
            output.push(match i {
                I::Loop { condition, body } => I::Loop {
                    condition,
                    body: separate(body),
                },
                I::Branch {
                    condition,
                    then_body,
                    else_body,
                } => I::Branch {
                    condition,
                    then_body: separate(then_body),
                    else_body: separate(else_body),
                },
                other => other,
            });
            if let Some(dst) = barrier {
                output.push(I::AddConst { dst, value: 0 });
            }
        }
        output
    }
    let original_route = program(separate(body));
    let bf = crate::optimize_bf(
        &lower_continuations_with_codegen_options(
            &original_route,
            AbiCodegenOptions {
                inplace_compare: true,
                ..Default::default()
            },
        )
        .unwrap(),
    )
    .to_source();
    let result = run_with_stats(bf.as_bytes(), &input).unwrap();
    assert_eq!(result.output, expected);
    assert!(optimized_rle < result.stats.executed_rle_instructions / 2);
}

#[test]
fn main_only_adapter_runs_with_sixteen_cell_chunks() {
    let cell = CellId::new(0);
    let instructions = vec![
        Instruction::Set {
            dst: cell,
            value: b'A',
        },
        Instruction::Output { src: cell },
    ];
    {
        let chunk_cells = 16;
        assert_eq!(
            execute_flat(
                instructions.clone(),
                1,
                AbiConfig::new(chunk_cells).unwrap()
            ),
            b"A"
        );
    }
}

#[test]
fn structured_branches_use_frame_temporaries() {
    let condition = CellId::new(0);
    let nested_condition = CellId::new(1);
    let value = CellId::new(2);
    let instructions = vec![
        Instruction::Set {
            dst: condition,
            value: 1,
        },
        Instruction::Branch {
            condition,
            then_body: vec![Instruction::Branch {
                condition: nested_condition,
                then_body: vec![],
                else_body: vec![Instruction::Set {
                    dst: value,
                    value: b'B',
                }],
            }],
            else_body: vec![],
        },
        Instruction::Output { src: value },
    ];
    assert_eq!(execute_flat(instructions, 3, AbiConfig::default()), b"B");
}

#[test]
fn empty_else_branches_need_no_flag_and_still_consume_the_condition() {
    let condition = CellId::new(0);
    let value = CellId::new(1);
    let instructions = vec![
        Instruction::Set {
            dst: value,
            value: b'Z',
        },
        Instruction::Branch {
            condition,
            then_body: vec![Instruction::Set {
                dst: value,
                value: b'X',
            }],
            else_body: vec![],
        },
        Instruction::Output { src: value },
        Instruction::Set {
            dst: condition,
            value: 2,
        },
        Instruction::Branch {
            condition,
            then_body: vec![
                Instruction::Set {
                    dst: condition,
                    value: 9,
                },
                Instruction::Set {
                    dst: value,
                    value: b'T',
                },
            ],
            else_body: vec![],
        },
        Instruction::Output { src: value },
        Instruction::Output { src: condition },
    ];

    let empty_else = FrameInstruction::Branch {
        condition: Address::Frame(FrameSlot::new(0)),
        then_body: vec![],
        else_body: vec![],
    };
    assert_eq!(maximum_branch_depth(&[empty_else]), 0);

    {
        let chunk_cells = 16;
        assert_eq!(
            execute_flat(
                instructions.clone(),
                2,
                AbiConfig::new(chunk_cells).unwrap()
            ),
            &[b'Z', b'T', 0]
        );
    }
}

#[test]
fn global_addresses_work_in_transfer_loop_and_branch_for_sixteen_cell_chunks() {
    let main = FunctionId::new(0);
    let global = crate::GlobalId::new(0);
    let entry = id(1);
    let slot = FrameSlot::new(0);
    let function = FunctionDescriptor::new(main, vec![], 1, ValueType::Void, entry);
    let continuation = Continuation::new(
        entry,
        main,
        vec![
            FrameInstruction::Set {
                dst: Address::Global(global),
                value: 2,
            },
            FrameInstruction::Loop {
                condition: Address::Global(global),
                body: vec![
                    FrameInstruction::Output {
                        src: Address::Global(global),
                    },
                    FrameInstruction::AddConst {
                        dst: Address::Global(global),
                        value: 255,
                    },
                ],
            },
            FrameInstruction::Set {
                dst: Address::Frame(slot),
                value: 3,
            },
            FrameInstruction::Transfer {
                src: Address::Frame(slot),
                targets: vec![FrameTransferTarget {
                    dst: Address::Global(global),
                    factor: 1,
                }],
            },
            FrameInstruction::Transfer {
                src: Address::Global(global),
                targets: vec![FrameTransferTarget {
                    dst: Address::Frame(slot),
                    factor: 1,
                }],
            },
            FrameInstruction::Output {
                src: Address::Frame(slot),
            },
            FrameInstruction::Set {
                dst: Address::Global(global),
                value: 1,
            },
            FrameInstruction::Branch {
                condition: Address::Global(global),
                then_body: vec![FrameInstruction::Set {
                    dst: Address::Frame(slot),
                    value: b'B',
                }],
                else_body: vec![FrameInstruction::Set {
                    dst: Address::Frame(slot),
                    value: b'X',
                }],
            },
            FrameInstruction::Output {
                src: Address::Frame(slot),
            },
        ],
        Terminator::Halt,
    );
    let program = ContinuationProgram::new_with_globals(
        main,
        vec![crate::GlobalDescriptor::cell(global)],
        vec![function],
        vec![continuation],
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(
            execute_continuations(&program, chunk_cells),
            &[2, 1, 3, b'B']
        );
    }
}

#[test]
fn global_to_frame_copy_preserves_every_value_in_sixteen_cell_chunks() {
    let main = FunctionId::new(0);
    let global = crate::GlobalId::new(0);
    let entry = id(1);
    let slot = FrameSlot::new(0);
    let function = FunctionDescriptor::new(main, vec![], 1, ValueType::Void, entry);
    let mut body = Vec::new();
    for value in 0..=u8::MAX {
        body.extend([
            FrameInstruction::Set {
                dst: Address::Global(global),
                value,
            },
            FrameInstruction::Set {
                dst: Address::Frame(slot),
                value: 99,
            },
            FrameInstruction::Copy {
                src: Address::Global(global),
                dst: Address::Frame(slot),
            },
            FrameInstruction::Output {
                src: Address::Frame(slot),
            },
            FrameInstruction::Output {
                src: Address::Global(global),
            },
        ]);
    }
    let program = ContinuationProgram::new_with_globals(
        main,
        vec![crate::GlobalDescriptor::cell(global)],
        vec![function],
        vec![Continuation::new(entry, main, body, Terminator::Halt)],
    )
    .unwrap();

    let expected = (0..=u8::MAX)
        .flat_map(|value| [value, value])
        .collect::<Vec<_>>();
    {
        let chunk_cells = 16;
        assert_eq!(
            execute_continuations(&program, chunk_cells),
            expected,
            "D={chunk_cells}"
        );
    }
}

#[test]
fn aggregate_self_copy_is_a_no_op() {
    let main = FunctionId::new(0);
    let entry = id(1);
    let array = crate::FrameArrayId::new(0);
    let region = ArrayRegion::Frame(array);
    let program = ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new_typed(
            main,
            vec![],
            0,
            vec![crate::FrameArrayDescriptor::new(array, 2)],
            0,
            ValueType::Void,
            entry,
        )],
        vec![Continuation::new(
            entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::ArrayElement {
                        array: region,
                        index: 0,
                    },
                    value: b'A',
                },
                FrameInstruction::AggregateCopy {
                    src: region,
                    dst: region,
                    cells: 2,
                },
                FrameInstruction::Output {
                    src: Address::ArrayElement {
                        array: region,
                        index: 0,
                    },
                },
            ],
            Terminator::Halt,
        )],
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"A");
    }
}
