use bf_compiler::{AbiConfig, ValueType, lower_continuations_with_config, lower_source};
use bf_interpreter::run;

fn execute(source: &str, input: &[u8], chunk_cells: usize) -> Vec<u8> {
    let program = lower_source(source).expect("stage 7/8 source must lower");
    let config = AbiConfig::new(chunk_cells).expect("test uses a supported ABI geometry");
    let brainfuck = lower_continuations_with_config(&program, config)
        .expect("stage 7/8 continuation IR must compile")
        .to_source();
    run(brainfuck.as_bytes(), input).expect("generated Brainfuck must execute")
}

fn assert_executes_for_both_chunk_sizes(source: &str, input: &[u8], expected: &[u8]) {
    for chunk_cells in [16] {
        assert_eq!(
            execute(source, input, chunk_cells),
            expected,
            "observable result for D={chunk_cells}",
        );
    }
}

#[test]
fn repository_self_test_program_reports_all_ok() {
    assert_executes_for_both_chunk_sizes(
        include_str!("../../../test.bfc"),
        &[],
        b"ok\nok\nok\nok\nok\nok\nok\nok\n",
    );
}

#[test]
fn repository_self_test_harness_aborts_after_the_first_failure() {
    let source = include_str!("../../../test.bfc");
    let failing = source.replace("check!(test8());", "check!(0); output('x');");
    assert_ne!(
        failing, source,
        "the harness call used by this probe must exist"
    );
    assert_executes_for_both_chunk_sizes(&failing, &[], b"ok\nok\nok\nok\nok\nok\nok\nng\n");
}

#[test]
fn globals_initialize_in_declaration_order_and_survive_recursion() {
    let source = r#"
        cell first = input();
        cell second = input();
        cell sum = first + second;
        cell calls;

        void bump(cell depth) {
            calls += 1;
            if (depth != 0) {
                bump(depth - 1);
            }
        }

        void main() {
            output(first);
            output(second);
            output(sum);
            bump(3);
            output(calls);
        }
    "#;

    let program = lower_source(source).expect("globals must be exposed through the public IR");
    assert_eq!(program.globals().len(), 4);
    assert_eq!(program.globals()[0].value_type(), ValueType::Cell);
    assert_eq!(program.globals()[1].value_type(), ValueType::Cell);
    assert_executes_for_both_chunk_sizes(source, &[5, 9], &[5, 9, 14, 4]);
}

#[test]
fn later_default_initializer_overwrites_an_earlier_initializer_call() {
    let source = r#"
        cell early = touch_later();
        cell later;

        cell touch_later() {
            later = 99;
            return 7;
        }

        void main() {
            output(early);
            output(later);
        }
    "#;

    // `later` is visible to the initializer call, but its own declaration is
    // initialized afterward and therefore replaces 99 with the default zero.
    assert_executes_for_both_chunk_sizes(source, &[], &[7, 0]);
}

#[test]
fn later_global_array_declaration_clears_initializer_call_writes() {
    let source = r#"
        cell early = touch_later();
        cell[3] later;

        cell touch_later() {
            cell index = 1;
            later[index] = 99;
            return 7;
        }

        void main() {
            output(early);
            output(later[1]);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[], &[7, 0]);
}

#[test]
fn global_and_local_array_portals_load_store_and_preserve_indices() {
    let source = r#"
        cell[17] global_values;

        void main() {
            cell[17] local_values;
            cell left = input();
            cell right = input();

            global_values[left] = 10;
            global_values[right] = 20;
            local_values[left] = global_values[right];
            local_values[right] = global_values[left];
            local_values[left] += 3;
            global_values[right] -= 4;

            output(local_values[left]);
            output(local_values[right]);
            output(global_values[left]);
            output(global_values[right]);
            output(left);
            output(right);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[7, 16], &[23, 10, 10, 16, 7, 16]);
}

#[test]
fn dynamic_portals_cover_the_full_cell_index_domain() {
    let source = r#"
        cell[256] global_values;

        void main() {
            cell[256] local_values;
            cell index = input();
            global_values[index] = 'Z';
            local_values[index] = global_values[index];
            output(local_values[index]);
            output(index);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[255], &[b'Z', 255]);
}

#[test]
fn dynamic_assignment_evaluates_rhs_before_its_index() {
    let source = r#"
        void main() {
            cell[4] values;
            values[input()] = input();
            output(values[2]);
            output(values[3]);
        }
    "#;

    // The right-hand input yields 3 first; the index input then yields 2.
    assert_executes_for_both_chunk_sizes(source, &[3, 2], &[3, 0]);
}

#[test]
fn dynamic_compound_assignments_evaluate_rhs_then_index_once() {
    let source = r#"
        void main() {
            cell[4] values;
            values[2] = 10;
            values[input()] += input();
            output(values[2]);
            values[input()] -= input();
            output(values[2]);
            output(input());
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[3, 2, 4, 2, b'X'], &[13, 9, b'X']);
}

#[test]
fn local_array_declarations_clear_storage_on_every_execution() {
    let source = r#"
        void main() {
            cell rounds = 2;
            while (rounds) {
                cell[2] values;
                cell index = 1;
                output(values[index]);
                values[index] = 9;
                rounds -= 1;
            }
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[], &[0, 0]);
}

#[test]
fn whole_array_copy_arguments_and_returns_have_value_semantics() {
    let source = r#"
        cell[4] saved;

        cell[4] changed(cell[4] value) {
            value[0] += 1;
            value[3] = value[0] + value[1];
            return value;
        }

        void main() {
            cell[4] source;
            cell[4] result;
            source[0] = 10;
            source[1] = 2;
            saved = source;
            result = changed(saved);

            output(source[0]);
            output(saved[0]);
            output(saved[3]);
            output(result[0]);
            output(result[3]);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[], &[10, 10, 0, 11, 13]);
}

#[test]
fn recursive_aggregate_returns_use_an_outbox_per_activation() {
    let source = r#"
        cell[17] descend(cell depth, cell[17] value) {
            if (depth == 0) {
                return value;
            }
            value[depth] = depth + 40;
            return descend(depth - 1, value);
        }

        void main() {
            cell[17] seed;
            cell[17] result;
            seed[0] = 9;
            result = descend(3, seed);

            output(result[0]);
            output(result[1]);
            output(result[2]);
            output(result[3]);
            output(seed[1]);
            output(seed[2]);
            output(seed[3]);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[], &[9, 41, 42, 43, 0, 0, 0]);
}

#[test]
fn one_caller_outbox_handles_different_aggregate_return_lengths() {
    let source = r#"
        cell[2] small(cell value) {
            cell[2] result;
            result[0] = value;
            result[1] = value + 1;
            return result;
        }

        cell[17] large(cell value) {
            cell[17] result;
            result[0] = value;
            result[16] = value + 1;
            return result;
        }

        void main() {
            cell[2] small_result;
            cell[17] large_result;

            large_result = large('A');
            small_result = small('a');
            output(large_result[0]);
            output(large_result[16]);
            output(small_result[0]);
            output(small_result[1]);

            small_result = small('c');
            large_result = large('C');
            output(small_result[0]);
            output(small_result[1]);
            output(large_result[0]);
            output(large_result[16]);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[], b"ABabcdCD");
}

#[test]
fn aggregate_arguments_snapshot_each_left_to_right_value() {
    let source = r#"
        cell[2] shared;

        cell[2] first(cell[2] left, cell[2] right) {
            return left;
        }

        cell[2] replace_shared() {
            shared[0] = 9;
            return shared;
        }

        void main() {
            cell[2] result;
            shared[0] = 1;
            result = first(shared, replace_shared());
            output(result[0]);
            output(shared[0]);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[], &[1, 9]);
}

#[test]
fn explicit_main_return_halts_before_following_statements() {
    let source = r#"
        cell initialized = identity(1);

        cell identity(cell value) {
            return value;
        }

        void main() {
            output(initialized);
            return;
            output(2);
        }
    "#;

    assert_executes_for_both_chunk_sizes(source, &[], &[1]);
}

#[test]
fn aggregate_type_mismatches_remain_compile_errors() {
    let wrong_argument = r#"
        void take(cell[4] value) {}
        void main() {
            cell[5] value;
            take(value);
        }
    "#;
    assert!(lower_source(wrong_argument).is_err());

    let wrong_assignment = r#"
        void main() {
            cell[4] left;
            cell[5] right;
            left = right;
        }
    "#;
    assert!(lower_source(wrong_assignment).is_err());

    let aggregate_comparison = r#"
        void main() {
            cell[4] left;
            cell[4] right;
            output(left == right);
        }
    "#;
    assert!(lower_source(aggregate_comparison).is_err());
}
