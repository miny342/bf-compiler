//! Differential execution and measurements for the private B0/B1 experiment.
use super::*;
use bf_interpreter::{ProfileMode, ProfileOptions, RunOptions};

pub(super) struct Measurement {
    pub(super) output: Vec<u8>,
    pub(super) visits: HashMap<ContinuationId, u64>,
    hidden_visits: u64,
    pub(super) bytes: usize,
    pub(super) instructions: u64,
    pub(super) rle_instructions: u64,
    selector_instructions: u64,
    selector_rle_instructions: u64,
    pub(super) semantic: crate::ContinuationRunStats,
}

pub(super) fn measure(program: &ContinuationProgram, input: &[u8], enabled: bool) -> Measurement {
    let mut expected = Vec::new();
    let semantic = crate::run_continuations_with_io(
        program,
        &mut &input[..],
        &mut expected,
        crate::ContinuationRunOptions {
            collect_transitions: true,
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    let annotated = lower_continuations_annotated_with_options(
        program,
        AbiConfig::default(),
        true,
        ProfileGranularity::Source,
        false,
        enabled,
    )
    .unwrap();
    let artifact = optimize_annotated_bf(&annotated).profile_artifact(false);
    let result = bf_interpreter::run_with_options(
        artifact.source.as_bytes(),
        input,
        RunOptions {
            collect_stats: true,
            profile: Some(ProfileOptions {
                map: artifact.map.clone(),
                mode: ProfileMode::Exact,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.output, expected, "region emission = {enabled}");
    // The ordinary optimizer and interpreter must agree with annotated output.
    let plain = optimize_bf(&annotated.into_plain()).to_source();
    assert_eq!(
        artifact.source, plain,
        "profiling must not change generated BF"
    );
    let unprofiled = bf_interpreter::run_with_options(
        plain.as_bytes(),
        input,
        RunOptions {
            collect_stats: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(unprofiled.output, expected);
    assert_eq!(
        unprofiled.stats.executed_instructions,
        result.stats.executed_instructions
    );
    assert_eq!(
        unprofiled.stats.executed_rle_instructions,
        result.stats.executed_rle_instructions
    );

    let plan = enabled.then(|| RegionPlan::new(program));
    let mut visits = HashMap::new();
    let mut terminals = HashMap::<ContinuationId, u64>::new();
    let mut hidden_visits = 0;
    let mut inputs = 0;
    let mut selector_instructions = 0;
    let mut selector_rle_instructions = 0;
    for site in &result.profile.as_ref().unwrap().sites {
        let key = &artifact
            .map
            .sites
            .iter()
            .find(|s| s.id == site.site)
            .unwrap()
            .stable_key;
        inputs += site.counters.input_operations;
        if key == "abi.region.select" || key.starts_with("abi.region.enter.") {
            selector_instructions += site.counters.raw_bf_instructions;
            selector_rle_instructions += site.counters.rle_instructions;
        }
        if let Some(id) = key.strip_prefix("abi.dispatch.enter.") {
            let id = ContinuationId::new(id.parse().unwrap()).unwrap();
            if program.continuation(id).is_some() {
                *visits.entry(id).or_default() += site.counters.loop_iterations;
                if plan.as_ref().is_none_or(|p| !p.regions.contains_key(&id)) {
                    *terminals.entry(id).or_default() += site.counters.loop_iterations;
                }
            } else {
                hidden_visits += site.counters.loop_iterations;
            }
        }
        if let Some(id) = key.strip_prefix("abi.region.enter.") {
            let id = ContinuationId::new(id.parse().unwrap()).unwrap();
            *terminals.entry(id).or_default() += site.counters.loop_iterations;
        }
    }
    assert_eq!(inputs, semantic.input_operations, "input consumption");
    for continuation in program.continuations() {
        if continuation.terminator().boundary() != crate::continuation_ir::BoundaryKind::Soft {
            let id = continuation.id();
            assert_eq!(
                terminals.get(&id).copied().unwrap_or(0),
                semantic.continuation_count(id),
                "terminal at {id:?}, regions={enabled}"
            );
        }
    }
    Measurement {
        output: result.output,
        visits,
        hidden_visits,
        bytes: plain.len(),
        instructions: result.stats.executed_instructions,
        rle_instructions: result.stats.executed_rle_instructions,
        selector_instructions,
        selector_rle_instructions,
        semantic,
    }
}

fn function_visits(
    program: &ContinuationProgram,
    result: &Measurement,
    function: FunctionId,
) -> u64 {
    result
        .visits
        .iter()
        .filter(|(id, _)| program.continuation(**id).unwrap().function() == function)
        .map(|(_, n)| n)
        .sum()
}

const MARK: &str = "void mark(cell x) { if(x) { output(120); return; } output(121); }";

#[test]
fn two_call_loop_reaches_five_real_dispatcher_visits() {
    let source = format!(
        "{MARK} void worker(cell n) {{ while(n) {{ output(65); mark(n); output(66); mark(n); output(67); n-=1; }} }} void main() {{ worker(input()); }}"
    );
    let program = crate::lower_source(&source).unwrap();
    let worker = program
        .functions()
        .iter()
        .find(|f| f.name() == Some("worker"))
        .unwrap();
    for n in [0, 1, 2, 3] {
        let before = measure(&program, &[n], false);
        let after = measure(&program, &[n], true);
        assert_eq!(
            function_visits(&program, &after, worker.id()),
            1 + 2 * u64::from(n)
        );
        if n == 2 {
            assert_eq!(function_visits(&program, &before, worker.id()), 8);
            report("two-call", &program, worker.id(), &before, &after);
        }
    }
}

#[test]
fn both_yielding_arms_reach_two_real_dispatcher_visits() {
    let source = format!(
        "{MARK} void worker(cell n) {{ if(n) {{ output(65); mark(n); output(66); }} else {{ output(67); mark(n); output(68); }} output(69); }} void main() {{ worker(input()); }}"
    );
    let program = crate::lower_source(&source).unwrap();
    let worker = program
        .functions()
        .iter()
        .find(|f| f.name() == Some("worker"))
        .unwrap();
    for n in [0, 1, 255] {
        let before = measure(&program, &[n], false);
        let after = measure(&program, &[n], true);
        assert_eq!(function_visits(&program, &before, worker.id()), 4);
        assert_eq!(function_visits(&program, &after, worker.id()), 2);
        if n == 1 {
            report("both-arm", &program, worker.id(), &before, &after);
        }
    }
}

#[test]
fn source_yield_lowering_preserves_dispatch_minimum() {
    let cases: [(&str, &str, &[u8], &[u8]); 6] = [
        (
            "single-call-loop",
            "while(n) { output(65); mark(n); output(66); n-=1; }",
            &[2],
            b"AxBAxB",
        ),
        (
            "one-arm-local",
            "if(n) { n+=1; } else { mark(n); } output(n);",
            &[1],
            &[2],
        ),
        (
            "one-arm-call",
            "if(n) { n+=1; } else { mark(n); } output(n);",
            &[0],
            b"y\0",
        ),
        (
            "inverted-loop",
            "while(n==0) { output(65); n+=1; mark(n); output(66); }",
            &[0],
            b"AxB",
        ),
        (
            "nested-prefix",
            "while(n) { if(input()) { output(65); } else { output(66); } mark(n); cell j=input(); while(j) { output(67); j-=1; } n-=1; }",
            &[2, 1, 2, 0, 0],
            b"AxCCBx",
        ),
        (
            "aggregate-local",
            "cell[2] p; p[0]=n; if(n) { p[1]=p[0]; } else { mark(n); } output(p[0]); output(p[1]);",
            &[1],
            &[1, 1],
        ),
    ];
    for (label, body, input, expected) in cases {
        let source =
            format!("{MARK} void worker(cell n) {{ {body} }} void main() {{ worker(input()); }}");
        let program = crate::lower_source(&source).unwrap();
        let worker = program
            .functions()
            .iter()
            .find(|f| f.name() == Some("worker"))
            .unwrap()
            .id();
        let plan = RegionPlan::new(&program);
        assert_eq!(plan.bounded_fallbacks, 0);
        let before = measure(&program, input, false);
        let after = measure(&program, input, true);
        assert_eq!(before.output, expected, "{label}: source semantics");
        assert_eq!(after.output, expected, "{label}: source semantics");
        let yields = program
            .continuations()
            .iter()
            .filter(|c| {
                c.function() == worker
                    && c.terminator().boundary() == crate::continuation_ir::BoundaryKind::Hard
            })
            .map(|c| before.semantic.continuation_count(c.id()))
            .sum::<u64>();
        assert_eq!(
            function_visits(&program, &after, worker),
            1 + yields,
            "{label}"
        );
        report(label, &program, worker, &before, &after);
    }
}

fn report(
    label: &str,
    program: &ContinuationProgram,
    function: FunctionId,
    before: &Measurement,
    after: &Measurement,
) {
    let plan = RegionPlan::new(program);
    let old_layout = build_layouts(program, AbiConfig::default()).unwrap();
    let layout = build_layouts_with_regions(program, AbiConfig::default(), Some(&plan)).unwrap();
    let f = program.function(function).unwrap();
    eprintln!(
        "{label}: blocks={} entries={}->{} visits={}->{} slots={} aggregates={} outbox={} scratch={}->{} chunks={}->{} BF bytes={}->{} BF instructions={}->{} RLE instructions={}->{} selector={} selector RLE={} hidden={}->{}",
        program
            .continuations()
            .iter()
            .filter(|c| c.function() == function)
            .count(),
        program.continuations().len(),
        plan.entries.len(),
        function_visits(program, before, function),
        function_visits(program, after, function),
        f.frame_slots(),
        f.frame_aggregates().len(),
        f.outbox_cells(),
        old_layout[&function].frame.value_cells() - f.frame_slots(),
        layout[&function].frame.value_cells() - f.frame_slots(),
        old_layout[&function].frame.frame_chunks(),
        layout[&function].frame.frame_chunks(),
        before.bytes,
        after.bytes,
        before.instructions,
        after.instructions,
        before.rle_instructions,
        after.rle_instructions,
        after.selector_instructions,
        after.selector_rle_instructions,
        before.hidden_visits,
        after.hidden_visits
    );
    let mut copies = HashMap::<ContinuationId, usize>::new();
    fn visit(node: &RegionNode, copies: &mut HashMap<ContinuationId, usize>) {
        if matches!(node.flow, RegionFlow::Continue) {
            return;
        }
        *copies.entry(node.id).or_default() += 1;
        match &node.flow {
            RegionFlow::Terminal(_) | RegionFlow::Continue => {}
            RegionFlow::Goto(child) => visit(child, copies),
            RegionFlow::Branch(left, right) => {
                visit(left, copies);
                visit(right, copies);
            }
        }
    }
    for &entry in &plan.entries {
        if let Some(region) = plan.regions.get(&entry) {
            visit(&region.root, &mut copies);
        } else {
            *copies.entry(entry).or_default() += 1;
        }
    }
    fn instructions(body: &[FrameInstruction]) -> usize {
        body.iter()
            .map(|i| {
                1 + match i {
                    FrameInstruction::Loop { body, .. } => instructions(body),
                    FrameInstruction::Branch {
                        then_body,
                        else_body,
                        ..
                    } => instructions(then_body) + instructions(else_body),
                    _ => 0,
                }
            })
            .sum::<usize>()
    }
    let duplicated_blocks = copies.values().map(|n| n - 1).sum::<usize>();
    let duplicated_instructions = copies
        .iter()
        .map(|(id, count)| {
            let c = program.continuation(*id).unwrap();
            (count - 1) * instructions(c.body())
        })
        .sum::<usize>();
    let semantic_visits = program
        .continuations()
        .iter()
        .filter(|c| c.function() == function)
        .map(|c| before.semantic.continuation_count(c.id()))
        .sum::<u64>();
    eprintln!(
        "{label}: CIR visits={semantic_visits} total BF visits={}->{} Call/Return={}/{} duplicated blocks/instructions={duplicated_blocks}/{duplicated_instructions}",
        before.visits.values().sum::<u64>() + before.hidden_visits,
        after.visits.values().sum::<u64>() + after.hidden_visits,
        before.semantic.calls,
        before.semantic.returns
    );
}

fn check_source(source: &str, cases: &[(&[u8], &[u8])]) {
    let program = crate::lower_source(source).unwrap();
    for &(input, expected) in cases {
        let mut output = Vec::new();
        crate::run_continuations_with_io(
            &program,
            &mut &input[..],
            &mut output,
            Default::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(output, expected, "fixture semantics");
        let before = measure(&program, input, false);
        let after = measure(&program, input, true);
        assert_eq!(
            before.hidden_visits, after.hidden_visits,
            "portal protocol visits"
        );
    }
}

#[test]
fn nested_control_and_single_call_loops_preserve_effect_order() {
    check_source(
        &format!(
            "{MARK} void main() {{ cell n=input(); while(n) {{ output(65); mark(n); output(66); n-=1; }} }}"
        ),
        &[(&[0], b""), (&[1], b"AxB"), (&[3], b"AxBAxBAxB")],
    );
    check_source(
        &format!(
            "{MARK} void main() {{ cell n=input(); while(n) {{ if(input()) {{ if(input()) {{ output(65); }} else {{ output(66); }} mark(n); }} else {{ output(67); }} cell j=input(); while(j) {{ output(68); j-=1; }} n-=1; }} }}"
        ),
        &[
            (&[0], b""),
            (&[2, 1, 0, 2, 0, 1], b"BxDDCD"),
            (&[1, 1, 1, 0], b"Ax"),
        ],
    );
    check_source(
        &format!(
            "{MARK} void main() {{ cell n=input(); while(n) {{ cell j=input(); while(j) {{ if(input()) {{ mark(j); }} else {{ output(65); }} j-=1; }} mark(n); n-=1; }} }}"
        ),
        &[
            (&[0], b""),
            (&[1, 0], b"x"),
            (&[2, 2, 1, 0, 1, 0], b"xAxAx"),
        ],
    );
}

#[test]
fn short_circuit_and_early_exit_preserve_calls_and_input_consumption() {
    let source = "cell read() { output(82); cell n=input(); if(n) { return n; } return 0; } void main() { output(input() && read()); output(input() || read()); output(input()); }";
    check_source(
        source,
        &[
            (&[0, 7, 9], &[0, 1, 9]),
            (&[2, 4, 0, 0, 9], &[82, 1, 82, 0, 9]),
            (&[1, 0, 0, 255, 9], &[82, 0, 82, 1, 9]),
        ],
    );
    check_source(
        "void stop(cell n) { if(n) { output(65); abort(); } output(66); } void main() { stop(input()); output(67); }",
        &[(&[0], b"BC"), (&[1], b"A")],
    );
    check_source(
        "cell down(cell n) { if(n) { return down(n-1)+1; } return 0; } void main() { output(down(input())); output(down(input())); }",
        &[(&[0, 1], &[0, 1]), (&[3, 2], &[3, 2])],
    );
    check_source(
        "cell a(cell n) { if(n) { return b(n-1)+1; } return 0; } cell b(cell n) { if(n) { return a(n-1)+1; } return 0; } void main() { output(a(input())); }",
        &[(&[0], &[0]), (&[4], &[4])],
    );
}

#[test]
fn call_and_portal_order_preserves_rhs_snapshots_and_partial_updates() {
    check_source(
        "cell value() { output(86); cell x=input(); if(x) { return x; } return 7; } cell index() { output(73); if(input()) { return 1; } return 0; } void main() { cell[2] a; a[index()]=value(); output(a[0]); output(a[1]); }",
        &[(&[42, 1], &[86, 73, 0, 42]), (&[0, 0], &[86, 73, 7, 0])],
    );
    check_source(
        "struct Pair { cell a; cell b; } Pair seed; Pair[2] values; Pair value() { output(86); if(input()) { return seed; } Pair p; p.a=8; p.b=9; return p; } cell index() { output(73); seed.a=99; if(input()) { return 1; } return 0; } void main() { seed.a=42; seed.b=43; values[0].b=66; values[1].b=67; values[index()]=value(); output(values[0].a); output(values[0].b); output(values[1].a); output(values[1].b); }",
        &[
            (&[1, 1], &[86, 73, 0, 66, 42, 43]),
            (&[0, 0], &[86, 73, 8, 9, 0, 67]),
        ],
    );
    check_source(
        "struct Triple { cell a; cell b; cell c; } Triple[100] global; void main() { Triple[100] local; cell i=input(); if(input()) { global[i].b=65; local[i]=global[i]; } else { local[i].b=66; global[i]=local[i]; } output(local[i].b); output(global[i].b); }",
        &[(&[0, 0], b"BB"), (&[99, 1], b"AA")],
    );
    // Recursion transports aggregate values through a separate outbox in each
    // activation, and subsequent calls reuse the returned frame.
    check_source(
        "struct Pair { cell a; cell b; } Pair down(cell n) { Pair p; if(n) { p=down(n-1); p.a+=1; return p; } p.b=9; return p; } void main() { Pair p; p=down(input()); output(p.a); output(p.b); p=down(input()); output(p.a); output(p.b); }",
        &[(&[2, 1], &[2, 9, 1, 9])],
    );
}

fn id(n: u16) -> ContinuationId {
    ContinuationId::new(n).unwrap()
}
fn slot(n: usize) -> Address {
    Address::Frame(FrameSlot::new(n))
}

#[test]
fn condition_slot_reuse_shared_resume_and_sparse_ids_keep_values_and_sources() {
    let main = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let branch = Terminator::Branch {
        condition: slot(0),
        then_target: id(3),
        else_target: id(4),
    };
    let span = SourceSpan {
        file_id: 0,
        start_byte: 10,
        end_byte: 20,
    };
    let program = ContinuationProgram::new(
        main,
        vec![
            FunctionDescriptor::new(main, vec![], 1, ValueType::Void, id(1)),
            FunctionDescriptor::new(
                callee,
                vec![FrameSlot::new(0)],
                1,
                ValueType::Cell,
                id(65535),
            ),
        ],
        vec![
            Continuation::new(
                id(1),
                main,
                vec![FrameInstruction::Input { dst: slot(0) }],
                branch,
            ),
            Continuation::new(
                id(3),
                main,
                vec![FrameInstruction::Set {
                    dst: slot(0),
                    value: 42,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![ValueOperand::Cell(slot(0))],
                    return_to: id(5),
                },
            )
            .with_source_spans(vec![Some(span)], Some(span)),
            Continuation::new(
                id(4),
                main,
                vec![FrameInstruction::Set {
                    dst: slot(0),
                    value: 43,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![ValueOperand::Cell(slot(0))],
                    return_to: id(5),
                },
            ),
            Continuation::new(
                id(5),
                main,
                vec![
                    FrameInstruction::Output {
                        src: Address::AbiValue,
                    },
                    FrameInstruction::Output { src: slot(0) },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                id(65535),
                callee,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(slot(0))),
                },
            ),
        ],
    )
    .unwrap()
    .with_source_files(vec![crate::SourceFileDescriptor {
        id: 0,
        path: "fixture.bfc".into(),
    }]);
    let plan = RegionPlan::new(&program);
    assert_eq!(plan.entries, HashSet::from([id(1), id(5), id(65535)]));
    for n in [0, 1, 255] {
        measure(&program, &[n], false);
        let after = measure(&program, &[n], true);
        assert_eq!(function_visits(&program, &after, main), 2);
    }
    let annotated = lower_continuations_annotated_with_options(
        &program,
        AbiConfig::default(),
        true,
        ProfileGranularity::Source,
        false,
        true,
    )
    .unwrap();
    let artifact = optimize_annotated_bf(&annotated).profile_artifact(false);
    let copies = artifact
        .map
        .sites
        .iter()
        .filter(|s| s.stable_key == "function.0.continuation.3")
        .collect::<Vec<_>>();
    assert!(
        copies.len() >= 2,
        "local body and deferred terminal keep provenance"
    );
    assert!(copies.iter().all(|s| {
        s.source
            .as_ref()
            .is_some_and(|s| s.start_byte == 10 && s.end_byte == 20)
    }));
}

#[test]
fn soft_cycle_executes_in_one_dispatcher_visit() {
    let main = FunctionId::new(0);
    let program = ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new(
            main,
            vec![],
            2,
            ValueType::Void,
            id(1),
        )],
        vec![
            Continuation::new(
                id(1),
                main,
                vec![FrameInstruction::Input { dst: slot(0) }],
                Terminator::Goto { target: id(2) },
            ),
            Continuation::new(
                id(2),
                main,
                vec![FrameInstruction::Copy {
                    src: slot(0),
                    dst: slot(1),
                }],
                Terminator::Branch {
                    condition: slot(1),
                    then_target: id(3),
                    else_target: id(4),
                },
            ),
            Continuation::new(
                id(3),
                main,
                vec![
                    FrameInstruction::AddConst {
                        dst: slot(0),
                        value: 255,
                    },
                    FrameInstruction::Output { src: slot(0) },
                ],
                Terminator::Goto { target: id(2) },
            ),
            Continuation::new(id(4), main, vec![], Terminator::Halt),
        ],
    )
    .unwrap();
    let plan = RegionPlan::new(&program);
    assert_eq!(plan.entries, HashSet::from([id(1)]));
    assert_eq!(plan.bounded_fallbacks, 0);
    for n in [0, 1, 3, 255] {
        let before = measure(&program, &[n], false);
        let after = measure(&program, &[n], true);
        assert_eq!(before.visits.values().sum::<u64>(), 3 + 2 * u64::from(n));
        assert_eq!(after.visits.values().sum::<u64>(), 1);
    }
}

fn terminal_tree(leaves: u16) -> ContinuationProgram {
    let main = FunctionId::new(0);
    let mut nodes = Vec::new();
    fn tree(
        nodes: &mut Vec<Continuation>,
        leaves: u16,
        value: &mut u8,
        next: &mut u16,
    ) -> ContinuationId {
        let current = id(*next);
        *next += 1;
        let (body, terminal) = if leaves == 1 {
            let body = vec![
                FrameInstruction::Set {
                    dst: slot(0),
                    value: *value,
                },
                FrameInstruction::Output { src: slot(0) },
            ];
            *value = value.wrapping_add(1);
            (body, Terminator::Halt)
        } else {
            let left = tree(nodes, leaves / 2, value, next);
            let right = tree(nodes, leaves - leaves / 2, value, next);
            (
                vec![FrameInstruction::Input { dst: slot(0) }],
                Terminator::Branch {
                    condition: slot(0),
                    then_target: left,
                    else_target: right,
                },
            )
        };
        nodes.push(Continuation::new(
            current,
            FunctionId::new(0),
            body,
            terminal,
        ));
        current
    }
    tree(&mut nodes, leaves, &mut 0, &mut 1);
    ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new(
            main,
            vec![],
            1,
            ValueType::Void,
            id(1),
        )],
        nodes,
    )
    .unwrap()
}

#[test]
fn selector_handles_255_terminals_and_falls_back_at_256() {
    for leaves in [255, 256] {
        let program = terminal_tree(leaves);
        let plan = RegionPlan::new(&program);
        if leaves == 255 {
            assert_eq!(plan.regions[&id(1)].terminals.len(), 255);
            assert_eq!(plan.bounded_fallbacks, 0);
        } else {
            assert!(!plan.regions.contains_key(&id(1)));
            assert_eq!(plan.bounded_fallbacks, 1);
        }
        for input in [[0; 8], [1; 8]] {
            let measured = measure(&program, &input, true);
            assert_eq!(
                measured.visits.values().sum::<u64>(),
                if leaves == 255 { 1 } else { 2 }
            );
        }
    }
}

#[test]
fn deep_paths_and_exponentially_shared_tails_have_bounded_expansion() {
    let main = FunctionId::new(0);
    for (blocks, shared) in [(140, false), (14, true)] {
        let nodes = (1..=blocks)
            .map(|n| {
                Continuation::new(
                    id(n),
                    main,
                    vec![],
                    if n == blocks {
                        Terminator::Halt
                    } else if shared {
                        Terminator::Branch {
                            condition: slot(0),
                            then_target: id(n + 1),
                            else_target: id(n + 1),
                        }
                    } else {
                        Terminator::Goto { target: id(n + 1) }
                    },
                )
            })
            .collect();
        let program = ContinuationProgram::new(
            main,
            vec![FunctionDescriptor::new(
                main,
                vec![],
                1,
                ValueType::Void,
                id(1),
            )],
            nodes,
        )
        .unwrap();
        let plan = RegionPlan::new(&program);
        assert!(plan.bounded_fallbacks > 0);
        assert!(!plan.regions.contains_key(&id(1)));
        fn size(node: &RegionNode) -> usize {
            1 + match &node.flow {
                RegionFlow::Terminal(_) | RegionFlow::Continue => 0,
                RegionFlow::Goto(next) => size(next),
                RegionFlow::Branch(left, right) => size(left) + size(right),
            }
        }
        assert!(plan.regions.values().all(|r| size(&r.root) <= 4096));
        // Exercise the long-chain fallback. The exponential DAG is a planner
        // stress case; emitting thousands of duplicate branches adds no coverage.
        if !shared {
            measure(&program, &[], true);
        }
    }
}

#[test]
fn legacy_array_portals_can_be_selected_after_local_branches() {
    let main = FunctionId::new(0);
    let array = AggregateRegion::Frame(crate::FrameAggregateId::new(0));
    let program = ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new_typed(
            main,
            vec![],
            3,
            vec![crate::FrameAggregateDescriptor::new(
                crate::FrameAggregateId::new(0),
                2,
            )],
            0,
            ValueType::Void,
            id(1),
        )],
        vec![
            Continuation::new(
                id(1),
                main,
                vec![
                    FrameInstruction::Input { dst: slot(0) },
                    FrameInstruction::Input { dst: slot(1) },
                    FrameInstruction::Set {
                        dst: slot(2),
                        value: 77,
                    },
                ],
                Terminator::Branch {
                    condition: slot(0),
                    then_target: id(2),
                    else_target: id(3),
                },
            ),
            Continuation::new(
                id(2),
                main,
                vec![],
                Terminator::ArrayStore {
                    array,
                    index: slot(1),
                    value: slot(2),
                    return_to: id(3),
                },
            ),
            Continuation::new(
                id(3),
                main,
                vec![],
                Terminator::ArrayLoad {
                    array,
                    index: slot(1),
                    destination: slot(2),
                    return_to: id(4),
                },
            ),
            Continuation::new(
                id(4),
                main,
                vec![FrameInstruction::Output { src: slot(2) }],
                Terminator::Halt,
            ),
        ],
    )
    .unwrap();
    for input in [[0, 0], [1, 0], [1, 1]] {
        let before = measure(&program, &input, false);
        let after = measure(&program, &input, true);
        assert_eq!(before.hidden_visits, after.hidden_visits);
    }
}

#[test]
fn frame_growth_and_global_navigation_cost_are_measured_separately() {
    let main = FunctionId::new(0);
    let worker = FunctionId::new(1);
    let global = Address::Global(GlobalId::new(0));
    let body = (0..4)
        .flat_map(|_| {
            [
                FrameInstruction::Copy {
                    src: global,
                    dst: slot(15),
                },
                FrameInstruction::Output { src: slot(15) },
            ]
        })
        .collect::<Vec<_>>();
    let program = ContinuationProgram::new_with_globals(
        main,
        vec![crate::GlobalDescriptor::cell(GlobalId::new(0))],
        vec![
            FunctionDescriptor::new(main, vec![], 0, ValueType::Void, id(1)),
            FunctionDescriptor::new(worker, vec![], 16, ValueType::Cell, id(3)),
        ],
        vec![
            Continuation::new(
                id(1),
                main,
                vec![FrameInstruction::Set {
                    dst: global,
                    value: 255,
                }],
                Terminator::Call {
                    callee: worker,
                    arguments: vec![],
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                id(3),
                worker,
                vec![FrameInstruction::Input { dst: slot(0) }],
                Terminator::Branch {
                    condition: slot(0),
                    then_target: id(4),
                    else_target: id(5),
                },
            ),
            Continuation::new(
                id(4),
                worker,
                body.clone(),
                Terminator::Return {
                    value: Some(ValueOperand::Cell(slot(15))),
                },
            ),
            Continuation::new(
                id(5),
                worker,
                body,
                Terminator::Return {
                    value: Some(ValueOperand::Cell(slot(15))),
                },
            ),
        ],
    )
    .unwrap();
    let plan = RegionPlan::new(&program);
    let old = build_layouts(&program, AbiConfig::default()).unwrap();
    let new = build_layouts_with_regions(&program, AbiConfig::default(), Some(&plan)).unwrap();
    assert_eq!(
        new[&worker].frame.frame_chunks(),
        old[&worker].frame.frame_chunks() + 1
    );
    let before = measure(&program, &[1], false);
    let after = measure(&program, &[1], true);
    assert_eq!(function_visits(&program, &before, worker), 2);
    assert_eq!(function_visits(&program, &after, worker), 1);
    report("global/frame-growth", &program, worker, &before, &after);
}

fn loop_count(node: &RegionNode) -> usize {
    usize::from(node.loop_header)
        + match &node.flow {
            RegionFlow::Continue | RegionFlow::Terminal(_) => 0,
            RegionFlow::Goto(next) => loop_count(next),
            RegionFlow::Branch(left, right) => loop_count(left) + loop_count(right),
        }
}

#[test]
fn optional_calls_in_soft_loops_visit_only_entry_and_hard_resumes() {
    let fixtures = [
        (
            "optional-call",
            format!(
                "{MARK} void worker(cell n) {{ while(n) {{ if(input()) {{ mark(n); }} else {{ output(76); }} output(67); n-=1; }} }} void main() {{ worker(input()); }}"
            ),
            vec![
                vec![0],
                vec![4, 0, 0, 0, 0],
                vec![4, 1, 0, 1, 0],
                vec![4, 1, 1, 1, 1],
            ],
        ),
        (
            "nested-optional-call",
            format!(
                "{MARK} void worker(cell n) {{ while(n) {{ cell j=input(); while(j) {{ if(input()) {{ mark(j); }} else {{ output(76); }} j-=1; }} output(79); n-=1; }} }} void main() {{ worker(input()); }}"
            ),
            vec![
                vec![0],
                vec![2, 0, 0],
                vec![2, 3, 0, 1, 0, 2, 1, 0],
                vec![1, 2, 1, 1],
            ],
        ),
    ];
    for (label, source, cases) in fixtures {
        let program = crate::lower_source(&source).unwrap();
        let worker = program
            .functions()
            .iter()
            .find(|f| f.name() == Some("worker"))
            .unwrap()
            .id();
        let plan = RegionPlan::new(&program);
        assert!(plan.regions.values().any(|r| loop_count(&r.root) > 0));
        assert_eq!(plan.bounded_fallbacks, 0);
        for (index, input) in cases.iter().enumerate() {
            let before = measure(&program, input, false);
            let after = measure(&program, input, true);
            let calls = before.semantic.calls - 1; // Exclude main -> worker.
            assert_eq!(function_visits(&program, &after, worker), 1 + calls);
            if index == 2 {
                report(label, &program, worker, &before, &after);
            }
        }
    }
}

#[test]
fn aggregate_portal_exits_and_reenters_a_soft_cycle_after_full_cleanup() {
    let program = crate::lower_source("struct Pair { cell a; cell b; } Pair[2] g; void worker(cell n) { cell i=input(); Pair p; while(n) { if(input()) { p.a=n; p.b=n+1; g[i]=p; } else { output(76); } output(n); n-=1; } output(g[i].a); output(g[i].b); } void main() { worker(input()); }").unwrap();
    let worker = program
        .functions()
        .iter()
        .find(|f| f.name() == Some("worker"))
        .unwrap()
        .id();
    let plan = RegionPlan::new(&program);
    assert!(plan.regions.values().any(|r| loop_count(&r.root) > 0));
    assert_eq!(plan.bounded_fallbacks, 0);
    for input in [
        &[0, 0][..],
        &[3, 1, 0, 0, 0],
        &[3, 1, 1, 0, 1],
        &[3, 0, 1, 1, 1],
    ] {
        let before = measure(&program, input, false);
        let after = measure(&program, input, true);
        let portals = before.semantic.aggregate_loads + before.semantic.aggregate_stores;
        assert_eq!(function_visits(&program, &after, worker), 1 + portals);
        assert_eq!(before.hidden_visits, after.hidden_visits);
        if input == [3, 1, 1, 0, 1] {
            report("soft-loop-portal", &program, worker, &before, &after);
        }
    }
}

#[test]
fn nested_and_multiple_entry_cycles_match_the_vm_for_bounded_walks() {
    // Every check consumes one unit of a shared budget, so every walk stops.
    // Independent branch edges cover self edges, nested/outer backedges and
    // multiple-entry SCCs. The input steers both arms through those graphs.
    let main = FunctionId::new(0);
    let mut seed = 1_u32;
    for case in 0..32 {
        let mut nodes = vec![Continuation::new(
            id(1),
            main,
            vec![FrameInstruction::Set {
                dst: slot(0),
                value: 6,
            }],
            Terminator::Goto { target: id(2) },
        )];
        for node in 0..3_u16 {
            let check = id(2 + 2 * node);
            let choose = id(3 + 2 * node);
            nodes.push(Continuation::new(
                check,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: slot(1),
                        value: node as u8,
                    },
                    FrameInstruction::Output { src: slot(1) },
                    FrameInstruction::AddConst {
                        dst: slot(0),
                        value: 255,
                    },
                    FrameInstruction::Copy {
                        src: slot(0),
                        dst: slot(1),
                    },
                ],
                Terminator::Branch {
                    condition: slot(1),
                    then_target: choose,
                    else_target: id(8),
                },
            ));
            let targets = if case == 0 {
                // Two independent entries from node 0 into the {1, 2} SCC.
                [[id(4), id(6)], [id(6), id(4)], [id(4), id(6)]][usize::from(node)]
            } else {
                [0, 1].map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    id(2 + 2 * ((seed >> 16) % 3) as u16)
                })
            };
            nodes.push(Continuation::new(
                choose,
                main,
                vec![FrameInstruction::Input { dst: slot(1) }],
                Terminator::Branch {
                    condition: slot(1),
                    then_target: targets[0],
                    else_target: targets[1],
                },
            ));
        }
        nodes.push(Continuation::new(
            id(8),
            main,
            vec![FrameInstruction::Output { src: slot(0) }],
            Terminator::Halt,
        ));
        let program = ContinuationProgram::new(
            main,
            vec![FunctionDescriptor::new(
                main,
                vec![],
                2,
                ValueType::Void,
                id(1),
            )],
            nodes,
        )
        .unwrap();
        let plan = RegionPlan::new(&program);
        assert_eq!(plan.bounded_fallbacks, 0);
        for input in [[0; 5], [1; 5], [0, 1, 0, 1, 0]] {
            measure(&program, &input, false);
            let after = measure(&program, &input, true);
            assert_eq!(after.visits.values().sum::<u64>(), 1);
        }
    }
}

#[test]
fn closed_soft_cycle_needs_no_terminal_selector_dispatch() {
    let main = FunctionId::new(0);
    let program = ContinuationProgram::new(
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
            vec![FrameInstruction::Output { src: slot(0) }],
            Terminator::Goto { target: id(1) },
        )],
    )
    .unwrap();
    let plan = RegionPlan::new(&program);
    assert!(plan.regions[&id(1)].terminals.is_empty());
    let artifact = lower_continuations_annotated_with_options(
        &program,
        AbiConfig::default(),
        true,
        ProfileGranularity::Source,
        false,
        true,
    )
    .unwrap()
    .profile_artifact(false);
    assert!(
        !artifact
            .map
            .sites
            .iter()
            .any(|s| s.stable_key.starts_with("abi.region.enter.")
                || s.stable_key == "abi.region.select")
    );
    artifact
        .map
        .validate_for_source(artifact.source.as_bytes())
        .unwrap();
}
