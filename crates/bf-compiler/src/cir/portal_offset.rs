//! Forward dead aggregate fields directly into single-cell global portals.
//!
//! Only a suffix of field-to-temporary copies is removed. No writes or control
//! boundaries are crossed, both the temporary and field must die at the
//! portal, and payload aliases are excluded. Multi-cell portals mutate their
//! offsets, so they deliberately retain private scalar snapshots.

use std::collections::HashMap;

use crate::cir::virtual_cleanup::{Live, body_entry, clean_function, function_exits};
use crate::{
    Address, AggregateRegion, Continuation, FrameInstruction as I, FunctionDescriptor, Terminator,
    ValueOperand as V,
};

pub(crate) fn optimize(
    f: FunctionDescriptor,
    nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    if !nodes.iter().any(|c| {
        matches!(
            c.terminator(),
            Terminator::AggregateLoad {
                source: AggregateRegion::Global(_),
                cells: 1,
                ..
            } | Terminator::AggregateStore {
                destination: AggregateRegion::Global(_),
                cells: 1,
                ..
            }
        ) && matches!(
            c.body().last(),
            Some(I::Copy {
                src: Address::ArrayElement {
                    array: AggregateRegion::Frame(_),
                    ..
                },
                ..
            })
        )
    }) {
        return (f, nodes);
    }
    let entries: HashMap<_, _> = nodes
        .iter()
        .zip(function_exits(&f, &nodes))
        .map(|(c, live)| (c.id(), body_entry(c.body(), live, &f)))
        .collect();
    let mut changed = false;
    let rewritten = nodes
        .iter()
        .map(|c| {
            let mut terminal = c.terminator().clone();
            let (offset, operand, return_to) = match &mut terminal {
                Terminator::AggregateLoad {
                    source: AggregateRegion::Global(_),
                    offset,
                    cells: 1,
                    destination,
                    return_to,
                } => (offset, *destination, *return_to),
                Terminator::AggregateStore {
                    destination: AggregateRegion::Global(_),
                    offset,
                    cells: 1,
                    source,
                    return_to,
                } => (offset, *source, *return_to),
                _ => return c.clone(),
            };
            let live = &entries[&return_to];
            let mut payload = Live::default();
            payload.read(operand, &f);
            let mut kept = c.body().len();
            let mut forwarded = Vec::new();
            while kept > 0 {
                let I::Copy { src, dst } = c.body()[kept - 1] else {
                    break;
                };
                if !matches!(
                    src,
                    Address::ArrayElement {
                        array: AggregateRegion::Frame(_),
                        ..
                    }
                ) || !matches!(dst, Address::Frame(_) | Address::ArrayElement {
                    array: AggregateRegion::Frame(_), ..
                })
                    // A preceding copy may define the source of an already
                    // forwarded offset. Keep that definition in the body.
                    || forwarded.contains(&dst)
                    || live.observed(V::Cell(dst), &f)
                    || live.observed(V::Cell(src), &f)
                    || payload.observed(V::Cell(src), &f)
                    || payload.observed(V::Cell(dst), &f)
                {
                    break;
                }
                if dst == offset.low && src != offset.high {
                    offset.low = src;
                } else if dst == offset.high && src != offset.low {
                    offset.high = src;
                } else {
                    break;
                }
                forwarded.push(src);
                kept -= 1;
            }
            changed |= kept != c.body().len();
            Continuation::new(c.id(), c.function(), c.body()[..kept].to_vec(), terminal)
                .with_source_spans(c.body_sources()[..kept].to_vec(), c.terminator_source())
        })
        .collect::<Vec<_>>();
    if changed {
        clean_function(&f, &rewritten)
    } else {
        (f, nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContinuationId, FrameAggregateDescriptor, FrameAggregateId, FrameSlot, FunctionId,
        GlobalId, LogicalOffset, ValueType,
    };

    fn fixture(
        global: bool,
        cells: usize,
        live: Option<Address>,
        payload: Address,
    ) -> (FunctionDescriptor, Vec<Continuation>) {
        let f = FunctionId::new(0);
        let id = |i| ContinuationId::new(i).unwrap();
        let field = |index| Address::ArrayElement {
            array: AggregateRegion::Frame(FrameAggregateId::new(0)),
            index,
        };
        let slot = |i| Address::Frame(FrameSlot::new(i));
        let descriptor = FunctionDescriptor::new_aggregates(
            f,
            vec![],
            3,
            vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 4)],
            0,
            ValueType::Void,
            id(1),
        );
        (
            descriptor,
            vec![
                Continuation::new(
                    id(1),
                    f,
                    vec![
                        I::Copy {
                            src: field(0),
                            dst: slot(0),
                        },
                        I::Copy {
                            src: field(1),
                            dst: slot(1),
                        },
                    ],
                    Terminator::AggregateStore {
                        destination: if global {
                            AggregateRegion::Global(GlobalId::new(0))
                        } else {
                            AggregateRegion::Frame(FrameAggregateId::new(0))
                        },
                        offset: LogicalOffset::new(slot(1), slot(0)),
                        cells,
                        source: V::Cell(payload),
                        return_to: id(2),
                    },
                ),
                Continuation::new(
                    id(2),
                    f,
                    live.into_iter().map(|src| I::Output { src }).collect(),
                    Terminator::Halt,
                ),
            ],
        )
    }

    #[test]
    fn dead_fields_replace_both_offset_copies() {
        let slot = Address::Frame(FrameSlot::new(2));
        let (f, nodes) = fixture(true, 1, None, slot);
        let (_, nodes) = optimize(f, nodes);
        assert!(nodes[0].body().is_empty());
        let Terminator::AggregateStore { offset, .. } = nodes[0].terminator() else {
            panic!()
        };
        for address in [offset.low, offset.high] {
            assert!(matches!(address, Address::ArrayElement { .. }));
        }
    }

    #[test]
    fn live_sources_temporaries_aliases_and_multi_cell_offsets_are_retained() {
        let field = Address::ArrayElement {
            array: AggregateRegion::Frame(FrameAggregateId::new(0)),
            index: 1,
        };
        let slot = Address::Frame(FrameSlot::new(2));
        for (global, cells, live, payload) in [
            (false, 1, None, slot),
            (true, 2, None, slot),
            (true, 1, Some(field), slot),
            (true, 1, Some(Address::Frame(FrameSlot::new(1))), slot),
            (true, 1, None, field),
        ] {
            let (f, nodes) = fixture(global, cells, live, payload);
            let (_, rewritten) = optimize(f, nodes.clone());
            assert_eq!(rewritten, nodes);
        }
    }

    #[test]
    fn flat_frame_forwarding_keeps_definitions_and_payload_snapshots() {
        let field = |index| Address::ArrayElement {
            array: AggregateRegion::Frame(FrameAggregateId::new(0)),
            index,
        };
        for payload_alias in [false, true] {
            let (f, mut nodes) = fixture(true, 1, None, Address::Frame(FrameSlot::new(2)));
            let mut terminal = nodes[0].terminator().clone();
            let Terminator::AggregateStore { offset, source, .. } = &mut terminal else {
                panic!()
            };
            *offset = crate::LogicalOffset::new(field(1), field(3));
            if payload_alias {
                *source = V::Aggregate {
                    region: AggregateRegion::Frame(FrameAggregateId::new(0)),
                    offset: 1,
                    cells: 1,
                };
            }
            nodes[0] = Continuation::new(
                nodes[0].id(),
                f.id(),
                vec![
                    I::Copy {
                        src: field(2),
                        dst: field(0),
                    },
                    I::Copy {
                        src: field(0),
                        dst: field(1),
                    },
                ],
                terminal,
            );
            let (_, rewritten) = optimize(f, nodes.clone());
            if payload_alias {
                assert_eq!(rewritten, nodes);
            } else {
                assert_eq!(rewritten[0].body(), &nodes[0].body()[..1]);
                let Terminator::AggregateStore { offset, .. } = rewritten[0].terminator() else {
                    panic!()
                };
                assert_ne!(offset.low, offset.high);
            }
        }
    }
}
