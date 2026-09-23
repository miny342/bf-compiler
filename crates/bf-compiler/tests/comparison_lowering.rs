use bf_compiler::{self as bfc, Address, FrameInstruction as I};

#[test]
fn binary_cir_relations_cover_every_pair() {
    use bfc::{SelfhostCirBinaryOp as Op, SelfhostCirInstruction as S, SelfhostCirTerminator as T};
    let mut instructions = Vec::new();
    for op in [Op::Less, Op::LessEqual, Op::Greater, Op::GreaterEqual] {
        instructions.extend([
            S::Input { destination: 0 },
            S::Input { destination: 1 },
            S::Binary {
                op,
                destination: 0,
                source: 1,
            },
            S::Output { source: 0 },
            S::Output { source: 1 },
        ]);
    }
    let cir = bfc::SelfhostCirProgram::new(
        0,
        0,
        vec![bfc::SelfhostCirFunction {
            id: 0,
            entry: 1,
            frame_cells: 3,
            return_type: bfc::SelfhostCirReturnType::Void,
            parameters: vec![],
        }],
        vec![
            bfc::SelfhostCirContinuation {
                id: 1,
                function: 0,
                instructions: vec![S::Input { destination: 2 }],
                terminator: T::Branch {
                    condition: 2,
                    then_target: 2,
                    else_target: 3,
                },
            },
            bfc::SelfhostCirContinuation {
                id: 2,
                function: 0,
                instructions,
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
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            input.push(1);
            for value in [a < b, a <= b, a > b, a >= b] {
                input.extend([a, b]);
                expected.extend([u8::from(value), 0]);
            }
        }
    }
    input.push(0);
    let program = bfc::lower_selfhost_cir(&cir).unwrap();
    {
        let chunk = 16;
        let bf = bfc::optimize_bf(
            &bfc::lower_continuations_with_config(&program, bfc::AbiConfig::new(chunk).unwrap())
                .unwrap(),
        )
        .to_source();
        assert_eq!(
            bf_interpreter::run(bf.as_bytes(), &input).unwrap(),
            expected
        );
    }
}

#[test]
fn all_unsigned_pairs_preserve_source_operands_and_relational_results() {
    // Wrapping counters cover every pair, including equality and both extremes.
    let source = "void compare(cell a, cell b) {
        output(a<b); output(a<=b); output(a>b); output(a>=b);
        output(a); output(b);
    }
    void main() {
        cell a; cell outer=1;
        while(outer) {
            cell b; cell inner=1;
            while(inner) { compare(a,b); b+=1; inner=b!=0; }
            a+=1; outer=a!=0;
        }
    }";
    let expected: Vec<u8> = (0..=255u8)
        .flat_map(|a| {
            (0..=255u8).flat_map(move |b| {
                [
                    u8::from(a < b),
                    u8::from(a <= b),
                    u8::from(a > b),
                    u8::from(a >= b),
                    a,
                    b,
                ]
            })
        })
        .collect();
    for structure in [false, true] {
        let (program, _) = bfc::lower_source_with_options(
            source,
            bfc::ContinuationOptimizationOptions {
                inline_branch_successors: true,
                structure_local_control_flow: structure,
                ..Default::default()
            },
        )
        .unwrap();
        let mut actual = Vec::new();
        bfc::run_continuations_with_io(
            &program,
            &mut &b""[..],
            &mut actual,
            bfc::ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(actual, expected);
        {
            let chunk = 16;
            let bf = bfc::optimize_bf(
                &bfc::lower_continuations_with_config(
                    &program,
                    bfc::AbiConfig::new(chunk).unwrap(),
                )
                .unwrap(),
            );
            assert_eq!(
                bf_interpreter::run(bf.to_source().as_bytes(), b"").unwrap(),
                expected
            );
        }
    }
}

#[test]
fn compare_snapshots_allow_aliases_and_clear_operands_before_writing_result() {
    let a = Address::Frame(bfc::FrameSlot::new(0));
    let b = Address::Frame(bfc::FrameSlot::new(1));
    let d = Address::Frame(bfc::FrameSlot::new(2));
    let global = bfc::GlobalId::new(0);
    let g = Address::Global(global);
    let function = bfc::FunctionId::new(0);
    let entry = bfc::ContinuationId::new(1).unwrap();
    let mut body = Vec::new();
    let mut expected = Vec::new();
    for left in [a, b, g, Address::AbiValue] {
        for right in [a, b, g, Address::AbiValue] {
            for dst in [a, b, d, g, Address::AbiValue] {
                for av in [0, 1, 127, 255] {
                    for bv in [0, 1, 128, 255] {
                        body.extend([
                            I::Set { dst: a, value: av },
                            I::Set { dst: b, value: bv },
                            I::Set { dst: d, value: 99 },
                            I::Set { dst: g, value: 254 },
                            I::Set {
                                dst: Address::AbiValue,
                                value: 42,
                            },
                        ]);
                        let mut cells =
                            vec![(a, av), (b, bv), (d, 99), (g, 254), (Address::AbiValue, 42)];
                        let get = |x| cells.iter().find(|(addr, _)| *addr == x).unwrap().1;
                        let value = if get(left) < get(right) { 173 } else { 91 };
                        for (addr, cell) in &mut cells {
                            if *addr == left || *addr == right {
                                *cell = 0;
                            }
                            if *addr == dst {
                                *cell = value;
                            }
                        }
                        body.push(I::Compare {
                            left,
                            right,
                            dst,
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
    let program = bfc::ContinuationProgram::new_with_globals(
        function,
        vec![bfc::GlobalDescriptor::cell(global)],
        vec![bfc::FunctionDescriptor::new(
            function,
            vec![],
            3,
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
    {
        let chunk = 16;
        let bf = bfc::optimize_bf(
            &bfc::lower_continuations_with_config(&program, bfc::AbiConfig::new(chunk).unwrap())
                .unwrap(),
        )
        .to_source();
        assert_eq!(bf_interpreter::run(bf.as_bytes(), b"").unwrap(), expected);
    }
}
