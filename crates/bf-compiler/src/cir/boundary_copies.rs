//! Available local copies across CFG, call and portal resume edges.
//!
//! Only call arguments and return operands are forwarded. Other operations retain
//! their storage identities. Calls cannot alias caller frame cells; globals and ABI
//! transport cells are never facts. Every write kills related copy relations.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::cir::effects::{self, Effect};
use crate::{
    Address, AggregateRegion, Continuation, FrameInstruction as I, FunctionDescriptor, Terminator,
    ValueOperand as V,
};

type Facts = HashMap<Address, Address>;
const MAX_DESTINATIONS: usize = 256;
const MAX_AGGREGATE_FIELDS: usize = 16;

fn local(a: Address) -> bool {
    matches!(
        a,
        Address::Frame(_)
            | Address::ArrayElement {
                array: AggregateRegion::Frame(_),
                ..
            }
    )
}
fn element(array: AggregateRegion, index: usize) -> Address {
    Address::ArrayElement { array, index }
}
fn pairs(i: &I, mut visit: impl FnMut(Address, Address)) {
    match *i {
        I::Copy { src, dst } if local(src) && local(dst) => visit(dst, src),
        I::AggregateCopy { src, dst, cells }
            if matches!(src, AggregateRegion::Frame(_))
                && matches!(dst, AggregateRegion::Frame(_)) =>
        {
            for index in 0..cells.min(MAX_AGGREGATE_FIELDS) {
                visit(element(dst, index), element(src, index));
            }
        }
        _ => {}
    }
}
fn destinations(body: &[I], found: &mut HashSet<Address>) {
    for i in body {
        pairs(i, |dst, _| {
            found.insert(dst);
        });
        match i {
            I::Loop { body, .. } => destinations(body, found),
            I::Branch {
                then_body,
                else_body,
                ..
            } => {
                destinations(then_body, found);
                destinations(else_body, found);
            }
            _ => {}
        }
    }
}
fn intersect(mut a: Facts, b: &Facts) -> Facts {
    a.retain(|dst, src| b.get(dst) == Some(src));
    a
}
fn resolve(a: Address, facts: &Facts) -> Address {
    let mut current = a;
    for _ in 0..=facts.len() {
        match facts.get(&current) {
            Some(next) => current = *next,
            None => return current,
        }
    }
    // Copy generation and write kills cannot create a cycle. Fail closed if
    // that invariant is ever weakened by a later transformation.
    a
}
fn overlaps(a: Address, value: V) -> bool {
    match value {
        V::Cell(b) => a == b,
        V::Array(region) => matches!(a, Address::ArrayElement { array, .. } if array == region),
        V::Aggregate {
            region,
            offset,
            cells,
        } => {
            matches!(a, Address::ArrayElement { array, index } if array == region && index >= offset && index - offset < cells)
        }
    }
}
fn kill(facts: &mut Facts, value: V) {
    facts.retain(|dst, src| !overlaps(*dst, value) && !overlaps(*src, value));
}
fn effect(facts: &mut Facts, e: Effect) {
    match e {
        Effect::Write(value) => kill(facts, value),
        Effect::Clobber(a) => kill(facts, V::Cell(a)),
        Effect::MayWrite(region) => kill(facts, V::Array(region)),
        Effect::Read(_) | Effect::CallResult(_) => {}
    }
}
fn aggregate_source(
    region: AggregateRegion,
    offset: usize,
    cells: usize,
    facts: &Facts,
) -> Option<(AggregateRegion, usize)> {
    if cells == 0 || cells > MAX_AGGREGATE_FIELDS || !matches!(region, AggregateRegion::Frame(_)) {
        return None;
    }
    let Address::ArrayElement {
        array,
        index: first,
    } = resolve(element(region, offset), facts)
    else {
        return None;
    };
    (0..cells)
        .all(|i| resolve(element(region, offset + i), facts) == element(array, first + i))
        .then_some((array, first))
}
fn operand(value: V, facts: &Facts, f: &FunctionDescriptor) -> V {
    match value {
        V::Cell(a) => V::Cell(resolve(a, facts)),
        V::Aggregate {
            region,
            offset,
            cells,
        } => aggregate_source(region, offset, cells, facts).map_or(value, |(region, offset)| {
            V::Aggregate {
                region,
                offset,
                cells,
            }
        }),
        V::Array(AggregateRegion::Frame(id)) => {
            let cells = f.frame_aggregate(id).unwrap().cells();
            match aggregate_source(AggregateRegion::Frame(id), 0, cells, facts) {
                Some((region @ AggregateRegion::Frame(src), 0))
                    if f.frame_aggregate(src).unwrap().cells() == cells =>
                {
                    V::Array(region)
                }
                _ => value,
            }
        }
        _ => value,
    }
}
fn terminal(t: &Terminator, facts: &Facts, f: &FunctionDescriptor) -> Terminator {
    let mut t = t.clone();
    match &mut t {
        Terminator::Call { arguments, .. } => {
            for a in arguments {
                *a = operand(*a, facts, f);
            }
        }
        Terminator::Return { value: Some(value) } => *value = operand(*value, facts, f),
        _ => {}
    }
    t
}
fn body(body: &[I], mut facts: Facts, allowed: &HashSet<Address>) -> Facts {
    for i in body {
        match i {
            I::Branch {
                condition,
                then_body,
                else_body,
            } => {
                kill(&mut facts, V::Cell(*condition));
                let left = self::body(then_body, facts.clone(), allowed);
                let right = self::body(else_body, facts, allowed);
                facts = intersect(left, &right);
                kill(&mut facts, V::Cell(*condition));
            }
            I::Loop {
                body: loop_body, ..
            } => {
                let incoming = facts;
                let mut header = incoming.clone();
                loop {
                    let next = intersect(
                        incoming.clone(),
                        &self::body(loop_body, header.clone(), allowed),
                    );
                    if next == header {
                        break;
                    }
                    header = next;
                }
                facts = header;
            }
            _ => {
                // Generate facts from original identities, independently of how
                // resolving a chain rewrites a later call or return operand.
                if matches!(i, I::Copy { src, dst } if src == dst)
                    || matches!(i, I::AggregateCopy { src, dst, .. } if src == dst)
                {
                    continue;
                }
                effects::instruction(i, |e| effect(&mut facts, e));
                pairs(i, |dst, src| {
                    if allowed.contains(&dst) && dst != src {
                        facts.insert(dst, src);
                    }
                });
            }
        }
    }
    facts
}

pub(crate) fn optimize(
    f: FunctionDescriptor,
    nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    let mut allowed = HashSet::new();
    for c in &nodes {
        destinations(c.body(), &mut allowed);
    }
    if allowed.is_empty() {
        return (f, nodes);
    }
    let mut ordered: Vec<_> = allowed.into_iter().collect();
    ordered.sort_by_key(|a| match *a {
        Address::Frame(s) => (0, s.index(), 0),
        Address::ArrayElement {
            array: AggregateRegion::Frame(id),
            index,
        } => (1, id.index(), index),
        _ => unreachable!(),
    });
    let allowed: HashSet<_> = ordered.into_iter().take(MAX_DESTINATIONS).collect();
    let entries: HashMap<_, _> = nodes.iter().enumerate().map(|(i, c)| (c.id(), i)).collect();
    let start = entries[&f.entry()];
    let mut predecessors = vec![Vec::new(); nodes.len()];
    for (i, c) in nodes.iter().enumerate() {
        for (target, _) in c.terminator().edges() {
            predecessors[entries[&target]].push(i);
        }
    }
    let mut incoming = vec![None; nodes.len()];
    let mut outgoing: Vec<Option<Facts>> = vec![None; nodes.len()];
    let mut queue = VecDeque::from([start]);
    let mut queued = vec![false; nodes.len()];
    queued[start] = true;
    while let Some(i) = queue.pop_front() {
        queued[i] = false;
        let input = if i == start {
            Some(Facts::new())
        } else {
            predecessors[i]
                .iter()
                .filter_map(|p| outgoing[*p].as_ref())
                .cloned()
                .reduce(|a, b| intersect(a, &b))
        };
        let Some(input) = input else {
            continue;
        };
        incoming[i] = Some(input.clone());
        let mut output = body(nodes[i].body(), input, &allowed);
        effects::terminator(nodes[i].terminator(), |e| effect(&mut output, e));
        if outgoing[i].as_ref() != Some(&output) {
            outgoing[i] = Some(output);
            for (target, _) in nodes[i].terminator().edges() {
                let j = entries[&target];
                if !queued[j] {
                    queued[j] = true;
                    queue.push_back(j);
                }
            }
        }
    }
    let mut changed = false;
    let rewritten: Vec<_> = nodes
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let input = incoming[i].clone().unwrap_or_default();
            let facts = body(c.body(), input, &allowed);
            let terminal = terminal(c.terminator(), &facts, &f);
            changed |= terminal != *c.terminator();
            Continuation::new(c.id(), c.function(), c.body().to_vec(), terminal)
                .with_source_spans(c.body_sources().to_vec(), c.terminator_source())
        })
        .collect();
    if changed {
        crate::cir::virtual_cleanup::clean_function(&f, &rewritten)
    } else {
        (f, nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AbiCodegenOptions, ContinuationId, ContinuationProgram, FrameAggregateDescriptor,
        FrameAggregateId, FrameSlot, FunctionId, GlobalDescriptor, GlobalId, LogicalOffset,
        ValueType, run_continuations_with_io,
    };

    fn id(n: u16) -> ContinuationId {
        ContinuationId::new(n).unwrap()
    }
    fn slot(n: usize) -> Address {
        Address::Frame(FrameSlot::new(n))
    }
    fn run(p: &ContinuationProgram, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        run_continuations_with_io(p, &mut &input[..], &mut output, Default::default(), |_| {})
            .unwrap();
        output
    }
    fn transformed(p: &ContinuationProgram) -> ContinuationProgram {
        let mut functions = Vec::new();
        let mut nodes = Vec::new();
        for f in p.functions() {
            let (f, cs) = optimize(
                f.clone(),
                p.continuations()
                    .iter()
                    .filter(|c| c.function() == f.id())
                    .cloned()
                    .collect(),
            );
            functions.push(f);
            nodes.extend(cs);
        }
        ContinuationProgram::new_with_globals(p.main(), p.globals().to_vec(), functions, nodes)
            .unwrap()
    }
    fn check_bf(
        original: &ContinuationProgram,
        rewritten: &ContinuationProgram,
        inputs: &[Vec<u8>],
    ) {
        for options in [
            AbiCodegenOptions::default(),
            AbiCodegenOptions {
                nibble_transfer: true,
                inplace_compare: true,
                anchor_bank: true,
                ..Default::default()
            },
            AbiCodegenOptions {
                region_emission: false,
                ..Default::default()
            },
            AbiCodegenOptions {
                static_frames: true,
                ..Default::default()
            },
        ] {
            let bf = crate::lower_continuations_with_codegen_options(rewritten, options).unwrap();
            let mut compressed = Vec::new();
            crate::optimize_bf(&bf)
                .write_compressed_source(&mut compressed)
                .unwrap();
            for input in inputs {
                let expected = run(original, input);
                assert_eq!(run(rewritten, input), expected);
                for disabled in [false, true] {
                    let result = bf_interpreter::run_with_options(
                        &compressed,
                        input,
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
                        "options={options:?}, rle_only={disabled}, input={input:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn copies_survive_calls_but_snapshots_and_joins_do_not_alias_writes() {
        let main = FunctionId::new(0);
        let helper = FunctionId::new(1);
        let f = FunctionDescriptor::new(main, vec![], 6, ValueType::Void, id(1));
        let g = FunctionDescriptor::new(helper, vec![FrameSlot::new(0)], 1, ValueType::Cell, id(7));
        let original = ContinuationProgram::new(
            main,
            vec![f, g],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![
                        I::Input { dst: slot(0) },
                        I::Copy {
                            src: slot(0),
                            dst: slot(1),
                        },
                    ],
                    Terminator::Goto { target: id(2) },
                ),
                Continuation::new(
                    id(2),
                    main,
                    vec![],
                    Terminator::Call {
                        callee: helper,
                        arguments: vec![V::Cell(slot(1))],
                        return_to: id(3),
                    },
                ),
                Continuation::new(
                    id(3),
                    main,
                    vec![
                        I::Output { src: slot(1) },
                        I::Copy {
                            src: Address::AbiValue,
                            dst: slot(2),
                        },
                        I::Output { src: slot(2) },
                        I::AddConst {
                            dst: slot(0),
                            value: 1,
                        },
                        I::Output { src: slot(1) },
                        I::Input { dst: slot(3) },
                        I::Copy {
                            src: slot(1),
                            dst: slot(4),
                        },
                    ],
                    Terminator::Branch {
                        condition: slot(3),
                        then_target: id(4),
                        else_target: id(5),
                    },
                ),
                Continuation::new(
                    id(4),
                    main,
                    vec![I::AddConst {
                        dst: slot(1),
                        value: 1,
                    }],
                    Terminator::Goto { target: id(6) },
                ),
                Continuation::new(
                    id(5),
                    main,
                    vec![I::Set {
                        dst: slot(0),
                        value: 5,
                    }],
                    Terminator::Goto { target: id(6) },
                ),
                Continuation::new(
                    id(6),
                    main,
                    vec![I::Output { src: slot(4) }, I::Output { src: slot(1) }],
                    Terminator::Halt,
                ),
                Continuation::new(
                    id(7),
                    helper,
                    vec![],
                    Terminator::Return {
                        value: Some(V::Cell(slot(0))),
                    },
                ),
            ],
        )
        .unwrap();
        let rewritten = transformed(&original);
        assert!(
            matches!(rewritten.continuation(id(2)).unwrap().terminator(),Terminator::Call{arguments,..} if arguments==&vec![V::Cell(slot(0))])
        );
        let inputs: Vec<_> = (0..=255u8).map(|v| vec![v, v & 1]).collect();
        check_bf(&original, &rewritten, &inputs);
    }

    #[test]
    fn portals_preserve_unwritten_copies_and_kill_overwritten_sources() {
        let main = FunctionId::new(0);
        let helper = FunctionId::new(1);
        let global = AggregateRegion::Global(GlobalId::new(0));
        for overwrite in [false, true] {
            let destination = if overwrite { slot(0) } else { slot(4) };
            let original = ContinuationProgram::new_with_globals(
                main,
                vec![GlobalDescriptor::new(
                    GlobalId::new(0),
                    ValueType::Array(256),
                )],
                vec![
                    FunctionDescriptor::new(main, vec![], 5, ValueType::Void, id(1)),
                    FunctionDescriptor::new(
                        helper,
                        vec![FrameSlot::new(0)],
                        1,
                        ValueType::Cell,
                        id(5),
                    ),
                ],
                vec![
                    Continuation::new(
                        id(1),
                        main,
                        vec![
                            I::Input { dst: slot(0) },
                            I::Set {
                                dst: slot(3),
                                value: 0,
                            },
                            I::Set {
                                dst: slot(4),
                                value: 0,
                            },
                            I::Copy {
                                src: slot(0),
                                dst: slot(1),
                            },
                            I::Copy {
                                src: slot(0),
                                dst: slot(2),
                            },
                        ],
                        Terminator::AggregateStore {
                            destination: global,
                            offset: LogicalOffset::new(slot(1), slot(3)),
                            source: V::Cell(slot(2)),
                            cells: 1,
                            return_to: id(2),
                        },
                    ),
                    Continuation::new(
                        id(2),
                        main,
                        vec![],
                        Terminator::AggregateLoad {
                            source: global,
                            offset: LogicalOffset::new(
                                if overwrite { slot(4) } else { slot(1) },
                                slot(3),
                            ),
                            destination: V::Cell(destination),
                            cells: 1,
                            return_to: id(3),
                        },
                    ),
                    Continuation::new(
                        id(3),
                        main,
                        vec![I::Output { src: destination }],
                        Terminator::Call {
                            callee: helper,
                            arguments: vec![V::Cell(slot(2))],
                            return_to: id(4),
                        },
                    ),
                    Continuation::new(
                        id(4),
                        main,
                        vec![I::Output {
                            src: Address::AbiValue,
                        }],
                        Terminator::Halt,
                    ),
                    Continuation::new(
                        id(5),
                        helper,
                        vec![],
                        Terminator::Return {
                            value: Some(V::Cell(slot(0))),
                        },
                    ),
                ],
            )
            .unwrap();
            let rewritten = transformed(&original);
            let t = rewritten.continuation(id(1)).unwrap().terminator();
            assert!(
                matches!(t,Terminator::AggregateStore{offset,source:V::Cell(a),..} if offset.low!=*a)
            );
            let t = rewritten.continuation(id(3)).unwrap().terminator();
            let expected = V::Cell(if overwrite { slot(2) } else { slot(0) });
            assert!(matches!(t,Terminator::Call{arguments,..} if arguments == &vec![expected]));
            let inputs: Vec<_> = (0..=255u8).map(|v| vec![v]).collect();
            check_bf(&original, &rewritten, &inputs);
        }
    }

    #[test]
    fn aggregate_views_and_loop_facts_require_every_field_and_path() {
        let main = FunctionId::new(0);
        let producer = FunctionId::new(1);
        let region = |n| AggregateRegion::Frame(FrameAggregateId::new(n));
        let field = |n, index| element(region(n), index);
        for changed in [false, true] {
            let caller = FunctionDescriptor::new_aggregates(
                main,
                vec![],
                0,
                vec![],
                2,
                ValueType::Void,
                id(1),
            );
            let f = FunctionDescriptor::new_aggregates(
                producer,
                vec![],
                2,
                (0..2)
                    .map(|n| FrameAggregateDescriptor::new(FrameAggregateId::new(n), 3))
                    .collect(),
                0,
                ValueType::Aggregate { cells: 2 },
                id(3),
            );
            let mut body = vec![
                I::Input { dst: field(0, 0) },
                I::Input { dst: field(0, 1) },
                I::Set {
                    dst: field(0, 2),
                    value: 23,
                },
                I::AggregateCopy {
                    src: region(0),
                    dst: region(1),
                    cells: 3,
                },
                I::Set {
                    dst: slot(0),
                    value: 3,
                },
                I::Copy {
                    src: field(1, 0),
                    dst: slot(1),
                },
            ];
            if changed {
                body.push(I::AddConst {
                    dst: field(0, 1),
                    value: 1,
                });
            }
            body.push(I::Loop {
                condition: slot(0),
                body: vec![
                    I::AddConst {
                        dst: field(0, 0),
                        value: 7,
                    },
                    I::Output { src: slot(1) },
                    I::AddConst {
                        dst: slot(0),
                        value: 255,
                    },
                ],
            });
            let original = ContinuationProgram::new(
                main,
                vec![caller, f],
                vec![
                    Continuation::new(
                        id(1),
                        main,
                        vec![],
                        Terminator::Call {
                            callee: producer,
                            arguments: vec![],
                            return_to: id(2),
                        },
                    ),
                    Continuation::new(
                        id(2),
                        main,
                        vec![
                            I::Output {
                                src: element(AggregateRegion::Outbox, 0),
                            },
                            I::Output {
                                src: element(AggregateRegion::Outbox, 1),
                            },
                        ],
                        Terminator::Halt,
                    ),
                    Continuation::new(
                        id(3),
                        producer,
                        body,
                        Terminator::Return {
                            value: Some(V::Aggregate {
                                region: region(1),
                                offset: 1,
                                cells: 2,
                            }),
                        },
                    ),
                ],
            )
            .unwrap();
            let rewritten = transformed(&original);
            assert!(
                matches!(rewritten.continuation(id(3)).unwrap().terminator(),Terminator::Return{value:Some(V::Aggregate{region:r,offset:1,cells:2})} if *r==region(usize::from(changed)))
            );
            let inputs: Vec<_> = (0..=255u8).map(|v| vec![v, v.wrapping_mul(37)]).collect();
            check_bf(&original, &rewritten, &inputs);
        }
    }
}
