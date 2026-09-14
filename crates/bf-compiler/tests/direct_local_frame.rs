use bf_compiler::{self as bfc, BfInstruction, FrameInstruction, Terminator};

fn has_minimal_output_loop(instructions: &[BfInstruction]) -> bool {
    instructions.iter().any(|instruction| {
        let BfInstruction::Loop(body) = instruction else { return false; };
        matches!(body.as_slice(), [BfInstruction::Move(a), BfInstruction::Output, BfInstruction::Move(b), BfInstruction::Add(1)] if *a != 0 && *a == -*b)
            || has_minimal_output_loop(body)
    })
}

#[test]
fn repeat_256_has_two_slots_and_a_minimal_bf_loop_without_inlining() {
    let source = "void emit_repeat_256(cell character) {
        cell count; output(character); count=1;
        while(count != 0) { output(character); count+=1; }
    }
    void main(){emit_repeat_256(input());emit_repeat_256(input());}";
    for structure in [false, true] {
        let (program, _) = bfc::lower_source_with_options(
            source,
            bfc::ContinuationOptimizationOptions {
                inline_branch_successors: true,
                structure_local_control_flow: structure,
            },
        )
        .unwrap();
        let function = program
            .functions()
            .iter()
            .find(|f| f.name() == Some("emit_repeat_256"))
            .unwrap();
        assert_eq!(
            function.frame_slots(),
            2,
            "only character and count should need slots"
        );
        let nodes: Vec<_> = program
            .continuations()
            .iter()
            .filter(|c| c.function() == function.id())
            .collect();
        assert_eq!(nodes.len(), 1);
        assert!(matches!(
            nodes[0].terminator(),
            Terminator::Return { value: None }
        ));
        let loops: Vec<_> = nodes[0]
            .body()
            .iter()
            .filter_map(|instruction| {
                if let FrameInstruction::Loop { condition, body } = instruction {
                    Some((condition, body))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(loops.len(), 1);
        let (condition, body) = loops[0];
        assert!(
            matches!(body.as_slice(), [FrameInstruction::Output { src }, FrameInstruction::AddConst { dst, value: 1 }] if dst == condition && src != dst)
        );
        assert!(!nodes[0].body().iter().any(|i| matches!(
            i,
            FrameInstruction::Branch { .. } | FrameInstruction::Copy { .. }
        )));
        {
            let chunk = 16;
            let bf = bfc::optimize_bf(
                &bfc::lower_continuations_with_config(
                    &program,
                    bfc::AbiConfig::new(chunk).unwrap(),
                )
                .unwrap(),
            );
            assert!(has_minimal_output_loop(bf.instructions()));
            let raw = bf.to_source();
            for character in 0..=255u8 {
                let expected: Vec<_> = std::iter::repeat_n(character, 256)
                    .chain(std::iter::repeat_n(255 - character, 256))
                    .collect();
                assert_eq!(
                    bf_interpreter::run(raw.as_bytes(), &[character, 255 - character]).unwrap(),
                    expected
                );
            }
        }
    }
}

#[test]
fn direct_conditions_preserve_values_wrapping_and_nested_local_lifetimes() {
    for source in [
        "void main(){cell n=input(); cell b=n!=0; output(b); while(0!=n){output(n); n-=1;} output(n); output(b);}",
        "void main(){cell n=input(); while(n){cell m=2; while(m!=0){output(n);m-=1;}n-=1;}}",
        "void main(){cell n=input(); n=n; while(n!=0){output(n);n=input();}output(n);}",
    ] {
        let program = bfc::lower_source(source).unwrap();
        let raw = bfc::compile_continuations(&program).unwrap();
        for initial in [0, 1, 2, 127, 255] {
            let input = [initial, 2, 0];
            let mut reference = Vec::new();
            bfc::run_continuations_with_io(
                &program,
                &mut input.as_slice(),
                &mut reference,
                bfc::ContinuationRunOptions::default(),
                |_| {},
            )
            .unwrap();
            assert_eq!(
                bf_interpreter::run(raw.as_bytes(), &input).unwrap(),
                reference
            );
            let expected = if source.contains("cell b") {
                [
                    vec![u8::from(initial != 0)],
                    (1..=initial).rev().collect(),
                    vec![0, u8::from(initial != 0)],
                ]
                .concat()
            } else if source.contains("cell m") {
                (1..=initial).rev().flat_map(|n| [n, n]).collect()
            } else if initial == 0 {
                vec![0]
            } else {
                vec![initial, 2, 0]
            };
            assert_eq!(reference, expected);
        }
    }
}

#[test]
fn calls_and_early_returns_keep_their_control_boundaries() {
    let source = "cell tick(cell n){output(n);return n-1;}
        cell work(cell n){while(n!=0){if(n==2){return n;}n=tick(n);}return 0;}
        void main(){output(work(3));output(work(0));}";
    let program = bfc::lower_source(source).unwrap();
    assert!(
        program
            .continuations()
            .iter()
            .any(|c| matches!(c.terminator(), Terminator::Call { .. }))
    );
    assert_eq!(
        bf_interpreter::run(
            bfc::compile_continuations(&program).unwrap().as_bytes(),
            b""
        )
        .unwrap(),
        [3, 2, 0]
    );
}
