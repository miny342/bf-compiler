//! Eliminate dead local writes and unused virtual storage before allocation.
//! Liveness uses intervals for aggregate payloads, so a dynamic read of a large
//! region does not allocate one set entry per cell. Globals and I/O are retained.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::continuation_effects::{self, Effect};
use crate::continuation_operands::{map_body, map_terminator};
use crate::{
    Address, AggregateRegion, Continuation, ContinuationId, ContinuationIrError,
    ContinuationProgram, FrameAggregateDescriptor, FrameAggregateId, FrameInstruction as I,
    FrameSlot, FunctionDescriptor, ParameterLocation, SourceSpan, ValueOperand as V,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Intervals(Vec<(usize, usize)>);
impl Intervals {
    fn insert(&mut self, mut low: usize, mut high: usize) {
        if low == high {
            return;
        }
        let first = self.0.partition_point(|&(_, end)| end < low);
        let mut last = first;
        while last < self.0.len() && self.0[last].0 <= high {
            low = low.min(self.0[last].0);
            high = high.max(self.0[last].1);
            last += 1;
        }
        self.0.splice(first..last, [(low, high)]);
    }
    fn remove(&mut self, low: usize, high: usize) {
        if low == high {
            return;
        }
        let mut result = Vec::new();
        for &(start, end) in &self.0 {
            if end <= low || start >= high {
                result.push((start, end));
                continue;
            }
            if start < low {
                result.push((start, low));
            }
            if end > high {
                result.push((high, end));
            }
        }
        self.0 = result;
    }
    fn intersects(&self, low: usize, high: usize) -> bool {
        low < high && self.0.iter().any(|&(start, end)| start < high && end > low)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Live {
    scalars: BTreeSet<FrameSlot>,
    aggregates: HashMap<FrameAggregateId, Intervals>,
}

enum Local {
    Scalar(FrameSlot),
    Range(FrameAggregateId, usize, usize),
    External,
}
fn local(operand: V, f: &FunctionDescriptor) -> Local {
    match operand {
        V::Cell(Address::Frame(slot)) => Local::Scalar(slot),
        V::Cell(Address::ArrayElement {
            array: AggregateRegion::Frame(a),
            index,
        }) => Local::Range(a, index, index + 1),
        V::Array(AggregateRegion::Frame(a)) => {
            Local::Range(a, 0, f.frame_aggregate(a).unwrap().cells())
        }
        V::Aggregate {
            region: AggregateRegion::Frame(a),
            offset,
            cells,
        } => Local::Range(a, offset, offset + cells),
        _ => Local::External,
    }
}
impl Live {
    fn read(&mut self, operand: V, f: &FunctionDescriptor) {
        match local(operand, f) {
            Local::Scalar(slot) => {
                self.scalars.insert(slot);
            }
            Local::Range(a, start, end) if start < end => {
                self.aggregates.entry(a).or_default().insert(start, end)
            }
            _ => {}
        }
    }
    fn write(&mut self, operand: V, f: &FunctionDescriptor) {
        match local(operand, f) {
            Local::Scalar(slot) => {
                self.scalars.remove(&slot);
            }
            Local::Range(a, start, end) => {
                if let Some(intervals) = self.aggregates.get_mut(&a) {
                    intervals.remove(start, end);
                    if intervals.0.is_empty() {
                        self.aggregates.remove(&a);
                    }
                }
            }
            Local::External => {}
        }
    }
    fn observed(&self, operand: V, f: &FunctionDescriptor) -> bool {
        match local(operand, f) {
            Local::Scalar(slot) => self.scalars.contains(&slot),
            Local::Range(a, start, end) => self
                .aggregates
                .get(&a)
                .is_some_and(|v| v.intersects(start, end)),
            Local::External => true,
        }
    }
    fn union(&mut self, other: &Self) {
        self.scalars.extend(other.scalars.iter().copied());
        for (&a, intervals) in &other.aggregates {
            for &(start, end) in &intervals.0 {
                self.aggregates.entry(a).or_default().insert(start, end);
            }
        }
    }
    fn effects(&mut self, effects: &[Effect], f: &FunctionDescriptor) {
        // Inputs are snapshots; all definitions kill BEFORE any read is added.
        for effect in effects {
            match *effect {
                Effect::Write(operand) => self.write(operand, f),
                Effect::Clobber(address) => self.write(V::Cell(address), f),
                _ => {}
            }
        }
        for effect in effects {
            match *effect {
                Effect::Read(operand) => self.read(operand, f),
                Effect::MayWrite(region) => self.read(V::Array(region), f),
                Effect::Write(_) | Effect::Clobber(_) | Effect::CallResult(_) => {}
            }
        }
    }
}

fn body_liveness(
    body: &[I],
    sources: &[Option<SourceSpan>],
    mut live: Live,
    f: &FunctionDescriptor,
    rewrite: bool,
) -> (Live, Vec<I>, Vec<Option<SourceSpan>>) {
    let mut output = Vec::new();
    let mut output_sources = Vec::new();
    for (index, instruction) in body.iter().enumerate().rev() {
        let kept = match instruction {
            I::Branch {
                condition,
                then_body,
                else_body,
            } => {
                // Structured Branch consumes the guard both before and after
                // its arm. Reads inside the arm observe the initial zero.
                live.write(V::Cell(*condition), f);
                let (mut left, then_body, _) =
                    body_liveness(then_body, &[], live.clone(), f, rewrite);
                let (right, else_body, _) = body_liveness(else_body, &[], live, f, rewrite);
                left.union(&right);
                left.write(V::Cell(*condition), f);
                left.read(V::Cell(*condition), f);
                live = left;
                rewrite.then_some(I::Branch {
                    condition: *condition,
                    then_body,
                    else_body,
                })
            }
            I::Loop { condition, body } => {
                let exit = live;
                let mut header = exit.clone();
                header.read(V::Cell(*condition), f);
                loop {
                    let (mut next, _, _) = body_liveness(body, &[], header.clone(), f, false);
                    next.union(&exit);
                    next.read(V::Cell(*condition), f);
                    if next == header {
                        break;
                    }
                    header = next;
                }
                let (_, body, _) = body_liveness(body, &[], header.clone(), f, rewrite);
                live = header;
                rewrite.then_some(I::Loop {
                    condition: *condition,
                    body,
                })
            }
            _ => {
                let mut effects = Vec::new();
                continuation_effects::instruction(instruction, |e| effects.push(e));
                let needed = continuation_effects::observable(instruction)
                    || effects.iter().any(|effect| match *effect {
                        Effect::Write(operand) => live.observed(operand, f),
                        Effect::Clobber(address) => live.observed(V::Cell(address), f),
                        Effect::MayWrite(_) | Effect::CallResult(_) => true,
                        Effect::Read(_) => false,
                    });
                if needed {
                    live.effects(&effects, f);
                }
                (needed && rewrite).then(|| instruction.clone())
            }
        };
        if let Some(instruction) = kept {
            output.push(instruction);
            output_sources.push(sources.get(index).copied().flatten());
        }
    }
    output.reverse();
    output_sources.reverse();
    (live, output, output_sources)
}

fn terminal_live(
    c: &Continuation,
    entries: &HashMap<ContinuationId, usize>,
    live: &[Live],
    f: &FunctionDescriptor,
) -> Live {
    let mut result = Live::default();
    for (target, _) in c.terminator().edges() {
        result.union(&live[entries[&target]]);
    }
    let mut effects = Vec::new();
    continuation_effects::terminator(c.terminator(), |e| effects.push(e));
    result.effects(&effects, f);
    result
}

fn clean_function(
    f: &FunctionDescriptor,
    nodes: &[Continuation],
) -> (FunctionDescriptor, Vec<Continuation>) {
    let entries: HashMap<_, _> = nodes.iter().enumerate().map(|(i, c)| (c.id(), i)).collect();
    let mut predecessors = vec![Vec::new(); nodes.len()];
    for (i, c) in nodes.iter().enumerate() {
        for (target, _) in c.terminator().edges() {
            predecessors[entries[&target]].push(i);
        }
    }
    let mut live = vec![Live::default(); nodes.len()];
    let mut pending: VecDeque<_> = (0..nodes.len()).rev().collect();
    let mut queued = vec![true; nodes.len()];
    while let Some(index) = pending.pop_front() {
        queued[index] = false;
        let c = &nodes[index];
        let out = terminal_live(c, &entries, &live, f);
        let (next, _, _) = body_liveness(c.body(), &[], out, f, false);
        if live[index] != next {
            live[index] = next;
            for &predecessor in &predecessors[index] {
                if !queued[predecessor] {
                    queued[predecessor] = true;
                    pending.push_back(predecessor);
                }
            }
        }
    }
    let rewritten = nodes
        .iter()
        .map(|c| {
            let out = terminal_live(c, &entries, &live, f);
            let (_, body, sources) = body_liveness(c.body(), c.body_sources(), out, f, true);
            Continuation::new(c.id(), c.function(), body, c.terminator().clone())
                .with_source_spans(sources, c.terminator_source())
        })
        .collect::<Vec<_>>();
    prune_storage(f, rewritten)
}

fn prune_storage(
    f: &FunctionDescriptor,
    mut nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    let mut scalars = BTreeSet::new();
    let mut aggregates = BTreeSet::new();
    for c in &nodes {
        let mut body = c.body().to_vec();
        let mut terminal = c.terminator().clone();
        let record = &mut |a: &mut Address| match a {
            Address::Frame(slot) => {
                scalars.insert(*slot);
            }
            Address::ArrayElement {
                array: AggregateRegion::Frame(a),
                ..
            } => {
                aggregates.insert(*a);
            }
            _ => {}
        };
        map_body(&mut body, record);
        map_terminator(&mut terminal, record);
    }
    for p in f.parameter_locations() {
        match p {
            ParameterLocation::Cell(slot) => {
                scalars.insert(*slot);
            }
            ParameterLocation::Array(a)
            | ParameterLocation::Aggregate(a)
            | ParameterLocation::AggregateElement { aggregate: a, .. } => {
                aggregates.insert(*a);
            }
        }
    }
    let slots: HashMap<_, _> = scalars
        .into_iter()
        .enumerate()
        .map(|(i, old)| (old, FrameSlot::new(i)))
        .collect();
    let regions: HashMap<_, _> = aggregates
        .into_iter()
        .enumerate()
        .map(|(i, old)| (old, FrameAggregateId::new(i)))
        .collect();
    let map = &mut |a: &mut Address| match a {
        Address::Frame(slot) => *slot = slots[slot],
        Address::ArrayElement {
            array: AggregateRegion::Frame(a),
            ..
        } => *a = regions[a],
        _ => {}
    };
    for c in &mut nodes {
        let mut body = c.body().to_vec();
        let mut terminal = c.terminator().clone();
        map_body(&mut body, map);
        map_terminator(&mut terminal, map);
        *c = Continuation::new(c.id(), c.function(), body, terminal)
            .with_source_spans(c.body_sources().to_vec(), c.terminator_source());
    }
    let parameters = f
        .parameter_locations()
        .iter()
        .map(|p| match *p {
            ParameterLocation::Cell(slot) => ParameterLocation::Cell(slots[&slot]),
            ParameterLocation::Array(a) => ParameterLocation::Array(regions[&a]),
            ParameterLocation::Aggregate(a) => ParameterLocation::Aggregate(regions[&a]),
            ParameterLocation::AggregateElement { aggregate, index } => {
                ParameterLocation::AggregateElement {
                    aggregate: regions[&aggregate],
                    index,
                }
            }
        })
        .collect();
    let mut descriptors = f
        .frame_aggregates()
        .iter()
        .filter_map(|a| {
            regions
                .get(&a.id())
                .map(|&id| FrameAggregateDescriptor::new(id, a.cells()))
        })
        .collect::<Vec<_>>();
    descriptors.sort_by_key(|a| a.id());
    let descriptor = FunctionDescriptor::new_aggregates(
        f.id(),
        parameters,
        slots.len(),
        descriptors,
        f.outbox_cells(),
        f.return_type(),
        f.entry(),
    );
    let descriptor = if let Some(name) = f.name() {
        descriptor.with_name(name)
    } else {
        descriptor
    };
    (descriptor, nodes)
}

pub(crate) fn cleanup(
    program: &ContinuationProgram,
) -> Result<ContinuationProgram, ContinuationIrError> {
    let mut functions = Vec::new();
    let mut nodes = Vec::new();
    for f in program.functions() {
        let original = program
            .continuations()
            .iter()
            .filter(|c| c.function() == f.id())
            .cloned()
            .collect::<Vec<_>>();
        let (f, cleaned) = clean_function(f, &original);
        functions.push(f);
        nodes.extend(cleaned);
    }
    ContinuationProgram::new_with_globals(
        program.main(),
        program.globals().to_vec(),
        functions,
        nodes,
    )
    .map(|p| p.with_source_files(program.source_files().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameTransferTarget, FunctionId, Terminator, ValueType};

    fn run(p: &ContinuationProgram, input: &[u8]) -> (Vec<u8>, u64) {
        let mut output = Vec::new();
        let stats = crate::run_continuations_with_io(
            p,
            &mut &input[..],
            &mut output,
            Default::default(),
            |_| {},
        )
        .unwrap();
        (output, stats.input_operations)
    }

    #[test]
    fn interval_ranges_preserve_holes_and_merge_adjacent_payloads() {
        let mut ranges = Intervals::default();
        ranges.insert(0, 65536);
        ranges.remove(12, 18);
        ranges.remove(20, 65536);
        assert_eq!(ranges.0, [(0, 12), (18, 20)]);
        assert!(!ranges.intersects(12, 18));
        ranges.insert(12, 18);
        assert_eq!(ranges.0, [(0, 20)]);
    }

    #[test]
    fn aliasing_destructive_operations_and_structured_cycles_match_original() {
        let main = FunctionId::new(0);
        let entry = ContinuationId::new(1).unwrap();
        let slot = |n| Address::Frame(FrameSlot::new(n));
        let a = AggregateRegion::Frame(FrameAggregateId::new(0));
        let cell = |n| Address::ArrayElement { array: a, index: n };
        let mut seed = 0x6c30928bu32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for _ in 0..96 {
            let mut body = (0..5)
                .map(|n| I::Set {
                    dst: slot(n),
                    value: (next() % 8) as u8,
                })
                .collect::<Vec<_>>();
            body.extend((0..4).map(|n| I::Set {
                dst: cell(n),
                value: (next() % 8) as u8,
            }));
            body.push(I::Input { dst: slot(9) }); // Keep input even though its value is dead.
            let mut iteration = Vec::new();
            for _ in 0..16 {
                let left = slot(next() % 5);
                let right = slot(next() % 5);
                let dst = slot(next() % 5);
                iteration.push(match next() % 6 {
                    0 => I::SubWithBorrow {
                        left,
                        right,
                        difference: dst,
                        borrow: slot(next() % 5),
                        true_value: 1,
                        false_value: 0,
                    },
                    1 => I::Compare {
                        left,
                        right,
                        dst,
                        true_value: 1,
                        false_value: 0,
                    },
                    2 => I::Transfer {
                        src: left,
                        targets: vec![FrameTransferTarget {
                            dst: if dst == left { slot(5) } else { dst },
                            factor: 1,
                        }],
                    },
                    3 => I::Copy {
                        src: left,
                        dst: cell(next() % 4),
                    },
                    4 => I::Copy {
                        src: cell(next() % 4),
                        dst,
                    },
                    _ => I::Branch {
                        condition: left,
                        then_body: vec![I::Set { dst, value: 7 }],
                        else_body: vec![I::Copy { src: right, dst }],
                    },
                });
            }
            iteration.push(I::Output { src: cell(1) });
            iteration.push(I::AddConst {
                dst: slot(8),
                value: 255,
            });
            body.push(I::Set {
                dst: slot(8),
                value: 3,
            });
            body.push(I::Loop {
                condition: slot(8),
                body: iteration,
            });
            body.push(I::Output { src: slot(0) });
            body.push(I::Set {
                dst: slot(30),
                value: 99,
            }); // Removable storage.
            let p = ContinuationProgram::new(
                main,
                vec![FunctionDescriptor::new_aggregates(
                    main,
                    vec![],
                    31,
                    vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 4)],
                    0,
                    ValueType::Void,
                    entry,
                )],
                vec![Continuation::new(entry, main, body, Terminator::Halt)],
            )
            .unwrap();
            let cleaned = cleanup(&p).unwrap();
            assert_eq!(run(&p, &[42]), run(&cleaned, &[42]));
            assert!(cleaned.function(main).unwrap().frame_slots() < 31);
        }
    }

    #[test]
    fn partial_aggregate_updates_across_calls_and_portals_keep_live_cells() {
        let ast = crate::parser::parse(crate::lexer::lex("cell f() { output(70); return input(); } void main() { cell[5] a; a[1]=11; a[3]=33; cell n=2; while(n) { if(input()) { a[2]=22; } else { a[4]=44; } a[input()]=f(); n-=1; } output(a[1]); output(a[2]); output(a[3]); output(a[4]); }").unwrap()).unwrap();
        let hir = crate::semantic::analyze(&ast).unwrap();
        let p = crate::continuation_lowering::lower_hir_unallocated(&hir).unwrap();
        let cleaned = cleanup(&p).unwrap();
        for input in [&[0, 90, 2, 1, 91, 4][..], &[1, 50, 0, 0, 51, 0][..]] {
            assert_eq!(run(&p, input), run(&cleaned, input));
        }
    }
}
