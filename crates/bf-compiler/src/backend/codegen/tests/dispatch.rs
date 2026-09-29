use super::*;

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
