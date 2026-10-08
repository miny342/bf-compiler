//! Guarded frame comparisons across chunk boundaries and coarse anchor phases.
use bf_compiler::{self as bfc, Address, FrameInstruction as I, Terminator as T};
fn slot(n: usize) -> Address {
    Address::Frame(bfc::FrameSlot::new(n))
}
#[test]
fn distant_operands_chunk_boundaries_and_dirty_neighbors_cover_every_pair() {
    let mut input = Vec::new();
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            input.extend([1, a, b]);
        }
    }
    input.push(0);
    for (left, right) in [(0, 1), (3, 4), (4, 3), (3, 17), (17, 3)] {
        let mut expected = Vec::new();
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                expected.extend([a.wrapping_sub(b), if a < b { 173 } else { 91 }, 0, 0]);
                for i in 0..20 {
                    if ![left, right, 12, 13].contains(&i) {
                        expected.push(77);
                    }
                }
            }
        }
        let mut loop_body = vec![
            I::Input { dst: slot(left) },
            I::Input { dst: slot(right) },
            I::SubWithBorrow {
                left: slot(left),
                right: slot(right),
                difference: slot(12),
                borrow: slot(13),
                true_value: 173,
                false_value: 91,
            },
            I::Output { src: slot(12) },
            I::Output { src: slot(13) },
            I::Output { src: slot(left) },
            I::Output { src: slot(right) },
        ];
        for i in 0..20 {
            if ![left, right, 12, 13].contains(&i) {
                loop_body.push(I::Output { src: slot(i) });
            }
        }
        loop_body.push(I::Input { dst: slot(20) });
        let mut body: Vec<_> = (0..20)
            .map(|n| I::Set {
                dst: slot(n),
                value: 77,
            })
            .collect();
        body.push(I::Input { dst: slot(20) });
        body.push(I::Loop {
            condition: slot(20),
            body: loop_body,
        });
        let f = bfc::FunctionId::new(0);
        let c = bfc::ContinuationId::new(1).unwrap();
        let p = bfc::ContinuationProgram::new_with_globals(
            f,
            vec![],
            vec![bfc::FunctionDescriptor::new(
                f,
                vec![],
                21,
                bfc::ValueType::Void,
                c,
            )],
            vec![bfc::Continuation::new(c, f, body, bfc::Terminator::Halt)],
        )
        .unwrap();
        let bf = bfc::optimize_bf(
            &bfc::lower_continuations_with_codegen_options(
                &p,
                bfc::AbiCodegenOptions {
                    inplace_compare: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .to_source();
        assert_eq!(
            bf_interpreter::run(bf.as_bytes(), &input).unwrap(),
            expected,
            "left={left} right={right}"
        );
    }
}

fn id(n: u16) -> bfc::ContinuationId {
    bfc::ContinuationId::new(n).unwrap()
}
#[test]
fn all_anchor_phases_preserve_unwind_markers_and_comparison_results() {
    let main = bfc::FunctionId::new(0);
    let recurse = bfc::FunctionId::new(1);
    let global = bfc::GlobalId::new(0);
    let g = Address::Global(global);
    for root_slots in [2, 17] {
        let report = vec![
            I::Copy {
                src: g,
                dst: slot(2),
            },
            I::Output { src: slot(2) },
            I::Output { src: slot(0) },
            I::Output { src: slot(5) },
        ];
        let program = bfc::ContinuationProgram::new_with_globals(
            main,
            vec![bfc::GlobalDescriptor::cell(global)],
            vec![
                bfc::FunctionDescriptor::new(main, vec![], root_slots, bfc::ValueType::Void, id(1)),
                bfc::FunctionDescriptor::new(recurse, vec![], 6, bfc::ValueType::Cell, id(3)),
            ],
            vec![
                bfc::Continuation::new(
                    id(1),
                    main,
                    vec![I::Set { dst: g, value: 173 }],
                    T::Call {
                        callee: recurse,
                        arguments: vec![],
                        return_to: id(2),
                    },
                ),
                bfc::Continuation::new(
                    id(2),
                    main,
                    vec![I::Output {
                        src: Address::AbiValue,
                    }],
                    T::Halt,
                ),
                bfc::Continuation::new(
                    id(3),
                    recurse,
                    vec![
                        I::Input { dst: slot(0) },
                        I::Input { dst: slot(1) },
                        I::Copy {
                            src: slot(0),
                            dst: slot(3),
                        },
                        I::Set {
                            dst: slot(4),
                            value: 128,
                        },
                        I::Compare {
                            left: slot(3),
                            right: slot(4),
                            dst: slot(5),
                            true_value: 1,
                            false_value: 0,
                        },
                    ],
                    T::Branch {
                        condition: slot(1),
                        then_target: id(4),
                        else_target: id(6),
                    },
                ),
                bfc::Continuation::new(
                    id(4),
                    recurse,
                    vec![],
                    T::Call {
                        callee: recurse,
                        arguments: vec![],
                        return_to: id(5),
                    },
                ),
                bfc::Continuation::new(
                    id(5),
                    recurse,
                    report.clone(),
                    T::Return {
                        value: Some(bfc::ValueOperand::Cell(slot(0))),
                    },
                ),
                bfc::Continuation::new(
                    id(6),
                    recurse,
                    report,
                    T::Return {
                        value: Some(bfc::ValueOperand::Cell(slot(0))),
                    },
                ),
            ],
        )
        .unwrap();
        for (anchor_bank, nibble_transfer) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let bf = bfc::optimize_bf(
                &bfc::lower_continuations_with_codegen_options(
                    &program,
                    bfc::AbiCodegenOptions {
                        inplace_compare: true,
                        anchor_bank,
                        nibble_transfer,
                        ..Default::default()
                    },
                )
                .unwrap(),
            )
            .to_source();
            for depth in 0..64 {
                let markers: Vec<_> = (0..=depth).map(|n| ((17 + 7 * n) % 256) as u8).collect();
                let input: Vec<_> = markers
                    .iter()
                    .enumerate()
                    .flat_map(|(n, &marker)| [marker, u8::from(n < depth)])
                    .collect();
                let mut expected: Vec<_> = markers
                    .iter()
                    .rev()
                    .flat_map(|&marker| [173, marker, u8::from(marker < 128)])
                    .collect();
                expected.push(markers[0]);
                assert_eq!(
                    bf_interpreter::run(bf.as_bytes(), &input).unwrap(),
                    expected,
                    "root={root_slots} depth={depth} anchor={anchor_bank} nibble={nibble_transfer}"
                );
            }
        }
    }
}

#[test]
fn guarded_frames_preserve_aggregates_across_calls_and_static_bridges() {
    let (program, _) = bfc::lower_source_with_options(
        "struct Pair {cell lo; cell hi;}
         Pair pack(cell a,cell b){Pair p;p.lo=a<b;p.hi=a-b;return p;}
         cell recurse(cell n){
             cell[17] values; cell index=1; values[index]=n;
             if(n==0){return 0;}
             Pair p=pack(n,3); cell small=n<4;
             return recurse(n-1)+values[index]+p.lo+p.hi+small;
         }
         void main(){output(recurse(input()));}",
        bfc::ContinuationOptimizationOptions {
            inline_functions: false,
            ..Default::default()
        },
    )
    .unwrap();
    for flags in 0..8 {
        for static_frames in [false, true] {
            for region_emission in [false, true] {
                let bf = bfc::optimize_bf(
                    &bfc::lower_continuations_with_codegen_options(
                        &program,
                        bfc::AbiCodegenOptions {
                            nibble_transfer: flags & 1 != 0,
                            inplace_compare: flags & 2 != 0,
                            anchor_bank: flags & 4 != 0,
                            static_frames,
                            region_emission,
                            ..Default::default()
                        },
                    )
                    .unwrap(),
                )
                .to_source();
                for n in [0, 1, 3, 7] {
                    let mut expected = Vec::new();
                    bfc::run_continuations_with_io(
                        &program,
                        &mut &[n][..],
                        &mut expected,
                        Default::default(),
                        |_| {},
                    )
                    .unwrap();
                    assert_eq!(
                        bf_interpreter::run(bf.as_bytes(), &[n]).unwrap(),
                        expected,
                        "flags={flags} static={static_frames} regions={region_emission} input={n}"
                    );
                }
            }
        }
    }
}

#[test]
fn optional_storage_is_included_in_the_tape_capacity_check() {
    let main = bfc::FunctionId::new(0);
    let program = bfc::ContinuationProgram::new_with_globals(
        main,
        (0..29_925)
            .map(|n| bfc::GlobalDescriptor::cell(bfc::GlobalId::new(n)))
            .collect(),
        vec![bfc::FunctionDescriptor::new(
            main,
            vec![],
            15,
            bfc::ValueType::Void,
            id(1),
        )],
        vec![bfc::Continuation::new(
            id(1),
            main,
            vec![I::Compare {
                left: slot(0),
                right: slot(14),
                dst: slot(1),
                true_value: 1,
                false_value: 0,
            }],
            T::Halt,
        )],
    )
    .unwrap();
    bfc::lower_continuations_with_codegen_options(&program, Default::default()).unwrap();
    for (inplace_compare, anchor_bank) in [(true, false), (false, true), (true, true)] {
        let mut options = bfc::AbiCodegenOptions {
            inplace_compare,
            anchor_bank,
            ..Default::default()
        };
        assert!(matches!(
            bfc::lower_continuations_with_codegen_options(&program, options),
            Err(bfc::AbiCodegenError::Layout(_) | bfc::AbiCodegenError::StaticLayout(_))
        ));
        options.unlimited_tape = true;
        let bf = bfc::optimize_bf(
            &bfc::lower_continuations_with_codegen_options(&program, options).unwrap(),
        )
        .to_source();
        assert_eq!(
            bf_interpreter::run_with_options(
                bf.as_bytes(),
                b"",
                bf_interpreter::RunOptions {
                    unbounded_tape: true,
                    ..Default::default()
                }
            )
            .unwrap()
            .output,
            b""
        );
    }
}
