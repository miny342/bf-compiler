use bf_interpreter::{RunResult, run, run_with_stats};

use super::*;
use crate::cell::continuation_adapter::adapt_flat_program;
use crate::{CellId, Instruction, Program};

fn execute_flat(instructions: Vec<Instruction>, cells: usize, config: AbiConfig) -> Vec<u8> {
    let flat = Program::new(cells, instructions).unwrap();
    let continuations = adapt_flat_program(&flat).unwrap();
    let bf = lower_continuations_with_config(&continuations, config)
        .unwrap()
        .to_source();
    run(bf.as_bytes(), b"").unwrap()
}

fn id(value: u16) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}

fn execute_continuations(program: &ContinuationProgram, chunk_cells: usize) -> Vec<u8> {
    execute_continuations_with_stats(program, chunk_cells).output
}

fn execute_continuations_with_stats(
    program: &ContinuationProgram,
    chunk_cells: usize,
) -> RunResult {
    let source = lower_continuations_with_config(program, AbiConfig::new(chunk_cells).unwrap())
        .unwrap()
        .to_source();
    run_with_stats(source.as_bytes(), b"").unwrap()
}

fn recursive_countdown_program() -> ContinuationProgram {
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
fn dispatcher_matches_nonzero_high_byte_ids() {
    let main = FunctionId::new(0);
    let entry = ContinuationId::new(0x0101).unwrap();
    let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, entry);
    let continuation = Continuation::new(
        entry,
        main,
        vec![
            FrameInstruction::Set {
                dst: Address::Frame(FrameSlot::new(0)),
                value: b'H',
            },
            FrameInstruction::Output {
                src: Address::Frame(FrameSlot::new(0)),
            },
        ],
        Terminator::Halt,
    );
    let program = ContinuationProgram::new(main, vec![function], vec![continuation]).unwrap();
    let source = compile_continuations(&program).unwrap();
    assert_eq!(run(source.as_bytes(), b"").unwrap(), b"H");
}

#[test]
fn dispatcher_selects_low_zero_and_crosses_high_byte_pages() {
    let main = FunctionId::new(0);
    let first = id(1);
    let second = id(0x0100);
    let third = id(0x0201);
    let fourth = id(u16::MAX);
    let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
    let output = |continuation, byte, target| {
        Continuation::new(
            continuation,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: byte,
                },
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(0)),
                },
            ],
            target,
        )
    };
    let program = ContinuationProgram::new(
        main,
        vec![function],
        vec![
            output(first, b'A', Terminator::Goto { target: second }),
            output(second, b'B', Terminator::Goto { target: third }),
            output(third, b'C', Terminator::Goto { target: fourth }),
            output(fourth, b'D', Terminator::Halt),
        ],
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"ABCD");
    }

    let artifact = lower_continuations_with_profile_and_codegen_options(
        &program,
        ProfileGranularity::Abi,
        AbiCodegenOptions {
            region_emission: false,
            ..Default::default()
        },
    )
    .map(|p| optimize_annotated_bf(&p).profile_artifact(false))
    .expect("profiled dispatcher should compile");
    assert_eq!(
        artifact.source,
        optimize_bf(
            &lower_continuations_with_codegen_options(
                &program,
                AbiCodegenOptions {
                    region_emission: false,
                    ..Default::default()
                }
            )
            .unwrap()
        )
        .to_source()
    );
    artifact
        .map
        .validate_for_source(artifact.source.as_bytes())
        .unwrap();
    let page_keys = artifact
        .map
        .sites
        .iter()
        .filter(|site| {
            site.stable_key
                .strip_prefix("abi.dispatch.page.")
                .is_some_and(|suffix| suffix.parse::<u8>().is_ok())
        })
        .map(|site| site.stable_key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        page_keys,
        [
            "abi.dispatch.page.0",
            "abi.dispatch.page.1",
            "abi.dispatch.page.2",
            "abi.dispatch.page.255"
        ]
    );
    for key in ["abi.dispatch.page.select", "abi.dispatch.page.countdown"] {
        assert_eq!(
            artifact
                .map
                .sites
                .iter()
                .filter(|site| site.stable_key == key)
                .count(),
            4,
            "each dispatch page should expose {key}",
        );
    }
}

#[test]
fn balanced_dispatch_visits_every_dense_entry_across_pages() {
    let main = FunctionId::new(0);
    let cell = Address::Frame(FrameSlot::new(0));
    let order = (1..=512_u16)
        .flat_map(|i| [i, 1025 - i])
        .collect::<Vec<_>>();
    let continuations = order
        .iter()
        .enumerate()
        .map(|(index, &value)| {
            Continuation::new(
                id(value),
                main,
                vec![
                    FrameInstruction::Set {
                        dst: cell,
                        value: (value % 251) as u8,
                    },
                    FrameInstruction::Output { src: cell },
                ],
                order
                    .get(index + 1)
                    .map_or(Terminator::Halt, |&next| Terminator::Goto {
                        target: id(next),
                    }),
            )
        })
        .collect();
    let program = ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new(
            main,
            vec![],
            1,
            ValueType::Void,
            id(1),
        )],
        continuations,
    )
    .unwrap();
    let encoding = DispatchEncoding::new(&program, &PortalPlan::new(&program).unwrap());
    assert_eq!(encoding.page_width, 33);
    let expected = order.iter().map(|v| (v % 251) as u8).collect::<Vec<_>>();
    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), expected);
    }
}

#[test]
fn balanced_encoding_is_injective_at_width_and_id_limits() {
    let main = FunctionId::new(0);
    for count in [256, 257, 288, 289, 65025, 65535] {
        let program = ContinuationProgram::new(
            main,
            vec![FunctionDescriptor::new(
                main,
                vec![],
                0,
                ValueType::Void,
                id(1),
            )],
            (1..=count)
                .map(|value| Continuation::new(id(value), main, vec![], Terminator::Halt))
                .collect(),
        )
        .unwrap();
        let encoding = DispatchEncoding::new(&program, &PortalPlan::new(&program).unwrap());
        let encoded = (1..=count)
            .map(|value| encoding.encode(id(value)))
            .collect::<HashSet<_>>();
        assert_eq!(encoded.len(), usize::from(count));
        assert!(!encoded.contains(&0));
    }
}

#[test]
fn balanced_dispatch_preserves_recursive_call_and_return() {
    let original = recursive_countdown_program();
    // Width 33 makes every live entry xx00 on a different page. Calls
    // and returns must migrate contexts without selecting a second case.
    let remap = |value: ContinuationId| id(value.get() * 33);
    let functions = original
        .functions()
        .iter()
        .map(|f| {
            FunctionDescriptor::new(
                f.id(),
                f.parameters().to_vec(),
                f.frame_slots(),
                f.return_type(),
                remap(f.entry()),
            )
        })
        .collect();
    let mut continuations = original
        .continuations()
        .iter()
        .map(|c| {
            let mut terminator = c.terminator().clone();
            match &mut terminator {
                Terminator::Call { return_to, .. } => *return_to = remap(*return_to),
                Terminator::Branch {
                    then_target,
                    else_target,
                    ..
                } => {
                    *then_target = remap(*then_target);
                    *else_target = remap(*else_target);
                }
                Terminator::Return { .. } | Terminator::Halt => (),
                _ => panic!("unexpected recursive fixture terminator"),
            }
            Continuation::new(remap(c.id()), c.function(), c.body().to_vec(), terminator)
        })
        .collect::<Vec<_>>();
    let live = continuations.iter().map(|c| c.id()).collect::<HashSet<_>>();
    continuations.extend(
        (1..=1024)
            .filter(|&value| !live.contains(&id(value)))
            .map(|value| Continuation::new(id(value), original.main(), vec![], Terminator::Halt)),
    );
    let program = ContinuationProgram::new(original.main(), functions, continuations).unwrap();
    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"\x04L");
    }
}

#[test]
fn dispatcher_does_not_run_low_zero_after_call_migrates_context() {
    let main = FunctionId::new(0);
    let helper = FunctionId::new(1);
    let main_entry = id(0x0101);
    let decoy = id(0x0100);
    let main_resume = id(2);
    let helper_entry = id(3);
    let functions = vec![
        FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(helper, vec![], 0, crate::ValueType::Void, helper_entry),
    ];
    // Keep the xx00 decoy after the entry to exercise the dispatcher's
    // ordering rather than relying on ContinuationProgram input order.
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![],
            Terminator::Call {
                callee: helper,
                arguments: vec![],
                return_to: main_resume,
            },
        ),
        Continuation::new(
            decoy,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: b'X',
                },
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(0)),
                },
            ],
            Terminator::Halt,
        ),
        Continuation::new(
            main_resume,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: b'A',
                },
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(0)),
                },
            ],
            Terminator::Halt,
        ),
        Continuation::new(
            helper_entry,
            helper,
            vec![],
            Terminator::Return { value: None },
        ),
    ];
    let program = ContinuationProgram::new(main, functions, continuations).unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"A");
    }
}

#[test]
fn dispatcher_uses_equality_scan_for_a_sparse_page() {
    let main = FunctionId::new(0);
    let first = id(0x0101);
    let last = id(0x01ff);
    let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
    let program = ContinuationProgram::new(
        main,
        vec![function],
        vec![
            Continuation::new(first, main, vec![], Terminator::Goto { target: last }),
            Continuation::new(
                last,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: b'S',
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                ],
                Terminator::Halt,
            ),
        ],
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"S");
    }
    let artifact = lower_continuations_with_profile_and_codegen_options(
        &program,
        ProfileGranularity::Abi,
        AbiCodegenOptions {
            region_emission: false,
            ..Default::default()
        },
    )
    .map(|p| optimize_annotated_bf(&p).profile_artifact(false))
    .expect("sparse profiled dispatcher should compile");
    assert!(
        artifact
            .map
            .sites
            .iter()
            .any(|site| site.stable_key == "abi.dispatch.page.compare")
    );
    assert!(
        artifact
            .map
            .sites
            .iter()
            .all(|site| site.stable_key != "abi.dispatch.page.countdown")
    );
}

#[test]
fn dispatcher_counts_down_contiguous_high_byte_pages() {
    let main = FunctionId::new(0);
    let first = id(0x0101);
    let second = id(0x0201);
    let third = id(0x0301);
    let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
    let output = |continuation, byte, target| {
        Continuation::new(
            continuation,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: byte,
                },
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(0)),
                },
            ],
            target,
        )
    };
    let program = ContinuationProgram::new(
        main,
        vec![function],
        vec![
            output(first, b'A', Terminator::Goto { target: second }),
            output(second, b'B', Terminator::Goto { target: third }),
            output(third, b'C', Terminator::Halt),
        ],
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"ABC");
    }
    let artifact = lower_continuations_with_profile_and_codegen_options(
        &program,
        ProfileGranularity::Abi,
        AbiCodegenOptions {
            region_emission: false,
            ..Default::default()
        },
    )
    .map(|p| optimize_annotated_bf(&p).profile_artifact(false))
    .expect("profiled high-byte countdown should compile");
    let keys = artifact
        .map
        .sites
        .iter()
        .map(|site| site.stable_key.as_str())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"abi.dispatch.pages.countdown"));
    assert!(!keys.contains(&"abi.dispatch.pages.compare"));
    assert!(!keys.contains(&"abi.dispatch.page.select"));
}

#[test]
fn dispatcher_uses_equality_scan_for_sparse_high_byte_pages() {
    let main = FunctionId::new(0);
    let first = id(0x0101);
    let last = id(0xff01);
    let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
    let program = ContinuationProgram::new(
        main,
        vec![function],
        vec![
            Continuation::new(first, main, vec![], Terminator::Goto { target: last }),
            Continuation::new(
                last,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: b'H',
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                ],
                Terminator::Halt,
            ),
        ],
    )
    .unwrap();

    {
        let chunk_cells = 16;
        assert_eq!(execute_continuations(&program, chunk_cells), b"H");
    }
    let artifact = lower_continuations_with_profile_and_codegen_options(
        &program,
        ProfileGranularity::Abi,
        AbiCodegenOptions {
            region_emission: false,
            ..Default::default()
        },
    )
    .map(|p| optimize_annotated_bf(&p).profile_artifact(false))
    .expect("profiled sparse high-byte dispatcher should compile");
    let keys = artifact
        .map
        .sites
        .iter()
        .map(|site| site.stable_key.as_str())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"abi.dispatch.pages.compare"));
    assert!(!keys.contains(&"abi.dispatch.pages.countdown"));
    assert_eq!(
        keys.iter()
            .filter(|key| **key == "abi.dispatch.page.select")
            .count(),
        2
    );
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
fn return_dispatches_to_a_continuation_with_a_nonzero_high_byte() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let main_entry = id(1);
    let main_resume = id(0x0101);
    let callee_entry = id(2);
    let functions = vec![
        FunctionDescriptor::new(main, vec![], 0, crate::ValueType::Void, main_entry),
        FunctionDescriptor::new(callee, vec![], 0, crate::ValueType::Void, callee_entry),
    ];
    let continuations = vec![
        Continuation::new(
            main_entry,
            main,
            vec![],
            Terminator::Call {
                callee,
                arguments: vec![],
                return_to: main_resume,
            },
        ),
        Continuation::new(
            main_resume,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::AbiValue,
                    value: b'H',
                },
                FrameInstruction::Output {
                    src: Address::AbiValue,
                },
            ],
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
        assert_eq!(execute_continuations(&program, chunk_cells), b"H");
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
