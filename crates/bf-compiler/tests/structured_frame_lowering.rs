use bf_compiler::{self as bfc, ContinuationOptimizationOptions, ContinuationProgram};

fn lower(source: &str) -> (ContinuationProgram, usize) {
    let (program, stats) = bfc::lower_source_with_options(
        source,
        ContinuationOptimizationOptions {
            inline_branch_successors: false,
            structure_local_control_flow: false,
        },
    )
    .unwrap();
    (program, stats.continuations_before)
}

fn check(program: &ContinuationProgram, input: &[u8], expected: &[u8]) {
    let mut actual = Vec::new();
    bfc::run_continuations_with_io(
        program,
        &mut &input[..],
        &mut actual,
        bfc::ContinuationRunOptions::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(actual, expected, "IR input={input:?}");
    let bf = bfc::compile_continuations(program).unwrap();
    assert_eq!(
        bf_interpreter::run(bf.as_bytes(), input).unwrap(),
        expected,
        "BF input={input:?}"
    );
}

#[test]
fn nested_frame_control_preserves_values_and_recomputes_predicates() {
    let (program, before) = lower(
        "cell g;struct Pair{cell a;cell b;}void main(){
        Pair p;cell n=input();g=n;cell sum;
        while(n>0&&g!=0){
            cell m=2;
            while(m){if(n<3||n==255){sum+=n;}else{sum-=n;}m-=1;}
            p.a=n;p.b=sum;n-=1;g-=1;
        }
        output(sum);output(n);output(g);output(p.a);output(p.b);
    }",
    );
    assert_eq!(before, 1);
    for n in [0, 1, 2, 3, 4, 127, 255u8] {
        let sum = (1..=n).fold(0u8, |sum, v| {
            if v < 3 || v == 255 {
                sum.wrapping_add(v.wrapping_mul(2))
            } else {
                sum.wrapping_sub(v.wrapping_mul(2))
            }
        });
        check(&program, &[n], &[sum, 0, 0, u8::from(n != 0), sum]);
    }
}

#[test]
fn structured_short_circuits_consume_only_the_selected_inputs() {
    for (source, input, expected) in [
        (
            "void main(){cell a=input();output(a&&input());output(input());output(a);}",
            vec![0, 7, 9],
            vec![0, 7, 0],
        ),
        (
            "void main(){cell a=input();output(a&&input());output(input());output(a);}",
            vec![255, 7, 9],
            vec![1, 9, 255],
        ),
        (
            "void main(){cell a=input();output(a||input());output(input());output(a);}",
            vec![0, 7, 9],
            vec![1, 9, 0],
        ),
        (
            "void main(){cell a=input();output(a||input());output(input());output(a);}",
            vec![255, 7, 9],
            vec![1, 7, 255],
        ),
        (
            "void main(){cell n=3;while(n&&input()){if(input()||input()){output(n);}n-=1;}output(n);output(input());}",
            vec![1, 0, 8, 1, 9, 0, 42],
            vec![3, 2, 1, 42],
        ),
        (
            "void main(){while(input()){cell n=input();if(n){output(n);}else{output(9);}}output(input());}",
            vec![1, 0, 1, 7, 0, 42],
            vec![9, 7, 42],
        ),
    ] {
        let (program, before) = lower(source);
        assert_eq!(before, 1, "{source}");
        check(&program, &input, &expected);
    }
}

#[test]
fn calls_portals_early_returns_and_abort_preserve_effect_order() {
    let (program, _) = lower(
        "cell seen;cell tick(){seen+=1;return input();}
        cell work(){cell[2] a;cell i=input();a[0]=3;a[1]=4;
            if(tick()){output(7);}else{output(8);}
            output(tick()&&input());output(input()||tick());
            while(a[i]){a[i]-=1;if(a[i]==1){return seen;}}
            return 0;}
        void main(){output(work());output(seen);if(input()){abort();}output(99);}",
    );
    check(&program, &[1, 1, 0, 1, 0], &[7, 0, 1, 2, 2, 99]);
    check(&program, &[0, 0, 1, 5, 0, 7, 1], &[8, 1, 1, 3, 3]);
}

#[test]
fn dead_effects_and_constant_logical_operands_need_no_dispatch() {
    let (program, before) = lower(
        "cell die(){abort();}void main(){cell n=input();while(n){
        if(0){abort();}while(0){output(die());}output(0&&die());output(1||die());
        output(1&&input());output(0||input());n-=1;
    }}",
    );
    assert_eq!(before, 2); // One for main, one for the still-reachable die declaration.
    check(&program, &[2, 3, 0, 0, 4], &[0, 1, 1, 0, 0, 1, 0, 1]);
}

#[test]
fn selfhost_hex_serializer_keeps_frame_control_and_exact_output() {
    let source = format!(
        "const cell COMPRESSED_BF_OUTPUT=1;
         struct WideValue{{cell low;cell mid;cell high;}}
         macro compiler_output(value){{output(value);}}
         {}
         void main(){{while(input()){{
             WideValue n;n.low=input();n.mid=input();n.high=input();
             emit_repeat_wide('>',n);output('.');
         }}}}",
        include_str!("../../../selfhost/stage2/compiler/09_bf_serialization.bfc")
    );
    let (program, _) = lower(&source);
    for (name, expected_count) in [("emit_repeat_wide", 4), ("emit_repeat_character", 1)] {
        let function = program
            .functions()
            .iter()
            .find(|f| f.name() == Some(name))
            .unwrap();
        assert_eq!(
            program
                .continuations()
                .iter()
                .filter(|c| c.function() == function.id())
                .count(),
            expected_count,
            "{name}"
        );
    }
    let mut input = Vec::new();
    let mut expected = String::new();
    for count in (0..=255u32).chain([
        256, 999, 1000, 65535, 65536, 99999, 100000, 999999, 1000000, 9999999, 10000000, 16777215,
    ]) {
        input.extend([1, count as u8, (count >> 8) as u8, (count >> 16) as u8]);
        if count != 0 {
            expected.push('>');
            if count > 255 {
                expected += &format!("{count:06x}");
            } else if count > 1 {
                expected += &format!("{count:x}");
            }
        }
        expected.push('.');
    }
    input.push(0);
    check(&program, &input, expected.as_bytes());
}
