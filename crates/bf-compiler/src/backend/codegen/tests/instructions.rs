use super::*;

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
