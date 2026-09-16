use bf_compiler::{self as bfc, Address, FrameInstruction as I};

fn slot(index: usize) -> Address {
    Address::Frame(bfc::FrameSlot::new(index))
}

fn run(body: Vec<I>, input: &[u8], expected: &[u8]) {
    let function = bfc::FunctionId::new(0);
    let entry = bfc::ContinuationId::new(1).unwrap();
    let program = bfc::ContinuationProgram::new_with_globals(
        function,
        vec![bfc::GlobalDescriptor::cell(bfc::GlobalId::new(0))],
        vec![bfc::FunctionDescriptor::new(
            function,
            vec![],
            6,
            bfc::ValueType::Void,
            entry,
        )],
        vec![bfc::Continuation::new(
            entry,
            function,
            body,
            bfc::Terminator::Halt,
        )],
    )
    .unwrap();
    check(&program, input, expected);
}

fn check(program: &bfc::ContinuationProgram, input: &[u8], expected: &[u8]) {
    let mut ir = Vec::new();
    bfc::run_continuations_with_io(
        program,
        &mut &input[..],
        &mut ir,
        bfc::ContinuationRunOptions::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(ir, expected);
    let bf = bfc::optimize_bf(
        &bfc::lower_continuations_with_config(program, bfc::AbiConfig::new(16).unwrap()).unwrap(),
    )
    .to_source();
    assert_eq!(bf_interpreter::run(bf.as_bytes(), input).unwrap(), expected);
}

fn fused_count(body: &[I]) -> usize {
    body.iter()
        .map(|i| match i {
            I::SubWithBorrow { .. } => 1,
            I::Loop { body, .. } => fused_count(body),
            I::Branch {
                then_body,
                else_body,
                ..
            } => fused_count(then_body) + fused_count(else_body),
            _ => 0,
        })
        .sum()
}

#[test]
fn source_fusion_covers_every_pair_and_preserves_inputs() {
    let source = include_str!("../../../scripts/sub-borrow-fusion/pairs.bfc");
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            input.extend([1, a, b]);
            expected.extend([a.wrapping_sub(b), u8::from(a < b), a, b]);
        }
    }
    input.push(0);
    for structure in [false, true] {
        let (program, _) = bfc::lower_source_with_options(
            source,
            bfc::ContinuationOptimizationOptions {
                inline_branch_successors: true,
                structure_local_control_flow: structure,
            },
        )
        .unwrap();
        assert!(
            program
                .continuations()
                .iter()
                .map(|c| fused_count(c.body()))
                .sum::<usize>()
                > 0
        );
        check(&program, &input, &expected);
    }
}

#[test]
fn binary_cir_fusion_tracks_copies_and_reused_slots() {
    use bfc::{SelfhostCirBinaryOp as Op, SelfhostCirInstruction as S, SelfhostCirTerminator as T};
    let mut body = vec![S::Input { destination: 0 }, S::Input { destination: 1 }];
    for op in [Op::Less, Op::GreaterEqual] {
        body.extend([
            S::Copy {
                destination: 2,
                source: 0,
            },
            S::Copy {
                destination: 3,
                source: 1,
            },
            S::Binary {
                op,
                destination: 2,
                source: 3,
            },
            S::Copy {
                destination: 4,
                source: 2,
            },
            // Reuse the comparison destination: the difference must not
            // overwrite the flag before it has been copied to slot 4.
            S::Copy {
                destination: 2,
                source: 0,
            },
            S::Copy {
                destination: 3,
                source: 1,
            },
            S::Binary {
                op: Op::Subtract,
                destination: 2,
                source: 3,
            },
            S::Output { source: 2 },
            S::Output { source: 4 },
            S::Output { source: 0 },
            S::Output { source: 1 },
            S::Output { source: 3 },
        ]);
    }
    let cir = bfc::SelfhostCirProgram::new(
        0,
        0,
        vec![bfc::SelfhostCirFunction {
            id: 0,
            entry: 1,
            frame_cells: 6,
            return_type: bfc::SelfhostCirReturnType::Void,
            parameters: vec![],
        }],
        vec![
            bfc::SelfhostCirContinuation {
                id: 1,
                function: 0,
                instructions: vec![S::Input { destination: 5 }],
                terminator: T::Branch {
                    condition: 5,
                    then_target: 2,
                    else_target: 3,
                },
            },
            bfc::SelfhostCirContinuation {
                id: 2,
                function: 0,
                instructions: body,
                terminator: T::Goto { target: 1 },
            },
            bfc::SelfhostCirContinuation {
                id: 3,
                function: 0,
                instructions: vec![],
                terminator: T::Halt,
            },
        ],
    )
    .unwrap();
    let program = bfc::lower_selfhost_cir(&cir).unwrap();
    assert!(
        program
            .continuations()
            .iter()
            .map(|c| fused_count(c.body()))
            .sum::<usize>()
            >= 2
    );
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            input.extend([1, a, b]);
            for flag in [a < b, a >= b] {
                expected.extend([a.wrapping_sub(b), u8::from(flag), a, b, 0]);
            }
        }
    }
    input.push(0);
    check(&program, &input, &expected);
}

#[test]
fn changed_inputs_and_io_prevent_fusion() {
    for (middle, a, b) in [
        ("a+=1;", 0u8, 1u8),
        ("b-=1;", 255, 0),
        ("output(a);", 255, 1),
    ] {
        let source = format!(
            "void main() {{ cell a=input(); cell b=input();
            cell borrow=a<b; {middle} cell diff=a-b; output(diff); output(borrow); }}"
        );
        let program = bfc::lower_source(&source).unwrap();
        assert_eq!(
            program
                .continuations()
                .iter()
                .map(|c| fused_count(c.body()))
                .sum::<usize>(),
            0
        );
        let expected = if middle.starts_with("output") {
            vec![255, 254, 0]
        } else {
            vec![a.wrapping_sub(b), 0]
        };
        check(&program, &[255, 1], &expected);
    }
}

#[test]
fn production_wide_subtract_preserves_borrow_chains_and_call_frames() {
    let arena = include_str!("../../../selfhost/stage2/compiler/06_arena.bfc");
    let function = arena
        .split("WideValue wide_subtract(")
        .nth(1)
        .unwrap()
        .split("WideValue wide_multiply_length(")
        .next()
        .unwrap();
    let source = format!(
        "struct WideValue {{ cell low; cell mid; cell high; }}
        void fail(cell a, cell b) {{ output(a); output(b); }}
        WideValue wide_subtract({function}
        void main() {{
            cell more=input();
            while(more) {{
                WideValue a; WideValue b;
                a.low=input(); a.mid=input(); a.high=input();
                b.low=input(); b.mid=input(); b.high=input();
                WideValue result=wide_subtract(a,b);
                output(result.low); output(result.mid); output(result.high);
                output(a.low); output(a.mid); output(a.high);
                output(b.low); output(b.mid); output(b.high);
                more=input();
            }}
        }}"
    );
    let program = bfc::lower_source(&source).unwrap();
    let wide = program
        .functions()
        .iter()
        .find(|f| f.name() == Some("wide_subtract"))
        .unwrap();
    assert!(
        program
            .continuations()
            .iter()
            .filter(|c| c.function() == wide.id())
            .map(|c| fused_count(c.body()))
            .sum::<usize>()
            >= 2
    );
    let mut pairs = Vec::new();
    for a in [0u32, 1, 255, 256, 257, 65535, 65536, 65537, 0xffffff] {
        for b in [0, 1, 255, 256, 257, 65535, 65536, 65537, 0xffffff] {
            if a >= b {
                pairs.push((a, b));
            }
        }
    }
    let mut seed = 19u32;
    for _ in 0..1024 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let a = seed & 0xffffff;
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let b = seed & 0xffffff;
        pairs.push((a.max(b), a.min(b)));
    }
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for (a, b) in pairs {
        input.push(1);
        input.extend_from_slice(&a.to_le_bytes()[..3]);
        input.extend_from_slice(&b.to_le_bytes()[..3]);
        for value in [a - b, a, b] {
            expected.extend_from_slice(&value.to_le_bytes()[..3]);
        }
    }
    input.push(0);
    check(&program, &input, &expected);
}

#[test]
fn sub_borrow_template_covers_every_unsigned_pair() {
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            input.extend([1, a, b]);
            expected.extend([a.wrapping_sub(b), u8::from(a < b), 0, 0]);
        }
    }
    input.push(0);
    run(
        vec![
            I::Input { dst: slot(4) },
            I::Loop {
                condition: slot(4),
                body: vec![
                    I::Input { dst: slot(0) },
                    I::Input { dst: slot(1) },
                    I::SubWithBorrow {
                        left: slot(0),
                        right: slot(1),
                        difference: slot(2),
                        borrow: slot(3),
                        true_value: 1,
                        false_value: 0,
                    },
                    I::Output { src: slot(2) },
                    I::Output { src: slot(3) },
                    I::Output { src: slot(0) },
                    I::Output { src: slot(1) },
                    I::Input { dst: slot(4) },
                ],
            },
        ],
        &input,
        &expected,
    );
}

#[test]
fn sub_borrow_snapshots_and_output_order_allow_aliases() {
    let addresses = [
        slot(0),
        slot(1),
        Address::Global(bfc::GlobalId::new(0)),
        Address::AbiValue,
    ];
    let mut body = Vec::new();
    let mut expected = Vec::new();
    for left in addresses {
        for right in addresses {
            for difference in addresses {
                for borrow in addresses {
                    for values in [[0, 255, 128, 1], [255, 0, 1, 128], [173, 173, 91, 91]] {
                        let mut cells: Vec<_> = addresses.into_iter().zip(values).collect();
                        for &(dst, value) in &cells {
                            body.push(I::Set { dst, value });
                        }
                        let get = |address| cells.iter().find(|(a, _)| *a == address).unwrap().1;
                        let a = get(left);
                        let b = get(right);
                        for (dst, value) in [
                            (left, 0),
                            (right, 0),
                            (difference, a.wrapping_sub(b)),
                            (borrow, if a < b { 173 } else { 91 }),
                        ] {
                            cells.iter_mut().find(|(a, _)| *a == dst).unwrap().1 = value;
                        }
                        body.push(I::SubWithBorrow {
                            left,
                            right,
                            difference,
                            borrow,
                            true_value: 173,
                            false_value: 91,
                        });
                        for (src, value) in cells {
                            body.push(I::Output { src });
                            expected.push(value);
                        }
                    }
                }
            }
        }
    }
    run(body, &[], &expected);
}
