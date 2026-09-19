use bf_compiler as bfc;

fn arena_source() -> String {
    let source = include_str!("../../../selfhost/stage2/compiler/06_arena.bfc");
    // Exercise the production function without allocating the million-cell
    // arena: arithmetic only needs the type and constant declarations.
    let declarations = source.split_once("WideValue wide_from_cell(").unwrap().0;
    let start = source.find("NodeId arena_advance(").unwrap();
    let end = source[start..].find("\ncell arena_read(").unwrap() + start;
    format!(
        "{declarations}
         void fail(cell group, cell detail){{output(group);output(detail);abort();}}
         {}
         void main(){{while(input()){{
             NodeId position;position.bank=input();position.page=input();position.slot=input();
             cell amount=input();NodeId result=arena_advance(position,amount);
             output(result.bank);output(result.page);output(result.slot);
             output(position.bank);output(position.page);output(position.slot);output(amount);
         }}}}",
        &source[start..end]
    )
}

fn check(program: &bfc::ContinuationProgram, bf: &[u8], input: &[u8], expected: &[u8]) {
    let mut actual = Vec::new();
    bfc::run_continuations_with_io(
        program,
        &mut &input[..],
        &mut actual,
        bfc::ContinuationRunOptions::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(bf_interpreter::run(bf, input).unwrap(), expected);
}

#[test]
fn arena_advance_covers_all_byte_pairs_and_page_bank_boundaries() {
    let program = bfc::lower_source(&arena_source()).unwrap();
    let bf = bfc::compile_continuations(&program).unwrap();
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for (bank, page) in [(7, 237), (14, 254)] {
        for slot in 0..=255u8 {
            for amount in 0..=255u8 {
                let sum = u16::from(slot) + u16::from(amount);
                let carry = u8::from(sum >= 256);
                let bank_carry = u8::from(page == 254) * carry;
                input.extend([1, bank, page, slot, amount]);
                expected.extend([
                    bank + bank_carry,
                    if bank_carry != 0 { 0 } else { page + carry },
                    sum as u8,
                    bank,
                    page,
                    slot,
                    amount,
                ]);
            }
        }
    }
    input.push(0);
    check(&program, bf.as_bytes(), &input, &expected);

    // The final valid cell can still be reached, and advancing by zero is valid.
    check(
        &program,
        bf.as_bytes(),
        &[1, 15, 254, 0, 255, 1, 15, 254, 255, 0, 0],
        &[15, 254, 255, 15, 254, 0, 255, 15, 254, 255, 15, 254, 255, 0],
    );
    // Crossing beyond the last bank retains the production failure code.
    for (slot, amount) in [(255, 1), (1, 255), (255, 255)] {
        check(
            &program,
            bf.as_bytes(),
            &[1, 15, 254, slot, amount, 0],
            b"BP",
        );
    }
}
