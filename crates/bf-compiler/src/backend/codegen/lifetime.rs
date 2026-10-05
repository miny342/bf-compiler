//! Decide when local copy sources may be consumed instead of restored.
//!
//! Analyze the allocated CIR, including structured control and call resume
//! edges. Aggregate liveness uses intervals, not one set entry per payload cell.
//! Globals remain observable even when no subsequent caller instruction reads
//! them. Fixed-storage instructions and argument bridges conservatively keep
//! their copy paths.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::marker::PhantomData;

use super::*;
use crate::cir::analysis::intervals::Intervals;
use crate::cir::effects::{self, Effect};

#[derive(Clone, Default, PartialEq, Eq)]
struct Live {
    scalars: BTreeSet<FrameSlot>,
    abi_value: bool,
    aggregates: HashMap<AggregateRegion, Intervals>,
}

fn local(address: Address) -> bool {
    matches!(
        address,
        Address::Frame(_)
            | Address::AbiValue
            | Address::ArrayElement {
                array: AggregateRegion::Frame(_) | AggregateRegion::Outbox,
                ..
            }
    )
}

impl Live {
    fn insert(&mut self, value: ValueOperand, f: &FunctionDescriptor) {
        match value {
            ValueOperand::Cell(Address::Frame(slot)) => {
                self.scalars.insert(slot);
            }
            ValueOperand::Cell(Address::AbiValue) => self.abi_value = true,
            ValueOperand::Cell(Address::ArrayElement { array, index }) => {
                self.range(array, index, index + 1);
            }
            ValueOperand::Array(region) => {
                let cells = match region {
                    AggregateRegion::Frame(id) => f.frame_aggregate(id).unwrap().cells(),
                    AggregateRegion::Outbox => f.outbox_cells(),
                    AggregateRegion::Global(_) => return,
                };
                self.range(region, 0, cells);
            }
            ValueOperand::Aggregate {
                region,
                offset,
                cells,
            } => {
                self.range(region, offset, offset + cells);
            }
            ValueOperand::Cell(Address::Global(_)) => {}
        }
    }

    fn range(&mut self, region: AggregateRegion, start: usize, end: usize) {
        if start < end && !matches!(region, AggregateRegion::Global(_)) {
            self.aggregates
                .entry(region)
                .or_default()
                .insert(start, end);
        }
    }

    fn contains(&self, address: Address) -> bool {
        match address {
            Address::Frame(slot) => self.scalars.contains(&slot),
            Address::AbiValue => self.abi_value,
            Address::ArrayElement { array, index } => self
                .aggregates
                .get(&array)
                .is_some_and(|range| range.intersects(index, index + 1)),
            Address::Global(_) => true,
        }
    }

    fn union(&mut self, other: &Self) {
        self.scalars.extend(other.scalars.iter().copied());
        self.abi_value |= other.abi_value;
        for (&region, ranges) in &other.aggregates {
            for &(start, end) in &ranges.0 {
                self.range(region, start, end);
            }
        }
    }

    fn subtract(&mut self, definitions: &Self) {
        self.scalars.retain(|s| !definitions.scalars.contains(s));
        self.abi_value &= !definitions.abi_value;
        for (region, ranges) in &definitions.aggregates {
            if let Some(live) = self.aggregates.get_mut(region) {
                for &(start, end) in &ranges.0 {
                    live.remove(start, end);
                }
                if live.0.is_empty() {
                    self.aggregates.remove(region);
                }
            }
        }
    }
}

#[derive(Default)]
struct Node {
    uses: Live,
    defs: Live,
    next: Vec<usize>,
    instruction: Option<*const FrameInstruction>,
    call: Option<ContinuationId>,
}

impl Node {
    fn effect(&mut self, effect: Effect, f: &FunctionDescriptor, p: &ContinuationProgram) {
        match effect {
            Effect::Read(value) => self.uses.insert(value, f),
            Effect::Write(value) => self.defs.insert(value, f),
            Effect::Clobber(address) => self.defs.insert(ValueOperand::Cell(address), f),
            // The runtime-selected leaf is unknown; other leaves survive.
            Effect::MayWrite(region) => self.uses.insert(ValueOperand::Array(region), f),
            Effect::CallResult(callee) => {
                self.defs.abi_value = true;
                let cells = match p.function(callee).unwrap().return_type() {
                    ValueType::Array(n) | ValueType::Aggregate { cells: n } => n,
                    _ => 0,
                };
                // Scalar/void calls leave the caller's outbox untouched.
                self.defs.range(AggregateRegion::Outbox, 0, cells);
            }
        }
    }
}

fn build_body(
    body: &[FrameInstruction],
    mut next: usize,
    nodes: &mut Vec<Node>,
    f: &FunctionDescriptor,
    p: &ContinuationProgram,
) -> usize {
    for instruction in body.iter().rev() {
        let mut node = Node {
            next: vec![next],
            ..Default::default()
        };
        match instruction {
            FrameInstruction::Loop { condition, body } => {
                node.uses.insert(ValueOperand::Cell(*condition), f);
                let header = nodes.len();
                nodes.push(node);
                let first = build_body(body, header, nodes, f, p);
                nodes[header].next.push(first);
                next = header;
                continue;
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                // Structured branches clear the guard before and after an arm.
                let mut exit = Node {
                    next: vec![next],
                    ..Default::default()
                };
                exit.defs.insert(ValueOperand::Cell(*condition), f);
                let exit_id = nodes.len();
                nodes.push(exit);
                let left = build_body(then_body, exit_id, nodes, f, p);
                let right = build_body(else_body, exit_id, nodes, f, p);
                node.next = vec![left, right];
                node.uses.insert(ValueOperand::Cell(*condition), f);
                node.defs.insert(ValueOperand::Cell(*condition), f);
            }
            _ => {
                effects::instruction(instruction, |e| node.effect(e, f, p));
                if matches!(
                    instruction,
                    FrameInstruction::Copy { .. } | FrameInstruction::AggregateCopy { .. }
                ) {
                    node.instruction = Some(instruction);
                }
            }
        }
        next = nodes.len();
        nodes.push(node);
    }
    next
}

fn live_out(node: &Node, live: &[Live]) -> Live {
    let mut out = Live::default();
    for &successor in &node.next {
        out.union(&live[successor]);
    }
    out
}

/// Instruction identities refer to the immutable program borrowed by the
/// emitter. Pointers are only compared/hashed, never dereferenced. A copied or
/// synthesized instruction has no entry and conservatively preserves sources.
pub(super) struct Plan<'a> {
    out: HashMap<*const FrameInstruction, Live>,
    calls: HashMap<ContinuationId, Live>,
    program: PhantomData<&'a ContinuationProgram>,
}

impl<'a> Plan<'a> {
    pub(super) fn new(p: &'a ContinuationProgram) -> Self {
        let mut plan = Self {
            out: HashMap::new(),
            calls: HashMap::new(),
            program: PhantomData,
        };
        let mut functions = HashMap::<FunctionId, Vec<&Continuation>>::new();
        for c in p.continuations() {
            functions.entry(c.function()).or_default().push(c);
        }
        for f in p.functions() {
            let cs = &functions[&f.id()];
            let entries: HashMap<_, _> = cs.iter().enumerate().map(|(i, c)| (c.id(), i)).collect();
            let mut nodes = (0..cs.len()).map(|_| Node::default()).collect::<Vec<_>>();
            for c in cs {
                let mut terminal = Node::default();
                effects::terminator(c.terminator(), |e| terminal.effect(e, f, p));
                terminal
                    .next
                    .extend(c.terminator().edges().map(|(id, _)| entries[&id]));
                if matches!(c.terminator(), Terminator::Call { .. }) {
                    terminal.call = Some(c.id());
                }
                let last = nodes.len();
                nodes.push(terminal);
                let first = build_body(c.body(), last, &mut nodes, f, p);
                nodes[entries[&c.id()]].next.push(first);
            }
            let mut predecessors = vec![vec![]; nodes.len()];
            for (i, node) in nodes.iter().enumerate() {
                for &s in &node.next {
                    predecessors[s].push(i);
                }
            }
            let mut live = vec![Live::default(); nodes.len()];
            let mut queue = (0..nodes.len()).rev().collect::<VecDeque<_>>();
            let mut queued = vec![true; nodes.len()];
            while let Some(i) = queue.pop_front() {
                queued[i] = false;
                let mut input = live_out(&nodes[i], &live);
                // Reads observe input snapshots, including aliased results.
                input.subtract(&nodes[i].defs);
                input.union(&nodes[i].uses);
                if input != live[i] {
                    live[i] = input;
                    for &before in &predecessors[i] {
                        if !queued[before] {
                            queued[before] = true;
                            queue.push_back(before);
                        }
                    }
                }
            }
            for node in &nodes {
                if let Some(instruction) = node.instruction {
                    plan.out.insert(instruction, live_out(node, &live));
                }
                if let Some(call) = node.call {
                    plan.calls.insert(call, live_out(node, &live));
                }
            }
        }
        plan
    }

    pub(super) fn dead(&self, instruction: &FrameInstruction, source: Address) -> bool {
        local(source)
            && self
                .out
                .get(&(instruction as *const _))
                .is_some_and(|live| !live.contains(source))
    }

    pub(super) fn call_dead(&self, call: ContinuationId, source: Address) -> bool {
        // Inspect resume liveness before removing ABI result definitions. This
        // conservatively preserves arguments whose cells receive new results.
        local(source)
            && self
                .calls
                .get(&call)
                .is_some_and(|live| !live.contains(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameAggregateDescriptor, FrameAggregateId};

    #[test]
    fn large_payload_liveness_tracks_partial_overwrites_without_expanding_cells() {
        let f = FunctionDescriptor::new_aggregates(
            FunctionId::new(0),
            vec![],
            0,
            vec![FrameAggregateDescriptor::new(
                FrameAggregateId::new(0),
                65536,
            )],
            65536,
            ValueType::Void,
            ContinuationId::new(1).unwrap(),
        );
        let region = AggregateRegion::Frame(FrameAggregateId::new(0));
        let mut live = Live::default();
        live.insert(ValueOperand::Array(region), &f);
        live.insert(ValueOperand::Array(AggregateRegion::Outbox), &f);
        assert_eq!(live.aggregates[&region].0, [(0, 65536)]);
        let mut defs = Live::default();
        defs.insert(
            ValueOperand::Aggregate {
                region,
                offset: 12,
                cells: 6,
            },
            &f,
        );
        defs.insert(ValueOperand::aggregate(AggregateRegion::Outbox, 2), &f);
        live.subtract(&defs);
        assert_eq!(live.aggregates[&region].0, [(0, 12), (18, 65536)]);
        assert_eq!(live.aggregates[&AggregateRegion::Outbox].0, [(2, 65536)]);
        assert!(live.contains(Address::ArrayElement {
            array: region,
            index: 65535
        }));
        assert!(!live.contains(Address::ArrayElement {
            array: region,
            index: 15
        }));
    }
}
