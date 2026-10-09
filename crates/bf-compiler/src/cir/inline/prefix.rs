//! Hoist a pure entry prefix into every caller, retaining the callee's entry,
//! portal sites and resume identities. Handoff parameters are virtual live cells.
use super::*;
use crate::cir::effects::{self, Effect};

fn local(address: Address) -> bool {
    matches!(
        address,
        Address::Frame(_)
            | Address::ArrayElement {
                array: AggregateRegion::Frame(_),
                ..
            }
    )
}

fn cells(value: ValueOperand, function: &FunctionDescriptor) -> Vec<Address> {
    match value {
        ValueOperand::Cell(a) => vec![a],
        ValueOperand::Array(AggregateRegion::Frame(id)) => {
            (0..function.frame_aggregate(id).unwrap().cells())
                .map(|index| Address::ArrayElement {
                    array: AggregateRegion::Frame(id),
                    index,
                })
                .collect()
        }
        ValueOperand::Aggregate {
            region,
            offset,
            cells,
        } => (offset..offset + cells)
            .map(|index| Address::ArrayElement {
                array: region,
                index,
            })
            .collect(),
        _ => vec![],
    }
}

fn parameter_cells(function: &FunctionDescriptor, parameter: ParameterLocation) -> Vec<Address> {
    match parameter {
        ParameterLocation::Cell(slot) => vec![Address::Frame(slot)],
        ParameterLocation::AggregateElement { aggregate, index } => vec![Address::ArrayElement {
            array: AggregateRegion::Frame(aggregate),
            index,
        }],
        ParameterLocation::Array(id) | ParameterLocation::Aggregate(id) => {
            cells(ValueOperand::Array(AggregateRegion::Frame(id)), function)
        }
    }
}

pub(crate) fn hoist_prefixes(
    program: &ContinuationProgram,
    max_arguments: usize,
) -> Result<(ContinuationProgram, usize), String> {
    let cleaned = crate::cir::virtual_cleanup::cleanup(program).map_err(|e| e.to_string())?;
    let recursive = recursive_functions(&cleaned);
    let mut graph = Graph::normalize(&cleaned)?;
    let mut moved = 0;
    let functions = graph.functions.clone();
    for original in functions {
        let function = graph
            .functions
            .iter()
            .find(|f| f.id() == original.id())
            .unwrap()
            .clone();
        if function.id() == cleaned.main() || recursive.contains(&function.id()) {
            continue;
        }
        // Keep caller-side activation snapshots bounded even for large local
        // arrays. Unsupported large prefixes retain the ordinary shared entry.
        let storage = function
            .frame_aggregates()
            .iter()
            .fold(function.frame_slots(), |sum, aggregate| {
                sum.saturating_add(aggregate.cells())
            });
        if storage > 4096 {
            continue;
        }
        if !graph.nodes.iter().any(|c| {
            c.function() == function.id()
                && c.terminator().boundary() == crate::cir::ir::BoundaryKind::Hard
        }) || graph
            .nodes
            .iter()
            .filter(|c| c.function() == function.id())
            .any(|c| c.terminator().edges().any(|(id, _)| id == function.entry()))
        {
            continue;
        }
        let entry_index = graph
            .nodes
            .iter()
            .position(|c| c.id() == function.entry())
            .unwrap();
        let entry = graph.nodes[entry_index].clone();
        let prefix_len = entry
            .body()
            .iter()
            .take_while(|i| {
                if matches!(
                    i,
                    I::Input { .. } | I::Output { .. } | I::Loop { .. } | I::Branch { .. }
                ) {
                    return false;
                }
                let mut pure = true;
                effects::instruction(i, |effect| match effect {
                    Effect::Write(v) => pure &= cells(v, &function).iter().all(|&a| local(a)),
                    Effect::Clobber(a) => pure &= local(a),
                    _ => {}
                });
                pure
            })
            .count();
        if !(2..=128).contains(&prefix_len) {
            continue;
        }
        let prefix = &entry.body()[..prefix_len];
        let mut inputs = HashSet::new();
        for &parameter in function.parameter_locations() {
            inputs.extend(parameter_cells(&function, parameter));
        }
        for instruction in prefix {
            effects::instruction(instruction, |effect| match effect {
                Effect::Write(v) => inputs.extend(cells(v, &function)),
                Effect::Clobber(a) => {
                    inputs.insert(a);
                }
                _ => {}
            });
        }
        // Only values live at the handoff matter. Reads after an intervening
        // overwrite must not turn dead prefix temporaries into parameters.
        let nodes = graph
            .nodes
            .iter()
            .filter(|c| c.function() == function.id())
            .cloned()
            .collect::<Vec<_>>();
        let entry_in_nodes = nodes.iter().position(|c| c.id() == entry.id()).unwrap();
        let exits = crate::cir::virtual_cleanup::function_exits(&function, &nodes);
        let live = crate::cir::virtual_cleanup::body_entry(
            &entry.body()[prefix_len..],
            exits[entry_in_nodes].clone(),
            &function,
        );
        let mut handoff = inputs
            .into_iter()
            .filter(|&a| local(a) && live.observed(ValueOperand::Cell(a), &function))
            .collect::<Vec<_>>();
        handoff.sort_by_key(|a| match a {
            Address::Frame(slot) => (0, slot.index(), 0),
            Address::ArrayElement {
                array: AggregateRegion::Frame(id),
                index,
            } => (1, id.index(), *index),
            _ => unreachable!(),
        });
        if handoff.len() > max_arguments {
            continue;
        }
        let old_arguments: usize = function
            .parameter_locations()
            .iter()
            .map(|&p| parameter_cells(&function, p).len())
            .sum();
        if std::env::var("BFC_EVAL_ENTRY_PREFIX_REDUCE").as_deref() == Ok("1")
            && handoff.len() >= old_arguments
        {
            continue;
        }
        let sites = graph
            .nodes
            .iter()
            .filter(|c| c.terminator().callee() == Some(function.id()))
            .map(Continuation::id)
            .collect::<Vec<_>>();
        if sites.is_empty() {
            continue;
        }
        for site in sites {
            let index = graph.nodes.iter().position(|c| c.id() == site).unwrap();
            let call = graph.nodes[index].clone();
            let Terminator::Call {
                arguments,
                return_to,
                ..
            } = call.terminator()
            else {
                unreachable!()
            };
            let caller_index = graph
                .functions
                .iter()
                .position(|f| f.id() == call.function())
                .unwrap();
            let caller = graph.functions[caller_index].clone();
            let scalar_base = caller.frame_slots();
            let aggregate_base = caller.frame_aggregates().len();
            let mut aggregates = caller.frame_aggregates().to_vec();
            aggregates.extend(function.frame_aggregates().iter().map(|a| {
                FrameAggregateDescriptor::new(
                    FrameAggregateId::new(aggregate_base + a.id().index()),
                    a.cells(),
                )
            }));
            graph.functions[caller_index] = descriptor(
                &caller,
                scalar_base
                    .checked_add(function.frame_slots())
                    .ok_or("CIR prefix frame overflow")?,
                aggregates,
                caller.outbox_cells(),
            );
            let map = &mut |a: &mut Address| match a {
                Address::Frame(slot) => *slot = FrameSlot::new(scalar_base + slot.index()),
                Address::ArrayElement {
                    array: AggregateRegion::Frame(id),
                    ..
                } => *id = FrameAggregateId::new(aggregate_base + id.index()),
                _ => {}
            };
            let mut body = call.body().to_vec();
            for slot in 0..function.frame_slots() {
                body.push(I::Set {
                    dst: Address::Frame(FrameSlot::new(scalar_base + slot)),
                    value: 0,
                });
            }
            for a in function.frame_aggregates() {
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
            for (&parameter, &argument) in function.parameter_locations().iter().zip(arguments) {
                for (offset, mut dst) in parameter_cells(&function, parameter)
                    .into_iter()
                    .enumerate()
                {
                    map(&mut dst);
                    body.push(I::Copy {
                        src: element(argument, offset),
                        dst,
                    });
                }
            }
            let mut copied = prefix.to_vec();
            map_body(&mut copied, map);
            let mut sources = call.body_sources().to_vec();
            sources.resize(body.len(), call.terminator_source());
            sources.extend_from_slice(&entry.body_sources()[..prefix_len]);
            body.extend(copied);
            let arguments = handoff
                .iter()
                .map(|&a| {
                    let mut a = a;
                    map(&mut a);
                    ValueOperand::Cell(a)
                })
                .collect();
            graph.nodes[index] = Continuation::new(
                call.id(),
                call.function(),
                body,
                Terminator::Call {
                    callee: function.id(),
                    arguments,
                    return_to: *return_to,
                },
            )
            .with_source_spans(sources, call.terminator_source());
        }
        graph.nodes[entry_index] = Continuation::new(
            entry.id(),
            entry.function(),
            entry.body()[prefix_len..].to_vec(),
            entry.terminator().clone(),
        )
        .with_source_spans(
            entry.body_sources()[prefix_len..].to_vec(),
            entry.terminator_source(),
        );
        let parameters = handoff
            .into_iter()
            .map(|a| match a {
                Address::Frame(slot) => ParameterLocation::Cell(slot),
                Address::ArrayElement {
                    array: AggregateRegion::Frame(aggregate),
                    index,
                } => ParameterLocation::AggregateElement { aggregate, index },
                _ => unreachable!(),
            })
            .collect();
        let updated = FunctionDescriptor::new_aggregates(
            function.id(),
            parameters,
            function.frame_slots(),
            function.frame_aggregates().to_vec(),
            function.outbox_cells(),
            function.return_type(),
            function.entry(),
        );
        let descriptor = graph
            .functions
            .iter_mut()
            .find(|f| f.id() == function.id())
            .unwrap();
        *descriptor = match function.name() {
            Some(name) => updated.with_name(name),
            None => updated,
        };
        moved += 1;
    }
    let program = graph.materialize(&cleaned)?;
    crate::cir::virtual_cleanup::cleanup(&program)
        .map(|p| (p, moved))
        .map_err(|e| e.to_string())
}
