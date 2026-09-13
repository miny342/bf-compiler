use std::collections::BTreeMap;

use bf_compiler::{
    AbiConfig, AbiField, PROTOCOL_CELLS, StaticLayout, ValueType, compile_source,
    lower_continuations_with_config, lower_source,
};
use bf_interpreter::run;

fn execute(source: &str, input: &[u8], chunk_cells: usize) -> Vec<u8> {
    let program = lower_source(source).expect("version 1 source must lower");
    let config = AbiConfig::new(chunk_cells).unwrap();
    let brainfuck = lower_continuations_with_config(&program, config)
        .expect("version 1 continuation IR must compile")
        .to_source();
    run(brainfuck.as_bytes(), input).expect("generated Brainfuck must execute")
}

fn assert_both(source: &str, input: &[u8], expected: &[u8]) {
    {
        let chunk_cells = 16;
        assert_eq!(
            execute(source, input, chunk_cells),
            expected,
            "D={chunk_cells}"
        );
    }
}

fn logical_offset_from_head(config: AbiConfig, logical: usize) -> usize {
    (logical / config.chunk_cells()) * config.stride() + 1 + logical % config.chunk_cells()
}

/// Run source while placing raw canaries in the selected global aggregate's
/// aux heads and tail padding. After normal program termination, normalize the
/// pointer from main's ACTIVE cell to the anchor and output every protocol,
/// head, and padding probe directly. Protocol fields start at zero and must be
/// zero again after all portal operations.
fn execute_with_global_region_canaries(
    source: &str,
    source_output: &[u8],
    chunk_cells: usize,
) -> (Vec<u8>, Vec<u8>) {
    let program = lower_source(source).expect("canary source must lower");
    let config = AbiConfig::new(chunk_cells).unwrap();
    let layout = StaticLayout::new(config, program.globals()).unwrap();
    let root = program.globals()[0];
    let payload_cells = match root.value_type() {
        ValueType::Array(cells) | ValueType::Aggregate { cells } => cells,
        actual => panic!("expected aggregate root, got {actual:?}"),
    };
    let base = layout.aggregate_base_head(root.id()).unwrap();
    let chunks = layout.aggregate_chunk_count(root.id()).unwrap();

    // position -> (initial value, expected final value). `None` means the
    // location must retain its required initial zero until the accessor uses
    // and clears it.
    let mut probes = BTreeMap::new();
    for field in 0..PROTOCOL_CELLS {
        probes.insert(base + logical_offset_from_head(config, field), (None, 0_u8));
    }
    for chunk in 0..chunks {
        let canary = 0x40 + u8::try_from(chunk).unwrap();
        probes.insert(base + chunk * config.stride(), (Some(canary), canary));
    }
    let first_padding = PROTOCOL_CELLS + payload_cells;
    let logical_capacity = chunks * config.chunk_cells();
    for (ordinal, logical) in (first_padding..logical_capacity).enumerate() {
        let canary = 0x60 + u8::try_from(ordinal).unwrap();
        probes.insert(
            base + logical_offset_from_head(config, logical),
            (Some(canary), canary),
        );
    }

    let mut prelude = String::new();
    let mut position = 0;
    for (&target, &(initial, _)) in &probes {
        let Some(initial) = initial else { continue };
        prelude.push_str(&">".repeat(target - position));
        prelude.push_str(&"+".repeat(usize::from(initial)));
        position = target;
    }
    prelude.push_str(&"<".repeat(position));

    let generated = lower_continuations_with_config(&program, config)
        .expect("canary continuation IR must compile")
        .to_source();

    // The trampoline exits on main's ACTIVE cell. Move to its context head,
    // then scan the allocation-flag lane leftward to the zero anchor.
    let mut epilogue = "<".repeat(logical_offset_from_head(config, AbiField::Active.index()));
    epilogue.push('[');
    epilogue.push_str(&"<".repeat(config.stride()));
    epilogue.push(']');

    let mut expected = source_output.to_vec();
    position = layout.anchor_head();
    for (&target, &(_, final_value)) in probes.iter().rev() {
        assert!(target <= position);
        epilogue.push_str(&"<".repeat(position - target));
        epilogue.push('.');
        position = target;
        expected.push(final_value);
    }

    let brainfuck = format!("{prelude}{generated}{epilogue}");
    let actual = run(brainfuck.as_bytes(), &[]).expect("canary Brainfuck must execute");
    (actual, expected)
}

#[test]
fn nominal_types_methods_and_constant_projections_have_value_semantics() {
    let source = r#"
        const cell N = 3;
        enum Kind { Empty, Ready = 7 }
        struct Pair { cell left; cell right; }
        struct Record { Kind kind; Pair pair; cell[N] bytes; }

        Record update(Record value, cell byte) {
            value.kind = Kind::Ready;
            value.pair.right = byte;
            value.bytes[2] = byte + 1;
            return value;
        }

        void main() {
            Record before;
            Record after = before.update(40);
            output(before.kind == Kind::Empty);
            output(after.kind == Kind::Ready);
            output(after.pair.right);
            output(after.bytes[2]);
        }
    "#;
    assert_both(source, &[], &[1, 1, 40, 41]);
}

#[test]
fn dynamic_struct_access_crosses_the_255_to_256_offset_boundary() {
    let source = r#"
        struct Triple { cell a; cell b; cell c; }
        Triple[100] global_values;

        void main() {
            cell index = 85;
            global_values[index].b = 'G';
            output(global_values[index].b);
        }
    "#;
    // Triple 85 starts at logical offset 255; field b is offset 256.
    assert_both(source, &[], b"G");
}

#[test]
fn dynamic_projection_supports_an_element_stride_larger_than_255() {
    let source = r#"
        struct Large {
            cell[150] first;
            cell[150] second;
        }
        Large[2] values;

        void main() {
            cell index = 1;
            values[index].second[0] = 'S';
            output(values[index].second[0]);
        }
    "#;
    // 1 * cells(Large) + offset(second) = 300 + 150 = 450.
    assert_both(source, &[], b"S");
}

#[test]
fn overlapping_dynamic_copy_preserves_value_and_region_non_payload_cells() {
    let source = r#"
        struct Packet { cell[7] bytes; }
        Packet[3] root;

        void main() {
            cell source = 2;
            cell destination = 2;
            root[source].bytes[0] = 'A';
            root[source].bytes[5] = 'F';
            root[source].bytes[6] = 'Z';

            // Exact overlap must first snapshot all seven payload cells. A
            // destructive destination-first copy would erase its own source.
            root[destination] = root[source];
            output(root[destination].bytes[0]);
            output(root[destination].bytes[5]);
            output(root[destination].bytes[6]);

            // A second copy within the same root must also be a value copy;
            // mutating the source afterward cannot change the destination.
            destination = 1;
            root[destination] = root[source];
            root[source].bytes[0] = 'X';
            output(root[destination].bytes[0]);
            output(root[destination].bytes[5]);
            output(root[destination].bytes[6]);
            output(root[source].bytes[0]);
            output(root[source].bytes[6]);
        }
    "#;

    // Packet 2 occupies flattened offsets 14..=20. Thus its dynamic aggregate
    // copy crosses a payload chunk boundary for D=16, and byte 6
    // is the root payload's final cell. The root also has tail padding in both
    // geometry (11 cells for D=16).
    {
        let chunk_cells = 16;
        let (actual, expected) =
            execute_with_global_region_canaries(source, b"AFZAFZXZ", chunk_cells);
        assert_eq!(actual, expected, "D={chunk_cells}");
    }
}

#[test]
fn local_portal_preserves_stack_heads_across_self_copy_and_call() {
    let source = r#"
        struct Packet { cell[7] bytes; }

        void emit(cell value) {
            output(value);
        }

        void main() {
            Packet[3] root;
            cell index = 2;
            root[index].bytes[6] = 'Z';
            root[index] = root[index];
            emit(root[index].bytes[6]);
            root[index].bytes[0] = 'A';
            output(root[index].bytes[0]);
            output(root[index].bytes[6]);
        }
    "#;

    // The local root's aligned chunk heads are live stack allocation flags.
    // Calling `emit` after the overlapping portal copy forces frame allocation
    // and flag-lane traversal before the root is accessed again.
    assert_both(source, &[], b"ZAZ");
}

#[test]
fn rhs_precedes_left_to_right_multidimensional_indices() {
    let source = r#"
        void main() {
            cell[4][4] values;
            values[input()][input()] = input();
            output(values[1][2]);
        }
    "#;
    assert_both(source, &[9, 1, 2], &[9]);
}

#[test]
fn zero_sized_aggregates_copy_call_and_return_without_storage() {
    let source = r#"
        struct Zero { cell[0] bytes; }
        Zero[0] none;

        Zero identity(Zero value) {
            return value;
        }

        void main() {
            Zero first;
            Zero second = identity(first);
            first = second;
            output('z');
        }
    "#;
    assert_both(source, &[], b"z");
}

#[test]
fn strings_len_and_hygienic_macros_need_no_runtime_string_type() {
    let source = r#"
        cell marker = 'D';
        cell[] prefix = "P\0";

        macro print(value) {
            cell index;
            while (index < len(value)) {
                output(value[index]);
                index += 1;
            }
        }

        macro emit_with_marker(value) {
            output(marker);
            output(value);
        }

        cell[3] identity(cell[3] value) {
            return value;
        }

        void main() {
            cell marker = 'L';
            cell[] empty = "";
            cell[] message = "A\0B";
            cell[3] returned = identity("C\0D");
            print!(prefix);
            print!(message);
            print!(returned);
            emit_with_marker!('x');
            output(len(empty));
            output(len(message));
        }
    "#;
    assert_both(
        source,
        &[],
        &[b'P', 0, b'A', 0, b'B', b'C', 0, b'D', b'D', b'x', 0, 3],
    );
}

#[test]
fn len_constant_does_not_execute_its_operand() {
    let source = r#"
        cell calls;

        cell[3] touch() {
            cell[3] result;
            calls += 1;
            return result;
        }

        const cell N = len(touch());

        void main() {
            output(N);
            output(calls);
        }
    "#;
    assert_both(source, &[], &[3, 0]);
}

#[test]
fn abort_from_a_deep_activation_stops_the_entire_trampoline() {
    let source = r#"
        void descend(cell depth) {
            output('x');
            if (depth != 0) {
                descend(depth - 1);
            } else {
                abort();
            }
            output('r');
        }

        void main() {
            descend(2);
            output('n');
        }
    "#;
    assert_both(source, &[], b"xxx");
}

#[test]
fn return_inside_a_macro_returns_from_the_expansion_target() {
    let source = r#"
        macro finish() {
            output('r');
            return;
        }

        void helper() {
            finish!();
            output('x');
        }

        void main() {
            helper();
            output('m');
        }
    "#;
    assert_both(source, &[], b"rm");
}

#[test]
fn abort_during_global_initialization_never_enters_main() {
    let source = r#"
        cell initialized = initialize();

        cell initialize() {
            output('i');
            abort();
        }

        void main() {
            output('m');
        }
    "#;
    assert_both(source, &[], b"i");
}

#[test]
fn invalid_version_one_programs_are_compile_errors() {
    let cases = [
        "enum Bad { One = 1 } void main() {}",
        "struct Bad { Bad child; } void main() {}",
        "void main() { cell[] missing; }",
        "void main() { cell[2] wrong = \"abc\"; }",
        "enum A { Zero } enum B { Zero } void main() { A a; B b; if (a == b) {} }",
        "void main() { cell[256] values; output(len(values)); }",
        "cell[2][3] values; const cell N = len(values[2]); void main() {}",
    ];
    for source in cases {
        assert!(
            lower_source(source).is_err(),
            "source unexpectedly compiled: {source}"
        );
    }
}

#[test]
fn a_valid_but_unplaceable_large_type_is_rejected_by_the_target_layout() {
    let source = "cell[255][255] huge; void main() {}";
    assert!(
        lower_source(source).is_ok(),
        "the source type itself is valid"
    );
    assert!(
        compile_source(source).is_err(),
        "the 30,000-cell target cannot place it"
    );
}
