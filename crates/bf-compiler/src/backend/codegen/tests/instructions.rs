use super::*;

#[test]
fn fixed_truth_banks_do_not_require_comparison_operands() {
    use crate::{GlobalDescriptor, GlobalId};
    use FrameInstruction as I;
    let function = FunctionId::new(0);
    let slot = |n| Address::Frame(FrameSlot::new(n));
    let p = ContinuationProgram::new_with_globals(
        function,
        vec![GlobalDescriptor::cell(GlobalId::new(0))],
        vec![FunctionDescriptor::new(
            function,
            vec![],
            3,
            ValueType::Void,
            id(1),
        )],
        vec![Continuation::new(
            id(1),
            function,
            vec![
                I::Output {
                    src: Address::Global(GlobalId::new(0)),
                },
                I::Input { dst: slot(2) },
                I::Loop {
                    condition: slot(2),
                    body: vec![
                        I::Input { dst: slot(0) },
                        I::Copy {
                            src: slot(0),
                            dst: slot(1),
                        },
                        I::Branch {
                            condition: slot(1),
                            then_body: vec![I::Output { src: slot(0) }],
                            else_body: vec![I::Output { src: slot(0) }],
                        },
                        I::Output { src: slot(0) },
                        I::Input { dst: slot(2) },
                    ],
                },
            ],
            Terminator::Halt,
        )],
    )
    .unwrap();
    for static_frames in [false, true] {
        let options = AbiCodegenOptions {
            inplace_compare: true,
            static_frames,
            ..Default::default()
        };
        let annotated = lower_continuations_with_profile_and_codegen_options(
            &p,
            ProfileGranularity::Abi,
            options,
        )
        .unwrap();
        let has_truth = annotated
            .sites()
            .records()
            .iter()
            .any(|s| s.stable_key == "abi.frame.truth_copy");
        assert_eq!(has_truth, static_frames);
        let mut input = Vec::new();
        let mut expected = vec![0];
        for value in 0..=255u8 {
            input.extend([1, value]);
            expected.extend([value, value]);
        }
        input.push(0);
        let source = optimize_annotated_bf(&annotated).to_source();
        for disabled in [false, true] {
            let result = bf_interpreter::run_with_options(
                source.as_bytes(),
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
            assert_eq!(result.output, expected);
        }
    }
}

#[test]
fn portal_accessed_aggregate_padding_stays_private_to_the_portal() {
    use crate::{FrameAggregateDescriptor, FrameAggregateId, LogicalOffset, ValueOperand};
    use FrameInstruction as I;
    let function = FunctionId::new(0);
    let aggregate = FrameAggregateId::new(0);
    let region = AggregateRegion::Frame(aggregate);
    let field = Address::ArrayElement {
        array: region,
        index: 0,
    };
    let slot = |n| Address::Frame(FrameSlot::new(n));
    let program = ContinuationProgram::new(
        function,
        vec![FunctionDescriptor::new_aggregates(
            function,
            vec![],
            4,
            vec![FrameAggregateDescriptor::new(aggregate, 3)],
            0,
            ValueType::Void,
            id(1),
        )],
        vec![
            Continuation::new(
                id(1),
                function,
                vec![
                    I::Input { dst: field },
                    I::Set {
                        dst: slot(0),
                        value: 0,
                    },
                    I::Set {
                        dst: slot(3),
                        value: 0,
                    },
                ],
                Terminator::AggregateLoad {
                    source: region,
                    offset: LogicalOffset::new(slot(0), slot(3)),
                    destination: ValueOperand::Cell(slot(1)),
                    cells: 1,
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                function,
                vec![
                    I::Copy {
                        src: field,
                        dst: slot(2),
                    },
                    I::Branch {
                        condition: slot(2),
                        then_body: vec![I::Output { src: field }],
                        else_body: vec![I::Output { src: field }],
                    },
                    I::Output { src: slot(1) },
                    I::Output { src: field },
                ],
                Terminator::Halt,
            ),
        ],
    )
    .unwrap();
    let annotated = lower_continuations_with_profile_and_codegen_options(
        &program,
        ProfileGranularity::Abi,
        AbiCodegenOptions {
            inplace_compare: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !annotated
            .sites()
            .records()
            .iter()
            .any(|site| site.stable_key == "abi.frame.truth_copy")
    );
    let source = optimize_annotated_bf(&annotated).to_source();
    for value in [0, 1, 17, 128, 255] {
        for disabled in [false, true] {
            let result = bf_interpreter::run_with_options(
                source.as_bytes(),
                &[value],
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
            assert_eq!(result.output, vec![value; 3]);
        }
    }
}

#[test]
fn aggregate_padding_truth_tests_preserve_fields_and_reuse_guards() {
    use crate::{FrameAggregateDescriptor, FrameAggregateId, GlobalDescriptor, GlobalId};
    use FrameInstruction as I;
    let function = FunctionId::new(0);
    let region = AggregateRegion::Frame(FrameAggregateId::new(0));
    let field = |index| Address::ArrayElement {
        array: region,
        index,
    };
    let slot = |index| Address::Frame(FrameSlot::new(index));
    let global = GlobalId::new(0);
    let mut input = Vec::new();
    for value in 0..=255u8 {
        input.extend([1, value]);
    }
    input.push(0);
    for cells in 1..=6 {
        for selected in 0..cells {
            for constant in [0u8, 1, 17, 128, 255] {
                let mut iteration = (0..cells)
                    .map(|index| I::Set {
                        dst: field(index),
                        value: 40 + index as u8,
                    })
                    .collect::<Vec<_>>();
                iteration.extend([
                    I::Input {
                        dst: field(selected),
                    },
                    I::Copy {
                        src: field(selected),
                        dst: slot(0),
                    },
                    I::AddConst {
                        dst: slot(0),
                        value: constant.wrapping_neg(),
                    },
                    I::Branch {
                        condition: slot(0),
                        then_body: vec![
                            I::Output {
                                src: field(selected),
                            },
                            I::Set {
                                dst: field(selected),
                                value: 17,
                            },
                            I::Set {
                                dst: slot(0),
                                value: 23,
                            },
                        ],
                        else_body: vec![
                            I::Output {
                                src: field(selected),
                            },
                            I::Set {
                                dst: field(selected),
                                value: 31,
                            },
                            I::Set {
                                dst: slot(0),
                                value: 37,
                            },
                        ],
                    },
                    // Reuse the same padding after overwriting the live source.
                    I::Copy {
                        src: field(selected),
                        dst: slot(0),
                    },
                    I::Branch {
                        condition: slot(0),
                        then_body: vec![I::Output {
                            src: field(selected),
                        }],
                        else_body: vec![],
                    },
                    I::Output { src: slot(0) },
                ]);
                iteration.retain(|i| !matches!(i, I::AddConst { value: 0, .. }));
                iteration.extend((0..cells).map(|index| I::Output { src: field(index) }));
                iteration.push(I::Input { dst: slot(1) });
                let p = ContinuationProgram::new_with_globals(
                    function,
                    vec![GlobalDescriptor::cell(global)],
                    vec![FunctionDescriptor::new_aggregates(
                        function,
                        vec![],
                        2,
                        vec![FrameAggregateDescriptor::new(
                            FrameAggregateId::new(0),
                            cells,
                        )],
                        0,
                        ValueType::Void,
                        id(1),
                    )],
                    vec![Continuation::new(
                        id(1),
                        function,
                        vec![
                            I::Set {
                                dst: Address::Global(global),
                                value: 1,
                            },
                            I::Input { dst: slot(1) },
                            I::Loop {
                                condition: slot(1),
                                body: iteration,
                            },
                        ],
                        Terminator::Halt,
                    )],
                )
                .unwrap();
                let expected: Vec<_> = (0..=255u8)
                    .flat_map(|value| {
                        let changed = if value == constant { 31 } else { 17 };
                        let mut bytes = vec![value, changed, 0];
                        bytes.extend((0..cells).map(|index| {
                            if index == selected {
                                changed
                            } else {
                                40 + index as u8
                            }
                        }));
                        bytes
                    })
                    .collect();
                for static_frames in [false, true] {
                    let options = AbiCodegenOptions {
                        inplace_compare: true,
                        static_frames,
                        ..Default::default()
                    };
                    let bf = optimize_bf(
                        &lower_continuations_with_codegen_options(&p, options).unwrap(),
                    )
                    .to_source();
                    let run = |disabled| {
                        bf_interpreter::run_with_options(
                            bf.as_bytes(),
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
                        .unwrap()
                    };
                    let native = run(false);
                    let rle = run(true);
                    assert_eq!(
                        native.output, expected,
                        "cells={cells} selected={selected} constant={constant} fixed={static_frames}"
                    );
                    assert_eq!(rle.output, expected);
                    assert_eq!(
                        native.stats.executed_instructions,
                        rle.stats.executed_instructions
                    );
                    assert_eq!(
                        native.stats.executed_rle_instructions,
                        rle.stats.executed_rle_instructions
                    );
                }
            }
        }
    }
}

#[test]
fn constant_truth_copies_restore_every_source_before_aliasing_branch_bodies() {
    use FrameInstruction as I;
    let function = FunctionId::new(0);
    let entry = id(1);
    let slot = |index| Address::Frame(FrameSlot::new(index));
    let mut input = Vec::new();
    for value in 0..=255u8 {
        input.extend([1, value]);
    }
    input.push(0);
    for constant in 0..=255u8 {
        let body = vec![
            I::Input { dst: slot(6) },
            I::Loop {
                condition: slot(6),
                body: vec![
                    I::Input { dst: slot(0) },
                    I::Set {
                        dst: slot(1),
                        value: 255,
                    },
                    I::Copy {
                        src: slot(0),
                        dst: slot(1),
                    },
                    I::AddConst {
                        dst: slot(1),
                        value: constant.wrapping_neg(),
                    },
                    I::Branch {
                        condition: slot(1),
                        then_body: vec![
                            I::Output { src: slot(0) },
                            I::Output { src: slot(1) },
                            // Both writes alias values used by the test. Any
                            // delayed restoration would corrupt this result.
                            I::Set {
                                dst: slot(0),
                                value: 17,
                            },
                            I::Set {
                                dst: slot(1),
                                value: 23,
                            },
                        ],
                        else_body: vec![
                            I::Output { src: slot(0) },
                            I::Output { src: slot(1) },
                            I::Set {
                                dst: slot(0),
                                value: 31,
                            },
                            I::Set {
                                dst: slot(1),
                                value: 37,
                            },
                        ],
                    },
                    I::Output { src: slot(0) },
                    I::Output { src: slot(1) },
                    I::Set {
                        dst: slot(4),
                        value: 255,
                    },
                    // Reserve and reuse the source's private comparison guards.
                    I::Compare {
                        left: slot(0),
                        right: slot(4),
                        dst: slot(5),
                        true_value: 1,
                        false_value: 0,
                    },
                    I::Output { src: slot(5) },
                    I::Input { dst: slot(6) },
                ],
            },
        ];
        let program = ContinuationProgram::new(
            function,
            vec![FunctionDescriptor::new(
                function,
                vec![],
                7,
                ValueType::Void,
                entry,
            )],
            vec![Continuation::new(entry, function, body, Terminator::Halt)],
        )
        .unwrap();
        let expected: Vec<_> = (0..=255u8)
            .flat_map(|value| {
                let source = if value == constant { 31 } else { 17 };
                // Branch clears its condition after the selected body too.
                [value, 0, source, 0, 1]
            })
            .collect();
        let mut options = vec![AbiCodegenOptions {
            inplace_compare: true,
            ..Default::default()
        }];
        if [0, 1, 32, 128, 255].contains(&constant) {
            options.extend([
                AbiCodegenOptions::default(),
                AbiCodegenOptions {
                    inplace_compare: true,
                    static_frames: true,
                    ..Default::default()
                },
            ]);
        }
        for options in options {
            let bf =
                optimize_bf(&lower_continuations_with_codegen_options(&program, options).unwrap())
                    .to_source();
            for disabled in [false, true] {
                let result = bf_interpreter::run_with_options(
                    bf.as_bytes(),
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
                    "constant={constant}, options={options:?}, rle_only={disabled}"
                );
            }
        }
    }
}

#[test]
fn constant_truth_copy_fusion_respects_source_file_boundaries() {
    use FrameInstruction as I;
    let function = FunctionId::new(0);
    let slot = |index| Address::Frame(FrameSlot::new(index));
    for same_file in [false, true] {
        let copy_span = SourceSpan {
            file_id: 0,
            start_byte: 10,
            end_byte: 20,
        };
        let adjust_span = SourceSpan {
            file_id: u32::from(!same_file),
            start_byte: 30,
            end_byte: 40,
        };
        let program = ContinuationProgram::new(
            function,
            vec![FunctionDescriptor::new(
                function,
                vec![],
                4,
                ValueType::Void,
                id(1),
            )],
            vec![
                Continuation::new(
                    id(1),
                    function,
                    vec![
                        I::Input { dst: slot(0) },
                        I::Copy {
                            src: slot(0),
                            dst: slot(1),
                        },
                        I::AddConst {
                            dst: slot(1),
                            value: 224,
                        },
                        I::Branch {
                            condition: slot(1),
                            then_body: vec![I::Output { src: slot(0) }],
                            else_body: vec![I::Output { src: slot(0) }],
                        },
                        I::Compare {
                            left: slot(0),
                            right: slot(2),
                            dst: slot(3),
                            true_value: 1,
                            false_value: 0,
                        },
                    ],
                    Terminator::Halt,
                )
                .with_source_spans(vec![None, Some(copy_span), Some(adjust_span)], None),
            ],
        )
        .unwrap()
        .with_source_files(vec![
            crate::SourceFileDescriptor {
                id: 0,
                path: "first.bfc".into(),
            },
            crate::SourceFileDescriptor {
                id: 1,
                path: "second.bfc".into(),
            },
        ]);
        let artifact = lower_continuations_annotated_with_options(
            &program,
            AbiConfig::default(),
            ProfileGranularity::Source,
            AbiCodegenOptions {
                inplace_compare: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            artifact
                .sites()
                .records()
                .iter()
                .any(|site| site.stable_key.starts_with("abi.frame.truth_const_copy")),
            same_file
        );
        assert_eq!(run(artifact.to_source().as_bytes(), &[32]).unwrap(), &[32]);
    }
}

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
