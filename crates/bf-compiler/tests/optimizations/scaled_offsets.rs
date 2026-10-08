use bf_compiler::*;

fn has_compare(body: &[FrameInstruction]) -> bool {
    body.iter().any(|i| match i {
        FrameInstruction::Compare { .. } => true,
        FrameInstruction::Loop { body, .. } => has_compare(body),
        FrameInstruction::Branch {
            then_body,
            else_body,
            ..
        } => has_compare(then_body) || has_compare(else_body),
        _ => false,
    })
}

fn check(program: &ContinuationProgram, input: &[u8], expected: &[u8]) {
    let mut actual = Vec::new();
    run_continuations_with_io(
        program,
        &mut &input[..],
        &mut actual,
        Default::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(actual, expected);
    for options in [
        AbiCodegenOptions::default(),
        AbiCodegenOptions {
            nibble_transfer: true,
            inplace_compare: true,
            anchor_bank: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            static_frames: false,
            ..Default::default()
        },
    ] {
        let bf = lower_continuations_with_codegen_options(program, options).unwrap();
        let mut compressed = Vec::new();
        optimize_bf(&bf)
            .write_compressed_source(&mut compressed)
            .unwrap();
        assert_eq!(bf_interpreter::run(&compressed, input).unwrap(), expected);
    }
}

#[test]
fn small_array_strides_cover_all_indices_nonzero_bases_and_nested_offsets() {
    let mut cases = Vec::new();
    for stride in 2..=8 {
        cases.push((
            format!("struct Value {{ cell[{stride}] bytes; }} Value[256] values;"),
            format!("values[i].bytes[{}]", stride - 1),
        ));
    }
    cases.extend([
        ("struct Value { cell[3] bytes; } Value[2][256] values;".into(),"values[j][i].bytes[2]".into()),
        ("struct Value { cell[3] bytes; } Value[256][2] values;".into(),"values[i][j].bytes[2]".into()),
        ("struct Value { cell[3] bytes; } struct Padded { cell[255] prefix; Value[256] items; } Padded values;".into(),"values.items[i].bytes[2]".into()),
    ]);
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for shift in [0, 255u8] {
        for i in 0..=255u8 {
            let j = i & 1;
            let value = i.wrapping_mul(13).wrapping_add(shift);
            input.extend([1, i, j, value]);
            expected.extend([value, i, j, value]);
        }
    }
    input.push(0);
    for (declarations, place) in cases {
        let source = format!(
            "{declarations} void main() {{ while(input()!=0) {{ cell i=input();cell j=input();cell value=input(); {place}=value; output({place});output(i);output(j);output(value); }} }}"
        );
        check(&lower_source(&source).unwrap(), &input, &expected);
    }
}

#[test]
fn binary_cir_small_scaled_offsets_preserve_source_consumption_and_word_wrap() {
    use SelfhostCirInstruction as I;
    for amount in 2..=8 {
        for initial_low in [Some(0), Some(127), Some(255), None] {
            let mut instructions = vec![I::Input { destination: 3 }, I::LocalOpen { condition: 3 }];
            instructions.push(
                initial_low.map_or(I::Input { destination: 0 }, |value| I::Set {
                    destination: 0,
                    value,
                }),
            );
            instructions.extend([
                I::Input { destination: 1 },
                I::Input { destination: 2 },
                I::Copy {
                    destination: 4,
                    source: 2,
                },
                I::OffsetAddScaled {
                    low: 0,
                    high: 1,
                    source: 2,
                    amount,
                },
                I::Output { source: 0 },
                I::Output { source: 1 },
                I::Output { source: 2 },
                I::Output { source: 4 },
                I::Input { destination: 3 },
                I::LocalClose { condition: 3 },
            ]);
            let wire = SelfhostCirProgram::new(
                0,
                0,
                vec![SelfhostCirFunction {
                    id: 0,
                    entry: 1,
                    frame_cells: 5,
                    return_type: SelfhostCirReturnType::Void,
                    parameters: vec![],
                }],
                vec![SelfhostCirContinuation {
                    id: 1,
                    function: 0,
                    instructions,
                    terminator: SelfhostCirTerminator::Halt,
                }],
            )
            .unwrap();
            let decoded = SelfhostCirProgram::decode(&wire.encode().unwrap()).unwrap();
            let program = lower_selfhost_cir(&decoded).unwrap();
            // The generic wire expansion has no Compare. This also checks that
            // the optimization actually runs through the public input entry.
            assert_eq!(
                program
                    .continuations()
                    .iter()
                    .any(|c| has_compare(c.body())),
                initial_low.is_some()
            );
            let mut input = Vec::new();
            let mut expected = Vec::new();
            for source in 0..=255u8 {
                let low = initial_low.unwrap_or(source.wrapping_mul(19));
                let high = source.wrapping_mul(97).wrapping_add(255);
                input.push(1);
                if initial_low.is_none() {
                    input.push(low);
                }
                input.extend([high, source]);
                let word = u16::from_le_bytes([low, high]).wrapping_add(u16::from(source) * amount);
                expected.extend(word.to_le_bytes());
                expected.extend([0, source]);
            }
            input.push(0);
            check(&program, &input, &expected);
        }
    }
}
