//! Regression coverage for portal request transport and profile counters.
use super::*;

#[test]
fn page_selectors_materialize_outward_and_return_digits_for_every_byte() {
    let program = crate::lower_source("void main() {}").unwrap();
    let config = AbiConfig::default();
    let layouts = build_layouts(&program, config).unwrap();
    let static_layout = StaticLayout::new(config, program.globals()).unwrap();
    let portal = PortalPlan::new(&program).unwrap();
    let make_source = |direct| {
        let mut emitter = AbiEmitter::new(
            &program,
            &layouts,
            &static_layout,
            &portal,
            config,
            ProfileGranularity::Abi,
        );
        for offset in 1..17 {
            emitter.set(offset, 31 + offset as u8);
        }
        emitter.move_to(0);
        emitter.emit_operation(AnnotatedBfOperation::Input);
        let body = emitter
            .capture(|emitter| {
                emitter.move_to(emitter.current_abi_offset(AbiField::Scratch0)?);
                emitter.emit_operation(AnnotatedBfOperation::Input);
                for field in [
                    AbiField::Scratch1,
                    AbiField::Scratch2,
                    AbiField::Restore,
                    AbiField::Scratch3,
                    AbiField::Branch,
                ] {
                    emitter.set_abi_field(field, 173)?;
                }
                if direct {
                    emitter.split_page_selectors()?;
                } else {
                    emitter.split_abi_nibbles(
                        AbiField::Scratch0,
                        AbiField::Scratch1,
                        AbiField::Scratch2,
                    )?;
                    emitter.copy(
                        emitter.current_abi_offset(AbiField::Scratch1)?,
                        emitter.current_abi_offset(AbiField::Restore)?,
                        emitter.current_abi_offset(AbiField::Scratch0)?,
                    );
                    emitter.copy(
                        emitter.current_abi_offset(AbiField::Scratch2)?,
                        emitter.current_abi_offset(AbiField::Scratch3)?,
                        emitter.current_abi_offset(AbiField::Scratch0)?,
                    );
                    emitter.clear_abi_field(AbiField::Branch)?;
                }
                for offset in 0..17 {
                    emitter.move_to(offset);
                    emitter.emit_operation(AnnotatedBfOperation::Output);
                }
                emitter.move_to(0);
                emitter.emit_operation(AnnotatedBfOperation::Input);
                Ok(())
            })
            .unwrap();
        emitter.emit_loop(body);
        optimize_annotated_bf(&AnnotatedBfProgram::new(emitter.output, emitter.sites)).to_source()
    };
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for value in 0..=255u8 {
        input.extend([1, value]);
        let mut fields: Vec<u8> = (0..17).map(|offset| 31 + offset).collect();
        fields[0] = 1;
        fields[8] = value % 16;
        fields[9] = 0;
        fields[13] = 0;
        fields[14] = value % 16;
        fields[15] = value / 16;
        fields[16] = value / 16;
        expected.extend(fields);
    }
    input.push(0);
    let old = make_source(false);
    let new = make_source(true);
    for disabled in [false, true] {
        let options = bf_interpreter::RunOptions {
            disable_clear: disabled,
            disable_scan: disabled,
            disable_transfer: disabled,
            disable_countdown: disabled,
            disable_remote_transfer: disabled,
            disable_compare: disabled,
            ..Default::default()
        };
        let reference =
            bf_interpreter::run_with_options(old.as_bytes(), &input, options.clone()).unwrap();
        let actual = bf_interpreter::run_with_options(new.as_bytes(), &input, options).unwrap();
        assert_eq!(reference.output, expected);
        assert_eq!(actual.output, expected, "rle_only={disabled}");
        eprintln!(
            "page selectors rle_only={disabled}: RLE {} -> {}, native {} -> {}, transfer {} -> {}, BF {} -> {}",
            reference.stats.executed_rle_instructions,
            actual.stats.executed_rle_instructions,
            reference.stats.optimization.executed_native_operations,
            actual.stats.optimization.executed_native_operations,
            reference.stats.optimization.transfer_iterations,
            actual.stats.optimization.transfer_iterations,
            old.len(),
            new.len()
        );
    }
}

#[test]
fn page_selector_round_trips_cross_both_digits_and_reuse_portals() {
    let program = crate::lower_source(
        "cell[256][256] data; void main() { cell go=input(); while(go) { \
         cell hi=input(); cell lo=input(); cell value=input(); \
         data[hi][lo]=value; output(data[hi][lo]); go=input(); } }",
    )
    .unwrap();
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for value in [0, 1, 173, 255] {
        for (hi, lo) in [(0, 0), (0, 255), (1, 0), (15, 255), (16, 0), (255, 255)] {
            input.extend([1, hi, lo, value]);
            expected.push(value);
        }
    }
    input.push(0);
    for options in [
        AbiCodegenOptions {
            unlimited_tape: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            unlimited_tape: true,
            nibble_transfer: true,
            inplace_compare: true,
            anchor_bank: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            unlimited_tape: true,
            static_frames: true,
            ..Default::default()
        },
    ] {
        let bf = lower_continuations_with_codegen_options(&program, options).unwrap();
        let mut compressed = Vec::new();
        optimize_bf(&bf)
            .write_compressed_source(&mut compressed)
            .unwrap();
        let actual = bf_interpreter::run_with_options(
            &compressed,
            &input,
            bf_interpreter::RunOptions {
                unbounded_tape: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(actual.output, expected, "options={options:?}");
    }
}

#[test]
fn portal_high_zero_test_preserves_every_byte_and_all_live_protocol_fields() {
    let program = crate::lower_source("void main() {}").unwrap();
    let config = AbiConfig::default();
    let layouts = build_layouts(&program, config).unwrap();
    let static_layout = StaticLayout::new(config, program.globals()).unwrap();
    let portal = PortalPlan::new(&program).unwrap();
    let mut emitter = AbiEmitter::new(
        &program,
        &layouts,
        &static_layout,
        &portal,
        config,
        ProfileGranularity::Abi,
    );
    for offset in 1..17 {
        emitter.set(offset, 31 + offset as u8);
    }
    emitter.move_to(0);
    emitter.emit_operation(AnnotatedBfOperation::Input);
    let body = emitter
        .capture(|emitter| {
            emitter.move_to(emitter.current_abi_offset(AbiField::Scratch0)?);
            emitter.emit_operation(AnnotatedBfOperation::Input);
            // Explicitly initialize dirty scratch on every request, including after
            // the previous test left Condition/Branch set on the direct path.
            for field in [AbiField::Restore, AbiField::Scratch1, AbiField::Scratch2] {
                emitter.set_abi_field(field, 173)?;
            }
            emitter.emit_portal_direct_test()?;
            for offset in 0..17 {
                emitter.move_to(offset);
                emitter.emit_operation(AnnotatedBfOperation::Output);
            }
            emitter.move_to(0);
            emitter.emit_operation(AnnotatedBfOperation::Input);
            Ok(())
        })
        .unwrap();
    emitter.emit_loop(body);
    let bf =
        optimize_annotated_bf(&AnnotatedBfProgram::new(emitter.output, emitter.sites)).to_source();
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for value in 0..=255u8 {
        input.extend([1, value]);
        let mut fields: Vec<u8> = (0..17).map(|offset| 31 + offset).collect();
        fields[0] = 1;
        fields[7] = u8::from(value == 0);
        fields[8] = 0;
        fields[9] = u8::from(value == 0);
        fields[13] = value;
        fields[14] = 0;
        fields[15] = 0;
        expected.extend(fields);
    }
    input.push(0);
    for disabled in [false, true] {
        let result = bf_interpreter::run_with_options(
            bf.as_bytes(),
            &input,
            bf_interpreter::RunOptions {
                disable_clear: disabled,
                disable_scan: disabled,
                disable_transfer: disabled,
                disable_countdown: disabled,
                disable_remote_transfer: disabled,
                disable_compare: disabled,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.output, expected, "rle_only={disabled}");
    }
}

#[test]
fn sliding_byte_decomposition_preserves_guards_for_every_byte() {
    let program = crate::lower_source("void main() {}").unwrap();
    let config = AbiConfig::default();
    let layouts = build_layouts(&program, config).unwrap();
    let static_layout = StaticLayout::new(config, program.globals()).unwrap();
    let portal = PortalPlan::new(&program).unwrap();
    for source in [1, 300] {
        let mut emitter = AbiEmitter::new(
            &program,
            &layouts,
            &static_layout,
            &portal,
            config,
            ProfileGranularity::Abi,
        );
        let zero = 8;
        let bits = std::array::from_fn(|index| zero + 1 + index as isize);
        // Nonzero neighbours catch either slide crossing the scratch boundary.
        emitter.set(zero - 1, 53);
        emitter.set(bits[7] + 1, 107);
        emitter.move_to(0);
        emitter.emit_operation(AnnotatedBfOperation::Input);
        let body = emitter.capture_infallible(|emitter| {
            emitter.move_to(source);
            emitter.emit_operation(AnnotatedBfOperation::Input);
            for offset in std::iter::once(zero).chain(bits) {
                emitter.set(offset, 173);
                emitter.clear(offset);
            }
            emitter.move_to(source);
            let decompose = emitter.capture_infallible(|emitter| {
                emitter.adjust(255);
                emitter.increment_contiguous_byte_bits(&bits, zero, source);
            });
            emitter.emit_loop(decompose);
            emitter.move_to(0);
            for index in 1..4 {
                emitter.move_static_value(bits[index], bits[0], 1 << index);
            }
            for index in 5..8 {
                emitter.move_static_value(bits[index], bits[4], 1 << (index - 4));
            }
            for offset in [source, zero]
                .into_iter()
                .chain(bits)
                .chain([zero - 1, bits[7] + 1])
            {
                emitter.move_to(offset);
                emitter.emit_operation(AnnotatedBfOperation::Output);
            }
            emitter.move_to(0);
            emitter.emit_operation(AnnotatedBfOperation::Input);
        });
        emitter.emit_loop(body);
        let bf = optimize_annotated_bf(&AnnotatedBfProgram::new(emitter.output, emitter.sites))
            .to_source();
        let mut input = Vec::new();
        let mut expected = Vec::new();
        for value in 0..=255u8 {
            input.extend([1, value]);
            expected.extend([0, 0, value % 16, 0, 0, 0, value / 16, 0, 0, 0, 53, 107]);
        }
        input.push(0);
        for disabled in [false, true] {
            let result = bf_interpreter::run_with_options(
                bf.as_bytes(),
                &input,
                bf_interpreter::RunOptions {
                    disable_clear: disabled,
                    disable_scan: disabled,
                    disable_transfer: disabled,
                    disable_countdown: disabled,
                    disable_remote_transfer: disabled,
                    disable_compare: disabled,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                result.output, expected,
                "source={source}, rle_only={disabled}"
            );
        }
    }
}

#[test]
fn nibble_global_aggregate_returns_survive_recursive_frames_and_repeated_requests() {
    let program = crate::lower_source(
        "struct Item { cell a; cell b; cell c; } Item[17] data; \
         Item fetch(cell depth, cell index) { cell[19] keep; keep[depth]=depth; \
         if (depth) { Item value=fetch(depth-1,index); output(keep[depth]); return value; } \
         return data[index]; } \
         void main() { cell go=input(); while(go) { cell i=input(); \
         data[i].a=input(); data[i].b=input(); data[i].c=input(); \
         Item value=fetch(3,i); output(value.a); output(value.b); output(value.c); \
         go=input(); } }",
    )
    .unwrap();
    let mut input = Vec::new();
    let mut expected = Vec::new();
    for value in 0..=255u8 {
        let bytes = [value, value.wrapping_mul(17), value.wrapping_add(255)];
        input.extend([1, value % 17]);
        input.extend(bytes);
        expected.extend([1, 2, 3]);
        expected.extend(bytes);
    }
    input.push(0);
    for options in [
        AbiCodegenOptions {
            nibble_transfer: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            nibble_transfer: true,
            anchor_bank: true,
            ..Default::default()
        },
        AbiCodegenOptions {
            nibble_transfer: true,
            static_frames: true,
            ..Default::default()
        },
    ] {
        let bf = lower_continuations_with_codegen_options(&program, options).unwrap();
        let mut compressed = Vec::new();
        optimize_bf(&bf)
            .write_compressed_source(&mut compressed)
            .unwrap();
        assert_eq!(
            bf_interpreter::run(&compressed, &input).unwrap(),
            expected,
            "options={options:?}"
        );
    }
}

#[test]
fn return_transport_consumes_every_byte_and_clears_shared_scratch() {
    for (padding, nibble_transfer) in [(16, false), (16, true), (1024, true)] {
        let program = crate::lower_source("cell[16] data; void main() {}").unwrap();
        let config = AbiConfig::default();
        let layouts = build_layouts(&program, config).unwrap();
        let static_layout = StaticLayout::new(config, program.globals()).unwrap();
        let portal = PortalPlan::new(&program).unwrap();
        let mut emitter = AbiEmitter::new(
            &program,
            &layouts,
            &static_layout,
            &portal,
            config,
            ProfileGranularity::Abi,
        );
        emitter.nibble_transfer = nibble_transfer;
        emitter.initialize_main(false).unwrap();
        let extra_chunks = padding / config.chunk_cells();
        for chunk in 1..=extra_chunks {
            emitter.set((chunk * config.stride()) as isize, 1);
        }
        emitter.migrate_context((extra_chunks * config.stride()) as isize);
        let base = static_layout.aggregate_base_head(GlobalId::new(0)).unwrap();
        let value = emitter.current_abi_offset(AbiField::Value).unwrap();
        let condition = emitter.current_abi_offset(AbiField::Active).unwrap();
        emitter.move_to(condition);
        emitter.emit_operation(AnnotatedBfOperation::Input);
        let body = emitter
            .capture(|emitter| {
                emitter.move_to(0);
                emitter.emit_context_to_global(base, config.portal_chunks());
                emitter.move_to(value);
                emitter.emit_operation(AnnotatedBfOperation::Input);
                emitter.move_to(0);
                emitter.move_global_portal_value_to_context(base, value)?;
                emitter.move_to(value);
                emitter.emit_operation(AnnotatedBfOperation::Output);
                emitter.move_to(0);
                emitter.emit_context_to_global(base, config.portal_chunks());
                emitter.move_to(value);
                emitter.emit_operation(AnnotatedBfOperation::Output);
                for index in 0..9 {
                    let scratch = static_layout.remote_copy_scratch_position(index).unwrap();
                    emitter.move_to(scratch as isize - base as isize);
                    emitter.emit_operation(AnnotatedBfOperation::Output);
                }
                emitter.move_to(0);
                emitter.emit_global_to_context(base, config.portal_chunks());
                emitter.move_to(condition);
                emitter.emit_operation(AnnotatedBfOperation::Input);
                Ok(())
            })
            .unwrap();
        emitter.emit_loop(body);
        let bf = AnnotatedBfProgram::new(emitter.output, emitter.sites).into_plain();
        let mut compressed = Vec::new();
        optimize_bf(&bf)
            .write_compressed_source(&mut compressed)
            .unwrap();
        let mut input = Vec::new();
        let mut expected = Vec::new();
        for value in 0..=255u8 {
            input.extend([1, value]);
            expected.push(value);
            expected.extend([0; 10]);
        }
        input.push(0);
        for disabled in [false, true] {
            let result = bf_interpreter::run_with_options(
                &compressed,
                &input,
                bf_interpreter::RunOptions {
                    disable_remote_transfer: disabled,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                result.output, expected,
                "padding={padding}, nibble={nibble_transfer}, disabled={disabled}"
            );
        }
    }
}

#[test]
fn request_transport_preserves_every_byte_and_profile_structure() {
    for (padding, nibble_transfer) in [(16, false), (16, true), (256, false), (256, true)] {
        let (artifact, _) = transport_fixture(padding, nibble_transfer);
        let mut input = Vec::new();
        let mut expected = Vec::new();
        for value in 0..=255u8 {
            input.push(1);
            for field in 0..7u8 {
                let byte = value.wrapping_add(field.wrapping_mul(31));
                input.push(byte);
                expected.push(byte);
            }
        }
        input.push(0);
        assert_eq!(
            bf_interpreter::run(artifact.source.as_bytes(), &input).unwrap(),
            expected
        );
        for disabled in [false, true] {
            let run = bf_interpreter::run_with_options(
                artifact.source.as_bytes(),
                &input,
                bf_interpreter::RunOptions {
                    disable_remote_transfer: disabled,
                    collect_stats: true,
                    profile: Some(bf_interpreter::ProfileOptions {
                        map: artifact.map.clone(),
                        mode: bf_interpreter::ProfileMode::Counters,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(run.output, expected);
            let profile = run.profile.unwrap();
            let transport_loops: u64 = profile
                .sites
                .iter()
                .filter(|site| {
                    artifact
                        .map
                        .sites
                        .iter()
                        .find(|record| record.id == site.site)
                        .unwrap()
                        .stable_key
                        .starts_with("abi.portal.route.transport.")
                })
                .map(|site| site.counters.remote_transfer_loops)
                .sum();
            assert_eq!(
                transport_loops,
                if disabled {
                    0
                } else {
                    // Four nibble fields use two loops each; three unary fields use one.
                    256 * if nibble_transfer { 11 } else { 7 }
                }
            );
            assert_eq!(run.stats.optimization.remote_transfer_fallbacks, 0);
        }
        let json = artifact.map.to_json_pretty().unwrap();
        for key in [
            "abi.portal.route.decompose",
            "abi.portal.route.pack",
            "abi.portal.route.transport.nibble.1",
            "abi.portal.route.transport.unary",
        ] {
            assert_eq!(
                json.contains(key),
                nibble_transfer || key.ends_with("unary")
            );
        }
    }
}

#[test]
fn frame_retained_portal_pcs_cover_all_selectors_and_mixed_frame_portals() {
    use crate::{FrameAggregateDescriptor, FrameAggregateId};
    use FrameInstruction as I;
    let main = FunctionId::new(0);
    let slot = |n| Address::Frame(FrameSlot::new(n));
    let local = AggregateRegion::Frame(FrameAggregateId::new(0));
    let fixture = |count: usize| {
        let globals = (0..count)
            .map(|i| crate::GlobalDescriptor::new(GlobalId::new(i), ValueType::Array(1)))
            .collect();
        let mut start = vec![
            I::Set {
                dst: slot(2),
                value: 173,
            },
            I::Set {
                dst: Address::ArrayElement {
                    array: local,
                    index: 0,
                },
                value: 113,
            },
        ];
        for i in 0..count {
            start.push(I::Set {
                dst: Address::ArrayElement {
                    array: AggregateRegion::Global(GlobalId::new(i)),
                    index: 0,
                },
                value: i as u8,
            });
        }
        let mut nodes = vec![Continuation::new(
            id(1),
            main,
            start,
            Terminator::Goto { target: id(2) },
        )];
        for i in 0..count {
            let load = id((2 + 2 * i) as u16);
            let resume = id((3 + 2 * i) as u16);
            nodes.push(Continuation::new(
                load,
                main,
                vec![],
                Terminator::ArrayLoad {
                    array: AggregateRegion::Global(GlobalId::new(i)),
                    index: slot(0),
                    destination: slot(1),
                    return_to: resume,
                },
            ));
            nodes.push(Continuation::new(
                resume,
                main,
                vec![I::Output { src: slot(1) }, I::Output { src: slot(2) }],
                Terminator::Goto {
                    target: id((4 + 2 * i) as u16),
                },
            ));
        }
        nodes.push(Continuation::new(
            id((2 + 2 * count) as u16),
            main,
            vec![],
            Terminator::ArrayLoad {
                array: local,
                index: slot(0),
                destination: slot(1),
                return_to: id((3 + 2 * count) as u16),
            },
        ));
        nodes.push(Continuation::new(
            id((3 + 2 * count) as u16),
            main,
            vec![I::Output { src: slot(1) }],
            Terminator::Halt,
        ));
        ContinuationProgram::new_with_globals(
            main,
            globals,
            vec![FunctionDescriptor::new_aggregates(
                main,
                vec![],
                3,
                vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 1)],
                0,
                ValueType::Void,
                id(1),
            )],
            nodes,
        )
        .unwrap()
    };
    let program = fixture(256);
    let plan = PortalPlan::with_frame_returns(&program, true).unwrap();
    assert!(plan.frame_returns);
    assert_eq!(plan.return_globals.len(), 256);
    assert_eq!(plan.routers.last().unwrap().return_selector, Some(255));
    let ids: Vec<_> = plan
        .accessors
        .iter()
        .map(|a| a.id)
        .chain(plan.routers.iter().map(|r| r.id))
        .chain(plan.ordered_sites.iter().map(|s| s.resume))
        .collect();
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
    let fallback = PortalPlan::with_frame_returns(&fixture(257), true).unwrap();
    assert!(!fallback.frame_returns);
    assert!(fallback.return_globals.is_empty());
    let expected: Vec<_> = (0..=255u8).flat_map(|n| [n, 173]).chain([113]).collect();
    for options in [
        AbiCodegenOptions {
            static_frames: false,
            ..Default::default()
        },
        AbiCodegenOptions {
            nibble_transfer: true,
            anchor_bank: true,
            ..Default::default()
        },
    ] {
        let bf = lower_continuations_with_codegen_options(&program, options).unwrap();
        let mut compressed = Vec::new();
        optimize_bf(&bf)
            .write_compressed_source(&mut compressed)
            .unwrap();
        let mut counts = None;
        for disabled in [false, true] {
            let actual = bf_interpreter::run_with_options(
                &compressed,
                &[],
                bf_interpreter::RunOptions {
                    disable_clear: disabled,
                    disable_scan: disabled,
                    disable_transfer: disabled,
                    disable_countdown: disabled,
                    disable_remote_transfer: disabled,
                    disable_compare: disabled,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(actual.output, expected);
            let logical = (
                actual.stats.executed_instructions,
                actual.stats.executed_rle_instructions,
            );
            if let Some(previous) = counts {
                assert_eq!(logical, previous);
            }
            counts = Some(logical);
        }
    }
}
