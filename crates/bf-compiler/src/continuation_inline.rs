//! Clone and splice virtual CIR graphs before frame allocation.
//!
//! Physical ABI result cells are materialized only after graph inlining. Every
//! call carries its logical result destination, including calls cloned from an
//! inlined activation. This prevents nested calls from sharing an outer inbox.

use std::collections::{HashMap, HashSet};

use crate::continuation_operands::{map_body, map_operand, map_region, map_terminator};
use crate::{
    Address, AggregateRegion, Continuation, ContinuationId, ContinuationProgram,
    FrameAggregateDescriptor, FrameAggregateId, FrameInstruction as I, FrameSlot,
    FunctionDescriptor, FunctionId, ParameterLocation, Terminator, ValueOperand, ValueType,
};

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct InlineStats {
    pub calls_inlined: usize,
    pub recursive_calls_preserved: usize,
    pub id_limit_calls_preserved: usize,
    pub blocks_cloned: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ResultStorage {
    scalar: Option<Address>,
    aggregate: Option<AggregateRegion>,
}

struct Ids {
    used: HashSet<ContinuationId>,
    next: u32,
}

impl Ids {
    fn new(program: &ContinuationProgram) -> Self {
        Self {
            used: program
                .continuations()
                .iter()
                .map(Continuation::id)
                .collect(),
            next: 1,
        }
    }

    fn fresh(&mut self) -> Result<ContinuationId, String> {
        while self.next <= u32::from(u16::MAX) {
            let id = ContinuationId::new(self.next as u16).unwrap();
            self.next += 1;
            if self.used.insert(id) {
                return Ok(id);
            }
        }
        Err("CIR inline exhausted continuation IDs".into())
    }
}

struct Graph {
    functions: Vec<FunctionDescriptor>,
    nodes: Vec<Continuation>,
    results: HashMap<ContinuationId, ResultStorage>,
    owners: HashMap<FunctionId, ResultStorage>,
    ids: Ids,
}

fn descriptor(
    original: &FunctionDescriptor,
    slots: usize,
    aggregates: Vec<FrameAggregateDescriptor>,
    outbox: usize,
) -> FunctionDescriptor {
    let rebuilt = FunctionDescriptor::new_aggregates(
        original.id(),
        original.parameter_locations().to_vec(),
        slots,
        aggregates,
        outbox,
        original.return_type(),
        original.entry(),
    );
    match original.name() {
        Some(name) => rebuilt.with_name(name),
        None => rebuilt,
    }
}

fn result_cells(value_type: ValueType) -> usize {
    match value_type {
        ValueType::Array(cells) | ValueType::Aggregate { cells } => cells,
        ValueType::Cell | ValueType::Void => 0,
    }
}

fn remap_result(result: ResultStorage, address: &mut Address) {
    match address {
        Address::AbiValue => *address = result.scalar.expect("observed scalar result"),
        Address::ArrayElement { array: region, .. } if *region == AggregateRegion::Outbox => {
            *region = result.aggregate.expect("observed aggregate result");
        }
        _ => {}
    }
}

impl Graph {
    fn normalize(program: &ContinuationProgram) -> Result<Self, String> {
        // Calls need no logical destination when the surrounding activation
        // never observes that part of its result. Scan explicit operands before
        // adding storage; an implicit ABI write alone is not an observation.
        let mut used = HashMap::<FunctionId, (bool, bool)>::new();
        for c in program.continuations() {
            let usage = used.entry(c.function()).or_default();
            let mut body = c.body().to_vec();
            let mut terminal = c.terminator().clone();
            let visit = &mut |address: &mut Address| match address {
                Address::AbiValue => usage.0 = true,
                Address::ArrayElement {
                    array: AggregateRegion::Outbox,
                    ..
                } => usage.1 = true,
                _ => {}
            };
            map_body(&mut body, visit);
            map_terminator(&mut terminal, visit);
        }
        let mut functions = Vec::new();
        let mut storage = HashMap::new();
        for f in program.functions() {
            let (scalar_used, aggregate_used) = used.get(&f.id()).copied().unwrap_or_default();
            let mut aggregates = f.frame_aggregates().to_vec();
            let aggregate = aggregate_used.then(|| {
                let id = FrameAggregateId::new(aggregates.len());
                aggregates.push(FrameAggregateDescriptor::new(id, f.outbox_cells()));
                AggregateRegion::Frame(id)
            });
            storage.insert(
                f.id(),
                ResultStorage {
                    scalar: scalar_used.then_some(Address::Frame(FrameSlot::new(f.frame_slots()))),
                    aggregate,
                },
            );
            functions.push(descriptor(
                f,
                f.frame_slots()
                    .checked_add(usize::from(scalar_used))
                    .ok_or("CIR inline frame overflow")?,
                aggregates,
                f.outbox_cells(),
            ));
        }
        let mut nodes = Vec::new();
        let mut results = HashMap::new();
        for c in program.continuations() {
            let result = storage[&c.function()];
            let mut body = c.body().to_vec();
            let mut terminal = c.terminator().clone();
            let map = &mut |a: &mut Address| remap_result(result, a);
            map_body(&mut body, map);
            map_terminator(&mut terminal, map);
            if matches!(terminal, Terminator::Call { .. }) {
                results.insert(c.id(), result);
            }
            nodes.push(
                Continuation::new(c.id(), c.function(), body, terminal)
                    .with_source_spans(c.body_sources().to_vec(), c.terminator_source()),
            );
        }
        Ok(Self {
            functions,
            nodes,
            results,
            owners: storage,
            ids: Ids::new(program),
        })
    }

    fn splice(&mut self, site: ContinuationId) -> Result<usize, String> {
        let call_index = self.nodes.iter().position(|c| c.id() == site).unwrap();
        let call = self.nodes[call_index].clone();
        let Terminator::Call {
            callee,
            arguments,
            return_to,
        } = call.terminator()
        else {
            unreachable!("inline site must be a call");
        };
        let callee = self
            .functions
            .iter()
            .find(|f| f.id() == *callee)
            .unwrap()
            .clone();
        let caller_index = self
            .functions
            .iter()
            .position(|f| f.id() == call.function())
            .unwrap();
        let caller = &self.functions[caller_index];
        let scalar_base = caller.frame_slots();
        let aggregate_base = caller.frame_aggregates().len();
        let slots = scalar_base
            .checked_add(callee.frame_slots())
            .ok_or("CIR inline frame overflow")?;
        let mut aggregates = caller.frame_aggregates().to_vec();
        for a in callee.frame_aggregates() {
            aggregates.push(FrameAggregateDescriptor::new(
                FrameAggregateId::new(aggregate_base + a.id().index()),
                a.cells(),
            ));
        }
        self.functions[caller_index] = descriptor(caller, slots, aggregates, caller.outbox_cells());
        let map = &mut |address: &mut Address| match address {
            Address::Frame(slot) => *slot = FrameSlot::new(scalar_base + slot.index()),
            Address::ArrayElement {
                array: AggregateRegion::Frame(a),
                ..
            } => {
                *a = FrameAggregateId::new(aggregate_base + a.index());
            }
            _ => {}
        };
        let cloned = self
            .nodes
            .iter()
            .filter(|c| c.function() == callee.id())
            .cloned()
            .collect::<Vec<_>>();
        let mut ids = HashMap::new();
        for c in &cloned {
            ids.insert(c.id(), self.ids.fresh()?);
        }
        let destination = self.results.remove(&site).unwrap();
        for c in &cloned {
            let mut body = c.body().to_vec();
            let mut sources = c.body_sources().to_vec();
            map_body(&mut body, map);
            let mut terminal = c.terminator().clone();
            map_terminator(&mut terminal, map);
            terminal.map_successors(|id| *id = ids[id]);
            match terminal {
                Terminator::Return { value } => {
                    let count = body.len();
                    if let Some(dst) = destination.scalar {
                        body.push(match value {
                            Some(ValueOperand::Cell(src)) => I::Copy { src, dst },
                            _ => I::Set { dst, value: 0 },
                        });
                    }
                    if let (Some(value), Some(aggregate)) = (value, destination.aggregate)
                        && !matches!(value, ValueOperand::Cell(_))
                    {
                        copy_value(
                            &mut body,
                            value,
                            ValueOperand::aggregate(aggregate, result_cells(callee.return_type())),
                            result_cells(callee.return_type()),
                        );
                    }
                    sources.extend(std::iter::repeat_n(
                        call.terminator_source(),
                        body.len() - count,
                    ));
                    terminal = Terminator::Goto { target: *return_to };
                }
                Terminator::Call { .. } => {
                    let result = self.results[&c.id()];
                    let scalar = result.scalar.map(|mut address| {
                        map(&mut address);
                        address
                    });
                    let aggregate = result.aggregate.map(|mut region| {
                        map_region(&mut region, map);
                        region
                    });
                    self.results
                        .insert(ids[&c.id()], ResultStorage { scalar, aggregate });
                }
                _ => {}
            }
            self.nodes.push(
                Continuation::new(ids[&c.id()], call.function(), body, terminal)
                    .with_source_spans(sources, c.terminator_source()),
            );
        }
        // A fresh activation is zero on EVERY invocation, including a call site
        // in a loop. Fresh virtual IDs keep all argument sources disjoint from
        // this initialization and from the ordered, possibly aliased parameters.
        let mut body = call.body().to_vec();
        for slot in 0..callee.frame_slots() {
            body.push(I::Set {
                dst: Address::Frame(FrameSlot::new(scalar_base + slot)),
                value: 0,
            });
        }
        for a in callee.frame_aggregates() {
            for index in 0..a.cells() {
                body.push(I::Set {
                    dst: Address::ArrayElement {
                        array: AggregateRegion::Frame(FrameAggregateId::new(
                            aggregate_base + a.id().index(),
                        )),
                        index,
                    },
                    value: 0,
                });
            }
        }
        for (&argument, &parameter) in arguments.iter().zip(callee.parameter_locations()) {
            let (mut destination, cells) = match parameter {
                ParameterLocation::Cell(slot) => (ValueOperand::Cell(Address::Frame(slot)), 1),
                ParameterLocation::AggregateElement { aggregate, index } => (
                    ValueOperand::Cell(Address::ArrayElement {
                        array: AggregateRegion::Frame(aggregate),
                        index,
                    }),
                    1,
                ),
                ParameterLocation::Array(a) | ParameterLocation::Aggregate(a) => {
                    let cells = callee.frame_aggregate(a).unwrap().cells();
                    (
                        ValueOperand::aggregate(AggregateRegion::Frame(a), cells),
                        cells,
                    )
                }
            };
            map_operand(&mut destination, map);
            copy_value(&mut body, argument, destination, cells);
        }
        let mut sources = call.body_sources().to_vec();
        sources.resize(body.len(), call.terminator_source());
        self.nodes[call_index] = Continuation::new(
            call.id(),
            call.function(),
            body,
            Terminator::Goto {
                target: ids[&callee.entry()],
            },
        )
        .with_source_spans(sources, call.terminator_source());
        Ok(cloned.len())
    }

    fn materialize(
        mut self,
        original: &ContinuationProgram,
    ) -> Result<ContinuationProgram, String> {
        // Reachability is rooted at main, not at every function descriptor.
        let entries: HashMap<_, _> = self.functions.iter().map(|f| (f.id(), f.entry())).collect();
        let nodes: HashMap<_, _> = self.nodes.iter().map(|c| (c.id(), c)).collect();
        let mut reachable = HashSet::new();
        let mut pending = vec![entries[&original.main()]];
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            let c = nodes[&id];
            pending.extend(c.terminator().edges().map(|(id, _)| id));
            if let Some(callee) = c.terminator().callee() {
                pending.push(entries[&callee]);
            }
        }
        self.nodes.retain(|c| reachable.contains(&c.id()));
        let live_functions: HashSet<_> = self.nodes.iter().map(Continuation::function).collect();
        self.functions.retain(|f| live_functions.contains(&f.id()));
        let types: HashMap<_, _> = self
            .functions
            .iter()
            .map(|f| (f.id(), f.return_type()))
            .collect();
        // Reuse the physical ABI inbox for an actual activation when no Call
        // in that function routes the same result kind into an inlined child.
        // Aggregate capacity must still come entirely from surviving Calls.
        // Otherwise the virtual region preserves old/partially updated tails.
        for f in &self.functions {
            let own = self.owners[&f.id()];
            let calls = self
                .nodes
                .iter()
                .filter(|c| c.function() == f.id())
                .filter_map(|c| c.terminator().callee().map(|callee| (c.id(), callee)))
                .collect::<Vec<_>>();
            let scalar = own.scalar.is_some()
                && calls
                    .iter()
                    .all(|(id, _)| self.results[id].scalar == own.scalar);
            let capacity = calls
                .iter()
                .map(|(_, callee)| result_cells(types[callee]))
                .max()
                .unwrap_or(0);
            let aggregate = own.aggregate.is_some_and(|region| {
                let AggregateRegion::Frame(id) = region else {
                    unreachable!()
                };
                capacity >= f.frame_aggregate(id).unwrap().cells()
            }) && calls.iter().all(|(id, callee)| {
                result_cells(types[callee]) == 0 || self.results[id].aggregate == own.aggregate
            });
            let remap = &mut |address: &mut Address| {
                if scalar && Some(*address) == own.scalar {
                    *address = Address::AbiValue;
                }
                if let Address::ArrayElement { array, .. } = address
                    && aggregate
                    && Some(*array) == own.aggregate
                {
                    *array = AggregateRegion::Outbox;
                }
            };
            for c in self.nodes.iter_mut().filter(|c| c.function() == f.id()) {
                let mut body = c.body().to_vec();
                let mut terminal = c.terminator().clone();
                map_body(&mut body, remap);
                map_terminator(&mut terminal, remap);
                if let Some(result) = self.results.get_mut(&c.id()) {
                    if let Some(scalar) = &mut result.scalar {
                        remap(scalar);
                    }
                    if let Some(aggregate) = &mut result.aggregate {
                        map_region(aggregate, remap);
                    }
                }
                *c = Continuation::new(c.id(), c.function(), body, terminal)
                    .with_source_spans(c.body_sources().to_vec(), c.terminator_source());
            }
        }
        let mut outboxes = HashMap::<FunctionId, usize>::new();
        let mut bridges = Vec::new();
        for c in &mut self.nodes {
            let Terminator::Call {
                callee, return_to, ..
            } = c.terminator()
            else {
                continue;
            };
            let cells = result_cells(types[callee]);
            let capacity = outboxes.entry(c.function()).or_default();
            *capacity = (*capacity).max(cells);
            let result = self.results[&c.id()];
            let mut body = Vec::new();
            if let Some(dst) = result.scalar
                && dst != Address::AbiValue
            {
                body.push(I::Copy {
                    src: Address::AbiValue,
                    dst,
                });
            }
            if let Some(aggregate) = result.aggregate
                && aggregate != AggregateRegion::Outbox
                && cells > 0
            {
                copy_value(
                    &mut body,
                    ValueOperand::aggregate(AggregateRegion::Outbox, cells),
                    ValueOperand::aggregate(aggregate, cells),
                    cells,
                );
            }
            if body.is_empty() {
                continue;
            }
            let bridge = self.ids.fresh()?;
            let sources = vec![c.terminator_source(); body.len()];
            bridges.push(
                Continuation::new(
                    bridge,
                    c.function(),
                    body,
                    Terminator::Goto { target: *return_to },
                )
                .with_source_spans(sources, c.terminator_source()),
            );
            let mut terminal = c.terminator().clone();
            terminal.map_successors(|target| *target = bridge);
            *c = Continuation::new(c.id(), c.function(), c.body().to_vec(), terminal)
                .with_source_spans(c.body_sources().to_vec(), c.terminator_source());
        }
        self.nodes.extend(bridges);
        self.functions = self
            .functions
            .iter()
            .map(|f| {
                descriptor(
                    f,
                    f.frame_slots(),
                    f.frame_aggregates().to_vec(),
                    outboxes.get(&f.id()).copied().unwrap_or(0),
                )
            })
            .collect();
        ContinuationProgram::new_with_globals(
            original.main(),
            original.globals().to_vec(),
            self.functions,
            self.nodes,
        )
        .map(|p| p.with_source_files(original.source_files().to_vec()))
        .map_err(|e| e.to_string())
    }
}

fn element(operand: ValueOperand, index: usize) -> Address {
    match operand {
        ValueOperand::Cell(address) => {
            assert_eq!(index, 0);
            address
        }
        ValueOperand::Array(array) => Address::ArrayElement { array, index },
        ValueOperand::Aggregate { region, offset, .. } => Address::ArrayElement {
            array: region,
            index: offset + index,
        },
    }
}

fn copy_value(body: &mut Vec<I>, src: ValueOperand, dst: ValueOperand, cells: usize) {
    match (src, dst) {
        (
            ValueOperand::Aggregate {
                region: src,
                offset: 0,
                ..
            },
            ValueOperand::Aggregate {
                region: dst,
                offset: 0,
                ..
            },
        ) if cells > 0 => {
            body.push(I::AggregateCopy { src, dst, cells });
        }
        _ => {
            for index in 0..cells {
                body.push(I::Copy {
                    src: element(src, index),
                    dst: element(dst, index),
                });
            }
        }
    }
}

fn recursive_functions(program: &ContinuationProgram) -> HashSet<FunctionId> {
    let mut edges = HashMap::<FunctionId, Vec<FunctionId>>::new();
    for c in program.continuations() {
        if let Some(callee) = c.terminator().callee() {
            edges.entry(c.function()).or_default().push(callee);
        }
    }
    program
        .functions()
        .iter()
        .filter_map(|f| {
            let mut visited = HashSet::new();
            let mut pending = edges.get(&f.id()).cloned().unwrap_or_default();
            while let Some(id) = pending.pop() {
                if id == f.id() {
                    return Some(id);
                }
                if visited.insert(id) {
                    pending.extend(edges.get(&id).into_iter().flatten().copied());
                }
            }
            None
        })
        .collect()
}

/// Internal explicit selection during migration. Recursive SCCs always retain
/// their calls. The caller must supply unallocated storage, never physical slots.
pub(crate) fn inline_selected(
    program: &ContinuationProgram,
    selected: &[FunctionId],
) -> Result<(ContinuationProgram, InlineStats), String> {
    let selected: HashSet<_> = selected.iter().copied().collect();
    let mut stats = InlineStats::default();
    if selected.is_empty() {
        return Ok((program.clone(), stats));
    }
    let recursive = recursive_functions(program);
    let mut graph = Graph::normalize(program)?;
    let mut skipped = HashSet::new();
    loop {
        let candidate = graph.nodes.iter().find_map(|c| {
            let callee = c.terminator().callee()?;
            (selected.contains(&callee) && !skipped.contains(&c.id())).then_some((c.id(), callee))
        });
        let Some((id, callee)) = candidate else {
            break;
        };
        if recursive.contains(&callee) {
            skipped.insert(id);
            stats.recursive_calls_preserved += 1;
            continue;
        }
        let clones = graph
            .nodes
            .iter()
            .filter(|c| c.function() == callee)
            .count();
        let bridges = graph.results.len() + clones;
        if graph.nodes.len() + clones + bridges > usize::from(u16::MAX) {
            skipped.insert(id);
            stats.id_limit_calls_preserved += 1;
            continue;
        }
        stats.blocks_cloned += graph.splice(id)?;
        stats.calls_inlined += 1;
    }
    if stats.calls_inlined == 0 {
        return Ok((program.clone(), stats));
    }
    let materialized = graph.materialize(program)?;
    let cleaned = crate::virtual_cleanup::cleanup(&materialized).map_err(|e| e.to_string())?;
    Ok((cleaned, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_inline_retains_call_when_no_clone_ids_fit() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let id = |n| ContinuationId::new(n).unwrap();
        let mut nodes = (1..u16::MAX)
            .map(|n| Continuation::new(id(n), main, vec![], Terminator::Halt))
            .collect::<Vec<_>>();
        nodes[0] = Continuation::new(
            id(1),
            main,
            vec![],
            Terminator::Call {
                callee,
                arguments: vec![],
                return_to: id(2),
            },
        );
        nodes.push(Continuation::new(
            id(u16::MAX),
            callee,
            vec![],
            Terminator::Return { value: None },
        ));
        let program = ContinuationProgram::new(
            main,
            vec![
                FunctionDescriptor::new(main, vec![], 0, ValueType::Void, id(1)),
                FunctionDescriptor::new(callee, vec![], 0, ValueType::Void, id(u16::MAX)),
            ],
            nodes,
        )
        .unwrap();
        let (after, stats) = inline_selected(&program, &[callee]).unwrap();
        assert_eq!(after, program);
        assert_eq!(stats.calls_inlined, 0);
        assert_eq!(stats.id_limit_calls_preserved, 1);
    }
}
