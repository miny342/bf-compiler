//! Differential tests use explicit CIR targets and bypass HIR inlining.
use super::region_probe::measure;
use super::*;
use crate::continuation_inline::{InlineStats, inline_selected};
use crate::{
    FrameAggregateDescriptor, FrameAggregateId, FrameInstruction as I, LogicalOffset,
    ParameterLocation,
};

fn source(source: &str) -> ContinuationProgram {
    let ast = crate::parser::parse(crate::lexer::lex(source).unwrap()).unwrap();
    let ast = crate::macro_expansion::expand(ast).unwrap();
    let hir = crate::semantic::analyze(&ast).unwrap();
    crate::continuation_lowering::lower_hir_unallocated(&hir).unwrap()
}

fn named(program: &ContinuationProgram, names: &[&str]) -> Vec<FunctionId> {
    names
        .iter()
        .map(|name| {
            program
                .functions()
                .iter()
                .find(|f| f.name() == Some(name))
                .unwrap()
                .id()
        })
        .collect()
}

fn compare(
    program: &ContinuationProgram,
    selected: &[FunctionId],
    input: &[u8],
    expected: &[u8],
) -> (ContinuationProgram, InlineStats) {
    let (inlined, stats) = inline_selected(program, selected).unwrap();
    for p in [program, &inlined] {
        let mut output = Vec::new();
        crate::run_continuations_with_io(
            p,
            &mut &input[..],
            &mut output,
            Default::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(output, expected, "virtual graph semantics");
    }
    let (before, _) = crate::continuation_pipeline::finish(program, Default::default()).unwrap();
    let (after, _) = crate::continuation_pipeline::finish(&inlined, Default::default()).unwrap();
    let old = measure(&before, input, true);
    let new = measure(&after, input, true);
    assert_eq!(old.output, expected);
    assert_eq!(new.output, expected);
    assert_eq!(measure(&after, input, false).output, expected);
    eprintln!(
        "inline: sites={} blocks={} Calls={}->{} BF visits={}->{} bytes={}->{} raw={}->{} RLE={}->{}",
        stats.calls_inlined,
        stats.blocks_cloned,
        old.semantic.calls,
        new.semantic.calls,
        old.visits.values().sum::<u64>(),
        new.visits.values().sum::<u64>(),
        old.bytes,
        new.bytes,
        old.instructions,
        new.instructions,
        old.rle_instructions,
        new.rle_instructions
    );
    let old_plan = RegionPlan::new(&before);
    let new_plan = RegionPlan::new(&after);
    let old_layouts =
        build_layouts_with_regions(&before, AbiConfig::default(), Some(&old_plan)).unwrap();
    let new_layouts =
        build_layouts_with_regions(&after, AbiConfig::default(), Some(&new_plan)).unwrap();
    for f in after.functions() {
        let old = before.function(f.id()).unwrap();
        eprintln!(
            "inline frame {}: slots={}->{} aggregates={}->{} outbox={}->{} chunks={}->{}",
            f.name()
                .map(str::to_owned)
                .unwrap_or_else(|| f.id().index().to_string()),
            old.frame_slots(),
            f.frame_slots(),
            old.frame_aggregates().len(),
            f.frame_aggregates().len(),
            old.outbox_cells(),
            f.outbox_cells(),
            old_layouts[&f.id()].frame.frame_chunks(),
            new_layouts[&f.id()].frame.frame_chunks()
        );
    }
    (after, stats)
}

#[test]
fn yielding_and_early_return_inline_preserves_inner_calls_and_iteration_order() {
    let p = source(
        "cell leaf(cell x) { output(76); return x+1; } cell work(cell x) { if(x==1) { return leaf(x); } output(65); cell y=leaf(x); output(66); return y+1; } void main() { cell n=input(); while(n) { output(work(n)); n-=1; } }",
    );
    let selected = named(&p, &["work"]);
    for (input, expected) in [
        (&[0][..], &b""[..]),
        (&[1][..], &b"L\x02"[..]),
        (&[2][..], &b"ALB\x04L\x02"[..]),
    ] {
        let (after, stats) = compare(&p, &selected, input, expected);
        assert_eq!(stats.calls_inlined, 1);
        assert!(after.functions().iter().all(|f| f.name() != Some("work")));
        assert!(
            after
                .continuations()
                .iter()
                .any(|c| matches!(c.terminator(), Terminator::Call { .. }))
        );
    }
}

#[test]
fn multiple_callers_and_nested_inline_have_fresh_storage() {
    let p = source(
        "cell leaf(cell x) { return x+1; } cell f(cell x) { cell a=x; if(x) { a=leaf(x); } return a; } cell a(cell x) { cell local=30; return f(x)+local; } cell b(cell x) { cell local=40; return f(x)+local; } void main() { output(a(input())); output(b(input())); }",
    );
    for names in [&["f"][..], &["leaf", "f", "a", "b"][..]] {
        let (after, stats) = compare(&p, &named(&p, names), &[2, 3], &[33, 44]);
        assert!(stats.calls_inlined >= 2);
        if names.len() == 4 {
            assert!(
                after
                    .continuations()
                    .iter()
                    .all(|c| !matches!(c.terminator(), Terminator::Call { .. }))
            );
        }
    }
}

#[test]
fn aggregate_arguments_nested_results_portals_and_abort_survive_inline() {
    let p = source(
        "struct Pair { cell a; cell b; } Pair seed; Pair leaf(cell n) { Pair p; p.a=n; p.b=n+1; return p; } Pair work(Pair p, cell n) { Pair q=leaf(n); if(n) { return q; } return p; } void main() { Pair[2] a; seed.a=8; seed.b=9; a[input()]=work(seed,input()); output(a[0].a); output(a[0].b); output(a[1].a); output(a[1].b); }",
    );
    let selected = named(&p, &["work"]);
    compare(&p, &selected, &[3, 1], &[0, 0, 3, 4]); // RHS consumes n before the index.
    compare(&p, &selected, &[0, 0], &[8, 9, 0, 0]);
    let p = source(
        "void stop(cell n) { if(n) { output(65); abort(); } output(66); return; } void main() { stop(input()); output(67); }",
    );
    compare(&p, &named(&p, &["stop"]), &[1], b"A");
    compare(&p, &named(&p, &["stop"]), &[0], b"BC");
}

#[test]
fn direct_and_mutual_recursive_sccs_are_never_inlined() {
    let p = source(
        "cell down(cell n) { if(n) { return down(n-1)+1; } return 0; } cell a(cell n) { if(n) { return b(n-1)+1; } return 0; } cell b(cell n) { if(n) { return a(n-1)+1; } return 0; } void main() { output(down(input())); output(a(input())); }",
    );
    let (after, stats) = inline_selected(&p, &named(&p, &["down", "a", "b"])).unwrap();
    assert_eq!(after, p);
    assert_eq!(stats.calls_inlined, 0);
    assert_eq!(stats.recursive_calls_preserved, 5);
    compare(&p, &named(&p, &["down", "a", "b"]), &[2, 3], &[2, 3]);
}

fn id(n: u16) -> ContinuationId {
    ContinuationId::new(n).unwrap()
}
fn slot(n: usize) -> Address {
    Address::Frame(FrameSlot::new(n))
}
fn aggregate(n: usize) -> AggregateRegion {
    AggregateRegion::Frame(FrameAggregateId::new(n))
}
fn cell(array: AggregateRegion, index: usize) -> Address {
    Address::ArrayElement { array, index }
}

#[test]
fn inline_activation_is_zeroed_on_each_loop_invocation_with_sparse_ids() {
    let main = FunctionId::new(0);
    let tick = FunctionId::new(1);
    let p = ContinuationProgram::new(
        main,
        vec![
            FunctionDescriptor::new(main, vec![], 2, ValueType::Void, id(1)),
            FunctionDescriptor::new(tick, vec![], 1, ValueType::Cell, id(65535)),
        ],
        vec![
            Continuation::new(
                id(1),
                main,
                vec![I::Input { dst: slot(0) }],
                Terminator::Goto { target: id(2) },
            ),
            Continuation::new(
                id(2),
                main,
                vec![],
                Terminator::Call {
                    callee: tick,
                    arguments: vec![],
                    return_to: id(3),
                },
            ),
            Continuation::new(
                id(3),
                main,
                vec![
                    I::Output {
                        src: Address::AbiValue,
                    },
                    I::AddConst {
                        dst: slot(0),
                        value: 255,
                    },
                    I::Copy {
                        src: slot(0),
                        dst: slot(1),
                    },
                ],
                Terminator::Branch {
                    condition: slot(1),
                    then_target: id(2),
                    else_target: id(4),
                },
            ),
            Continuation::new(id(4), main, vec![], Terminator::Halt),
            Continuation::new(
                id(65535),
                tick,
                vec![I::AddConst {
                    dst: slot(0),
                    value: 1,
                }],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(slot(0))),
                },
            ),
        ],
    )
    .unwrap();
    let (after, stats) = compare(&p, &[tick], &[3], &[1, 1, 1]);
    assert_eq!(stats.calls_inlined, 1);
    assert_eq!(measure(&after, &[3], true).visits.values().sum::<u64>(), 1);
}

#[test]
fn nested_aggregate_call_preserves_outer_outbox_tail_and_scalar_zeroing() {
    let main = FunctionId::new(0);
    let outer = FunctionId::new(1);
    let inner = FunctionId::new(2);
    let empty = FunctionId::new(3);
    let out = AggregateRegion::Outbox;
    let mut init = (0..4)
        .map(|index| I::Set {
            dst: cell(out, index),
            value: 10 + index as u8,
        })
        .collect::<Vec<_>>();
    init.push(I::Set {
        dst: Address::AbiValue,
        value: 77,
    });
    let mut read = (0..4)
        .map(|index| I::Output {
            src: cell(out, index),
        })
        .collect::<Vec<_>>();
    read.push(I::Output {
        src: Address::AbiValue,
    });
    let mut before_void = read.clone();
    before_void.push(I::Set {
        dst: Address::AbiValue,
        value: 55,
    });
    let p = ContinuationProgram::new(
        main,
        vec![
            FunctionDescriptor::new_aggregates(main, vec![], 0, vec![], 4, ValueType::Void, id(1)),
            FunctionDescriptor::new_aggregates(
                outer,
                vec![],
                0,
                vec![],
                3,
                ValueType::Aggregate { cells: 1 },
                id(4),
            ),
            FunctionDescriptor::new_aggregates(
                inner,
                vec![],
                0,
                vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 3)],
                0,
                ValueType::Aggregate { cells: 3 },
                id(6),
            ),
            FunctionDescriptor::new(empty, vec![], 0, ValueType::Void, id(7)),
        ],
        vec![
            Continuation::new(
                id(1),
                main,
                init,
                Terminator::Call {
                    callee: outer,
                    arguments: vec![],
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                before_void,
                Terminator::Call {
                    callee: empty,
                    arguments: vec![],
                    return_to: id(3),
                },
            ),
            Continuation::new(id(3), main, read, Terminator::Halt),
            Continuation::new(
                id(4),
                outer,
                vec![],
                Terminator::Call {
                    callee: inner,
                    arguments: vec![],
                    return_to: id(5),
                },
            ),
            Continuation::new(
                id(5),
                outer,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Aggregate {
                        region: out,
                        offset: 1,
                        cells: 1,
                    }),
                },
            ),
            Continuation::new(
                id(6),
                inner,
                (0..3)
                    .map(|index| I::Set {
                        dst: cell(aggregate(0), index),
                        value: 99 - index as u8,
                    })
                    .collect(),
                Terminator::Return {
                    value: Some(ValueOperand::aggregate(aggregate(0), 3)),
                },
            ),
            Continuation::new(id(7), empty, vec![], Terminator::Return { value: None }),
        ],
    )
    .unwrap();
    for targets in [
        &[outer][..],
        &[outer, empty][..],
        &[outer, inner, empty][..],
    ] {
        let (after, _) = compare(&p, targets, &[], &[98, 11, 12, 13, 0, 98, 11, 12, 13, 0]);
        assert_eq!(
            after.function(main).unwrap().outbox_cells(),
            if targets.contains(&inner) { 0 } else { 3 }
        );
    }
}

#[test]
fn aggregate_parameter_subranges_and_aliased_scalar_parameters_keep_value_semantics() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let p = ContinuationProgram::new(
        main,
        vec![
            FunctionDescriptor::new_aggregates(
                main,
                vec![],
                1,
                vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 5)],
                0,
                ValueType::Void,
                id(1),
            ),
            FunctionDescriptor::new_aggregates(
                callee,
                vec![
                    ParameterLocation::Aggregate(FrameAggregateId::new(0)),
                    ParameterLocation::AggregateElement {
                        aggregate: FrameAggregateId::new(0),
                        index: 1,
                    },
                    ParameterLocation::Cell(FrameSlot::new(0)),
                ],
                3,
                vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 3)],
                0,
                ValueType::Cell,
                id(3),
            ),
        ],
        vec![
            Continuation::new(
                id(1),
                main,
                (0..5)
                    .map(|index| I::Set {
                        dst: cell(aggregate(0), index),
                        value: 40 + index as u8,
                    })
                    .chain([I::Input { dst: slot(0) }])
                    .collect(),
                Terminator::Call {
                    callee,
                    arguments: vec![
                        ValueOperand::Aggregate {
                            region: aggregate(0),
                            offset: 1,
                            cells: 3,
                        },
                        ValueOperand::Cell(cell(aggregate(0), 0)),
                        ValueOperand::Cell(slot(0)),
                    ],
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                vec![
                    I::Output {
                        src: Address::AbiValue,
                    },
                    I::Output {
                        src: cell(aggregate(0), 2),
                    },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                id(3),
                callee,
                vec![],
                Terminator::AggregateLoad {
                    source: aggregate(0),
                    offset: LogicalOffset::new(slot(0), slot(1)),
                    destination: ValueOperand::Cell(slot(2)),
                    cells: 1,
                    return_to: id(4),
                },
            ),
            Continuation::new(
                id(4),
                callee,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(slot(2))),
                },
            ),
        ],
    )
    .unwrap();
    for (input, expected) in [(0, 41), (1, 40), (2, 43)] {
        let (after, _) = compare(&p, &[callee], &[input], &[expected, 42]);
        assert!(
            after
                .continuations()
                .iter()
                .any(|c| matches!(c.terminator(), Terminator::AggregateLoad { .. }))
        );
    }
}

#[test]
fn inline_keeps_short_circuit_evaluation_and_zero_sized_returns() {
    let p = source(
        "cell read() { output(82); return input(); } void main() { output(input() && read()); output(input() || read()); }",
    );
    compare(&p, &named(&p, &["read"]), &[0, 7], &[0, 1]);
    compare(&p, &named(&p, &["read"]), &[1, 2, 0, 3], &[82, 1, 82, 1]);
    let p = source(
        "struct Empty { cell[0] cells; } Empty f(Empty p) { output(65); return p; } void main() { Empty e; e=f(e); e=f(e); }",
    );
    compare(&p, &named(&p, &["f"]), &[], b"AA");
}

#[test]
fn inline_preserves_callee_sources_through_graph_and_arithmetic_fusion() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let call_span = SourceSpan {
        file_id: 0,
        start_byte: 10,
        end_byte: 20,
    };
    let callee_span = SourceSpan {
        file_id: 1,
        start_byte: 100,
        end_byte: 110,
    };
    let p = ContinuationProgram::new(
        main,
        vec![
            FunctionDescriptor::new(main, vec![], 0, ValueType::Void, id(1)),
            FunctionDescriptor::new(callee, vec![], 1, ValueType::Cell, id(3)),
        ],
        vec![
            Continuation::new(
                id(1),
                main,
                vec![],
                Terminator::Call {
                    callee,
                    arguments: vec![],
                    return_to: id(2),
                },
            )
            .with_source_spans(vec![], Some(call_span)),
            Continuation::new(
                id(2),
                main,
                vec![I::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                id(3),
                callee,
                vec![I::AddConst {
                    dst: slot(0),
                    value: 65,
                }],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(slot(0))),
                },
            )
            .with_source_spans(vec![Some(callee_span)], Some(callee_span)),
        ],
    )
    .unwrap()
    .with_source_files(vec![
        crate::SourceFileDescriptor {
            id: 0,
            path: "caller.bfc".into(),
        },
        crate::SourceFileDescriptor {
            id: 1,
            path: "callee.bfc".into(),
        },
    ]);
    let (after, _) = compare(&p, &[callee], &[], b"A");
    let mut saw_callee = false;
    let mut saw_call = false;
    for c in after.continuations() {
        for (instruction, source) in c.body().iter().zip(c.body_sources()) {
            if matches!(instruction, I::AddConst { value: 65, .. }) {
                assert_eq!(*source, Some(callee_span));
                saw_callee = true;
            }
            saw_call |= *source == Some(call_span);
        }
    }
    assert!(saw_callee && saw_call);
    let artifact = lower_continuations_annotated_with_options(
        &after,
        AbiConfig::default(),
        true,
        ProfileGranularity::Source,
        false,
        true,
    )
    .unwrap()
    .profile_artifact(false);
    assert!(artifact.map.sites.iter().any(|site| {
        site.source
            .as_ref()
            .is_some_and(|s| s.start_byte == 100 && s.end_byte == 110)
    }));
}

#[test]
fn structured_branch_consumes_its_condition_after_either_arm() {
    let main = FunctionId::new(0);
    let p = ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new(
            main,
            vec![],
            1,
            ValueType::Void,
            id(1),
        )],
        vec![Continuation::new(
            id(1),
            main,
            vec![
                I::Input { dst: slot(0) },
                I::Branch {
                    condition: slot(0),
                    then_body: vec![
                        I::Set {
                            dst: slot(0),
                            value: 7,
                        },
                        I::Output { src: slot(0) },
                    ],
                    else_body: vec![
                        I::Set {
                            dst: slot(0),
                            value: 9,
                        },
                        I::Output { src: slot(0) },
                    ],
                },
                I::Output { src: slot(0) },
            ],
            Terminator::Halt,
        )],
    )
    .unwrap();
    for (input, expected) in [(0, 9), (1, 7)] {
        for enabled in [false, true] {
            assert_eq!(measure(&p, &[input], enabled).output, [expected, 0]);
        }
        let cleaned = crate::virtual_cleanup::cleanup(&p).unwrap();
        assert_eq!(measure(&cleaned, &[input], true).output, [expected, 0]);
    }
}

#[test]
fn automatic_cir_inline_compares_with_hir_cheap_cases() {
    let cases: [(&str, &str, &[u8], &[u8]); 7] = [
        (
            "void-global",
            "cell total; void add(cell v) { if(v) { total+=v; } } void main() { cell v=3; while(v) { add(v); add(v); v-=1; } output(total); }",
            &[],
            &[12],
        ),
        (
            "nested-global",
            "cell total; void bump(cell v) { total+=v; } void wrap(cell v) { if(v) { bump(v); } } void main() { wrap(1); wrap(2); output(total); }",
            &[],
            &[3],
        ),
        (
            "scalar-pure",
            "cell add1(cell v) { cell r=v+1; return r; } void main() { cell v=3; output(add1(v)); output(add1(v)); }",
            &[],
            &[4, 4],
        ),
        (
            "input-arg",
            "cell add1(cell v) { return v+1; } void main() { output(add1(input())); }",
            &[9],
            &[10],
        ),
        (
            "input-body",
            "cell add_input(cell v) { return v+input(); } void main() { output(add_input(3)); }",
            &[4],
            &[7],
        ),
        (
            "scalar-loop",
            "cell twice(cell v) { cell r=v+v; return r; } void main() { cell v=input(); while(v) { output(twice(v)); v-=1; } }",
            &[3],
            &[6, 4, 2],
        ),
        (
            "yield-early",
            "cell leaf(cell v) { output(76); return v+1; } cell work(cell v) { if(v==1) { return leaf(v); } output(65); cell y=leaf(v); output(66); return y+1; } void main() { cell n=input(); while(n) { output(work(n)); n-=1; } }",
            &[2],
            b"ALB\x04L\x02",
        ),
    ];
    for (label, text, input, expected) in cases {
        let ast = crate::parser::parse(crate::lexer::lex(text).unwrap()).unwrap();
        let mut hir = crate::semantic::analyze(&ast).unwrap();
        let plain = crate::continuation_lowering::lower_hir_unallocated(&hir).unwrap();
        crate::hir_inline::inline_single_use_functions(&mut hir);
        let hir = crate::continuation_lowering::lower_hir_unallocated(&hir).unwrap();
        let (auto, stats) =
            crate::continuation_inline::inline_automatic(&plain, Default::default()).unwrap();
        let (hir, _) = crate::continuation_pipeline::finish(&hir, Default::default()).unwrap();
        let (auto, _) = crate::continuation_pipeline::finish(&auto, Default::default()).unwrap();
        let before = measure(&hir, input, true);
        let after = measure(&auto, input, true);
        assert_eq!(before.output, expected, "{label}");
        assert_eq!(after.output, expected, "{label}");
        let hir_frame = estimated_frame_chunks(&hir).unwrap()[&hir.main()];
        let cir_frame = estimated_frame_chunks(&auto).unwrap()[&auto.main()];
        eprintln!(
            "HIR->CIR {label}: accepted={} frame rejected={} calls={}->{} visits={}->{} frame={}->{} bytes={}->{} raw={}->{} RLE={}->{}",
            stats.calls_inlined,
            stats.frame_limit_calls_preserved,
            before.semantic.calls,
            after.semantic.calls,
            before.visits.values().sum::<u64>(),
            after.visits.values().sum::<u64>(),
            hir_frame,
            cir_frame,
            before.bytes,
            after.bytes,
            before.instructions,
            after.instructions,
            before.rle_instructions,
            after.rle_instructions
        );
        assert!(after.semantic.calls <= before.semantic.calls, "{label}");
    }
}

#[test]
fn automatic_inline_limits_transitive_global_frame_growth() {
    let locals = (0..17)
        .map(|n| format!("cell v{n}=input();"))
        .collect::<String>();
    let outputs = (0..17)
        .map(|n| format!("output(v{n});"))
        .collect::<String>();
    for global in [true, false] {
        let prefix = if global {
            "cell g; void touch() { output(g); }"
        } else {
            "void touch() { output(65); }"
        };
        let p = source(&format!(
            "{prefix} void leaf() {{ {locals} touch(); {outputs} }} void main() {{ cell saved=input(); leaf(); output(saved); }}"
        ));
        let (auto, stats) =
            crate::continuation_inline::inline_automatic(&p, Default::default()).unwrap();
        let (before, _) = crate::continuation_pipeline::finish(&p, Default::default()).unwrap();
        let (after, _) = crate::continuation_pipeline::finish(&auto, Default::default()).unwrap();
        let before_frames = estimated_frame_chunks(&before).unwrap();
        let after_frames = estimated_frame_chunks(&after).unwrap();
        if global {
            assert!(stats.frame_limit_calls_preserved > 0);
            for (&id, &size) in &after_frames {
                assert!(size <= before_frames[&id]);
            }
        } else {
            assert!(after_frames[&after.main()] > before_frames[&before.main()]);
            assert!(
                after
                    .continuations()
                    .iter()
                    .all(|c| !matches!(c.terminator(), Terminator::Call { .. }))
            );
        }
        let mut input = vec![1; 18];
        input[0] = 9;
        let mut expected = vec![if global { 0 } else { 65 }];
        expected.extend([1; 17]);
        expected.push(9);
        assert_eq!(measure(&after, &input, true).output, expected);
        eprintln!("global={global} frame guard: {stats:?}");
    }
}

#[test]
fn readonly_parameter_aliases_preserve_frame_and_abi_snapshots() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    for reverse in [false, true] {
        let mut arguments = vec![
            ValueOperand::Cell(slot(0)),
            ValueOperand::Cell(Address::AbiValue),
        ];
        if reverse {
            arguments.reverse();
        }
        let program = ContinuationProgram::new(
            main,
            vec![
                FunctionDescriptor::new(main, vec![], 1, ValueType::Void, id(1)),
                FunctionDescriptor::new(
                    callee,
                    vec![FrameSlot::new(0), FrameSlot::new(1)],
                    2,
                    ValueType::Cell,
                    id(3),
                ),
            ],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![
                        I::Set {
                            dst: slot(0),
                            value: 11,
                        },
                        I::Set {
                            dst: Address::AbiValue,
                            value: 22,
                        },
                    ],
                    Terminator::Call {
                        callee,
                        arguments,
                        return_to: id(2),
                    },
                ),
                Continuation::new(
                    id(2),
                    main,
                    vec![
                        I::Output {
                            src: Address::AbiValue,
                        },
                        I::Output { src: slot(0) },
                    ],
                    Terminator::Halt,
                ),
                Continuation::new(
                    id(3),
                    callee,
                    vec![],
                    Terminator::Return {
                        value: Some(ValueOperand::Cell(slot(1))),
                    },
                ),
            ],
        )
        .unwrap();
        compare(
            &program,
            &[callee],
            &[],
            &[if reverse { 11 } else { 22 }, 11],
        );
    }
    let p = source(
        "cell g; cell change() { g=9; return 7; } cell f(cell a, cell b) { cell ignored=change(); b+=1; return a+b; } void main() { g=3; cell n=4; output(f(g,n)); output(n); output(g); }",
    );
    compare(&p, &named(&p, &["f", "change"]), &[], &[8, 4, 9]);
}
