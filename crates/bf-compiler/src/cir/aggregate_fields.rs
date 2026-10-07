//! Scalar replacement of small, statically accessed local aggregates.
//!
//! A field has its own lifetime and allocation color. Regions used by a portal
//! or as aggregate ABI operands retain contiguous storage; public flat frames
//! and aggregate parameters therefore keep their original representation.

use std::collections::HashMap;

use crate::cir::effects::{self, Effect};
use crate::cir::operands::{map_body, map_terminator};
use crate::{
    Address, AggregateRegion, Continuation, FrameAggregateId, FrameInstruction as I, FrameSlot,
    FunctionDescriptor, ParameterLocation, SourceSpan, ValueOperand as V,
};

const MAX_FIELDS: usize = 16;
type Fields = HashMap<FrameAggregateId, usize>;

fn exclude(region: AggregateRegion, fields: &mut Fields) {
    if let AggregateRegion::Frame(id) = region {
        fields.remove(&id);
    }
}

fn address(region: AggregateRegion, index: usize, fields: &Fields) -> Address {
    if let AggregateRegion::Frame(id) = region
        && let Some(&base) = fields.get(&id)
    {
        return Address::Frame(FrameSlot::new(base + index));
    }
    Address::ArrayElement {
        array: region,
        index,
    }
}

fn rewrite_body(
    body: &[I],
    sources: &[Option<SourceSpan>],
    fields: &Fields,
) -> (Vec<I>, Vec<Option<SourceSpan>>) {
    let mut result = Vec::new();
    let mut spans = Vec::new();
    for (index, instruction) in body.iter().enumerate() {
        let mut replacement = match instruction {
            I::AggregateCopy { src, dst, cells }
                if [*src, *dst].iter().any(|region| {
                    matches!(region, AggregateRegion::Frame(id) if fields.contains_key(id))
                }) =>
            {
                // Distinct virtual regions cannot overlap. A self-copy has no
                // effect, and must not introduce sequential aliasing writes.
                if src == dst {
                    Vec::new()
                } else {
                    (0..*cells)
                        .map(|index| I::Copy {
                            src: address(*src, index, fields),
                            dst: address(*dst, index, fields),
                        })
                        .collect()
                }
            }
            I::Loop { condition, body } => vec![I::Loop {
                condition: *condition,
                body: rewrite_body(body, &[], fields).0,
            }],
            I::Branch { condition, then_body, else_body } => vec![I::Branch {
                condition: *condition,
                then_body: rewrite_body(then_body, &[], fields).0,
                else_body: rewrite_body(else_body, &[], fields).0,
            }],
            _ => vec![instruction.clone()],
        };
        // All AggregateCopy operands involving these regions have already
        // become scalar copies; region operands left here must remain regions.
        map_body(&mut replacement, &mut |a| {
            if let Address::ArrayElement { array, index } = *a {
                *a = address(array, index, fields);
            }
        });
        spans.extend(std::iter::repeat_n(
            sources.get(index).copied().flatten(),
            replacement.len(),
        ));
        result.extend(replacement);
    }
    (result, spans)
}

pub(crate) fn scalarize(
    f: FunctionDescriptor,
    nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    let mut fields: Fields = f
        .frame_aggregates()
        .iter()
        .filter(|a| a.cells() <= MAX_FIELDS)
        .map(|a| (a.id(), 0))
        .collect();
    for parameter in f.parameter_locations() {
        match *parameter {
            ParameterLocation::Cell(_) => {}
            ParameterLocation::Array(id)
            | ParameterLocation::Aggregate(id)
            | ParameterLocation::AggregateElement { aggregate: id, .. } => {
                fields.remove(&id);
            }
        }
    }
    for c in &nodes {
        effects::terminator(c.terminator(), |effect| match effect {
            Effect::Read(V::Array(region) | V::Aggregate { region, .. })
            | Effect::Write(V::Array(region) | V::Aggregate { region, .. })
            | Effect::MayWrite(region) => exclude(region, &mut fields),
            _ => {}
        });
    }
    if fields.is_empty() {
        return (f, nodes);
    }
    let mut slots = f.frame_slots();
    // Descriptor order, rather than HashMap iteration, makes allocation stable.
    for aggregate in f.frame_aggregates() {
        if let Some(base) = fields.get_mut(&aggregate.id()) {
            *base = slots;
            slots += aggregate.cells();
        }
    }
    let rewritten = nodes
        .into_iter()
        .map(|c| {
            let (body, sources) = rewrite_body(c.body(), c.body_sources(), &fields);
            let mut terminal = c.terminator().clone();
            map_terminator(&mut terminal, &mut |a| {
                if let Address::ArrayElement { array, index } = *a {
                    *a = address(array, index, &fields);
                }
            });
            Continuation::new(c.id(), c.function(), body, terminal)
                .with_source_spans(sources, c.terminator_source())
        })
        .collect::<Vec<_>>();
    // Prune dead fields before the affine graph sees them. This also removes
    // the replaced aggregate descriptors, so allocation does not reserve both
    // scalar fields and the old whole-region payload.
    crate::cir::virtual_cleanup::clean_function(&f.with_frame_slots(slots), &rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContinuationId, ContinuationProgram, FrameAggregateDescriptor, FunctionId, Terminator,
        ValueType, run_continuations_with_io,
    };

    fn region(n: usize) -> AggregateRegion {
        AggregateRegion::Frame(FrameAggregateId::new(n))
    }
    fn field(n: usize, index: usize) -> Address {
        Address::ArrayElement {
            array: region(n),
            index,
        }
    }
    fn slot(n: usize) -> Address {
        Address::Frame(FrameSlot::new(n))
    }
    fn id(n: u16) -> ContinuationId {
        ContinuationId::new(n).unwrap()
    }
    fn run(program: &ContinuationProgram, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        run_continuations_with_io(
            program,
            &mut &input[..],
            &mut output,
            Default::default(),
            |_| {},
        )
        .unwrap();
        output
    }

    #[test]
    fn field_snapshots_dead_writes_and_continuation_loops() {
        let main = FunctionId::new(0);
        let f = FunctionDescriptor::new_aggregates(
            main,
            vec![],
            2,
            (0..2)
                .map(|n| FrameAggregateDescriptor::new(FrameAggregateId::new(n), 3))
                .collect(),
            0,
            ValueType::Void,
            id(1),
        );
        let original = ContinuationProgram::new(
            main,
            vec![f.clone()],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![
                        I::Input { dst: field(0, 0) },
                        I::Input { dst: field(0, 1) },
                        I::Set {
                            dst: field(0, 2),
                            value: 255,
                        },
                        I::AggregateCopy {
                            src: region(0),
                            dst: region(1),
                            cells: 3,
                        },
                        I::AddConst {
                            dst: field(0, 0),
                            value: 1,
                        },
                        I::Set {
                            dst: slot(0),
                            value: 3,
                        },
                    ],
                    Terminator::Goto { target: id(2) },
                ),
                Continuation::new(
                    id(2),
                    main,
                    vec![
                        I::Copy {
                            src: field(1, 1),
                            dst: slot(1),
                        },
                        I::Branch {
                            condition: slot(1),
                            then_body: vec![I::AddConst {
                                dst: field(1, 0),
                                value: 7,
                            }],
                            else_body: vec![I::AddConst {
                                dst: field(1, 0),
                                value: 255,
                            }],
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
                        else_target: id(3),
                    },
                ),
                Continuation::new(
                    id(3),
                    main,
                    vec![
                        I::Output { src: field(0, 0) },
                        I::Output { src: field(1, 0) },
                        I::Set {
                            dst: field(1, 0),
                            value: 99,
                        },
                    ],
                    Terminator::Halt,
                ),
            ],
        )
        .unwrap();
        let (descriptor, nodes) = scalarize(f, original.continuations().to_vec());
        assert!(descriptor.frame_aggregates().is_empty());
        assert!(descriptor.frame_slots() <= 6); // Both dead third fields disappeared.
        let rewritten = ContinuationProgram::new(main, vec![descriptor], nodes).unwrap();
        let (allocated, _) =
            crate::cir::pipeline::optimize_and_allocate(&rewritten, Default::default()).unwrap();
        for value in 0..=255u8 {
            let input = [value, value.wrapping_mul(19)];
            let expected = run(&original, &input);
            assert_eq!(run(&rewritten, &input), expected);
            assert_eq!(run(&allocated, &input), expected);
            assert_eq!(
                bf_interpreter::run(
                    crate::compile_continuations(&allocated).unwrap().as_bytes(),
                    &input
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn portal_regions_and_aggregate_interfaces_remain_contiguous() {
        let main = FunctionId::new(0);
        let f = FunctionDescriptor::new_aggregates(
            main,
            vec![ParameterLocation::Aggregate(FrameAggregateId::new(0))],
            2,
            (0..3)
                .map(|n| FrameAggregateDescriptor::new(FrameAggregateId::new(n), 3))
                .collect(),
            0,
            ValueType::Aggregate { cells: 3 },
            id(1),
        );
        let c = Continuation::new(
            id(1),
            main,
            vec![
                I::AggregateCopy {
                    src: region(0),
                    dst: region(2),
                    cells: 3,
                },
                I::Output { src: field(2, 1) },
            ],
            Terminator::ArrayLoad {
                array: region(1),
                index: slot(0),
                destination: slot(1),
                return_to: id(2),
            },
        );
        let r = Continuation::new(
            id(2),
            main,
            vec![],
            Terminator::Return {
                value: Some(V::aggregate(region(0), 3)),
            },
        );
        let (descriptor, nodes) = scalarize(f, vec![c, r]);
        assert_eq!(descriptor.frame_aggregates().len(), 2);
        assert_eq!(
            descriptor.parameter_locations(),
            &[ParameterLocation::Aggregate(FrameAggregateId::new(0))]
        );
        assert!(
            nodes[0]
                .body()
                .iter()
                .all(|i| !matches!(i, I::AggregateCopy { .. }))
        );
        assert!(matches!(
            nodes[0].terminator(),
            Terminator::ArrayLoad {
                array: AggregateRegion::Frame(_),
                ..
            }
        ));
    }
}
