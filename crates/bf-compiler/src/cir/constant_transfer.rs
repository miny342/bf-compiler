//! Fold adjacent constant initialization and destructive transfer.
//!
//! No value state crosses an instruction or control boundary. Keep the source
//! zero write and wrapping target updates, including zero contributions.

use crate::{Address, AggregateRegion, Continuation, FrameInstruction as I};

pub(crate) fn fold_continuations(continuations: Vec<Continuation>) -> Vec<Continuation> {
    continuations
        .into_iter()
        .map(|c| {
            let folded = fold_body(c.body());
            let sources = folded
                .iter()
                .map(|(_, origin)| c.body_sources()[*origin])
                .collect();
            let body = folded.into_iter().map(|(i, _)| i).collect();
            Continuation::new(c.id(), c.function(), body, c.terminator().clone())
                .with_source_spans(sources, c.terminator_source())
        })
        .collect()
}

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

fn fold_body(body: &[I]) -> Vec<(I, usize)> {
    let mut output = Vec::with_capacity(body.len());
    let mut index = 0;
    while index < body.len() {
        if let I::Set { dst, value } = body[index]
            && local(dst)
            && let Some(I::Transfer { src, targets }) = body.get(index + 1)
            && *src == dst
            && targets.iter().all(|t| t.dst != dst)
        {
            output.push((I::Set { dst, value: 0 }, index));
            for target in targets {
                let contribution = value.wrapping_mul(target.factor);
                if contribution != 0 {
                    output.push((
                        I::AddConst {
                            dst: target.dst,
                            value: contribution,
                        },
                        index + 1,
                    ));
                }
            }
            index += 2;
            continue;
        }
        let instruction = match &body[index] {
            I::Loop { condition, body } => I::Loop {
                condition: *condition,
                body: fold_body(body).into_iter().map(|(i, _)| i).collect(),
            },
            I::Branch {
                condition,
                then_body,
                else_body,
            } => I::Branch {
                condition: *condition,
                then_body: fold_body(then_body).into_iter().map(|(i, _)| i).collect(),
                else_body: fold_body(else_body).into_iter().map(|(i, _)| i).collect(),
            },
            instruction => instruction.clone(),
        };
        output.push((instruction, index));
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContinuationId, ContinuationProgram, FrameAggregateDescriptor, FrameAggregateId, FrameSlot,
        FrameTransferTarget, FunctionDescriptor, FunctionId, GlobalDescriptor, GlobalId,
        SourceSpan, Terminator, ValueType, run_continuations_with_io,
    };

    fn slot(index: usize) -> Address {
        Address::Frame(FrameSlot::new(index))
    }

    #[test]
    fn all_constant_bytes_preserve_wrap_fanout_and_source_zero() {
        let f = FunctionId::new(0);
        let entry = ContinuationId::new(1).unwrap();
        let region = AggregateRegion::Frame(FrameAggregateId::new(0));
        let element = |index| Address::ArrayElement {
            array: region,
            index,
        };
        let global = Address::Global(GlobalId::new(0));
        let mut body = Vec::new();
        let mut input = Vec::new();
        for value in 0..=255u8 {
            for factor in [0, 1, 2, 15, 16, 127, 128, 254, 255] {
                let source = if value.is_multiple_of(2) {
                    slot(0)
                } else {
                    element(0)
                };
                input.push(value.wrapping_mul(17).wrapping_add(factor));
                body.extend([
                    I::Input { dst: slot(1) },
                    I::Set {
                        dst: global,
                        value: 17,
                    },
                    I::Set {
                        dst: element(1),
                        value: 129,
                    },
                    I::Set { dst: source, value },
                    I::Transfer {
                        src: source,
                        targets: vec![
                            FrameTransferTarget {
                                dst: slot(1),
                                factor,
                            },
                            FrameTransferTarget {
                                dst: global,
                                factor: 255,
                            },
                            FrameTransferTarget {
                                dst: element(1),
                                factor: 16,
                            },
                        ]
                        .into_iter()
                        .filter(|target| target.factor != 0)
                        .collect(),
                    },
                    I::Output { src: slot(1) },
                    I::Output { src: slot(0) },
                    I::Output { src: element(0) },
                    I::Output { src: global },
                    I::Output { src: element(1) },
                ]);
            }
        }
        body.extend([
            I::Set {
                dst: slot(2),
                value: 2,
            },
            I::Loop {
                condition: slot(2),
                body: vec![
                    I::Set {
                        dst: slot(0),
                        value: 254,
                    },
                    I::Transfer {
                        src: slot(0),
                        targets: vec![FrameTransferTarget {
                            dst: slot(1),
                            factor: 255,
                        }],
                    },
                    I::AddConst {
                        dst: slot(2),
                        value: 255,
                    },
                ],
            },
            I::Branch {
                condition: slot(1),
                then_body: vec![
                    I::Set {
                        dst: slot(0),
                        value: 255,
                    },
                    I::Transfer {
                        src: slot(0),
                        targets: vec![FrameTransferTarget {
                            dst: global,
                            factor: 255,
                        }],
                    },
                ],
                else_body: vec![],
            },
            I::Output { src: slot(0) },
            I::Output { src: global },
        ]);
        let continuations = vec![Continuation::new(entry, f, body, Terminator::Halt)];
        let folded = fold_continuations(continuations.clone());
        assert!(
            folded[0]
                .body()
                .iter()
                .all(|i| !matches!(i, I::Transfer { .. }))
        );
        let function = FunctionDescriptor::new_aggregates(
            f,
            vec![],
            3,
            vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 2)],
            0,
            ValueType::Void,
            entry,
        );
        let execute = |continuations| {
            let p = ContinuationProgram::new_with_globals(
                f,
                vec![GlobalDescriptor::cell(GlobalId::new(0))],
                vec![function.clone()],
                continuations,
            )
            .unwrap();
            let mut output = Vec::new();
            run_continuations_with_io(
                &p,
                &mut input.as_slice(),
                &mut output,
                Default::default(),
                |_| {},
            )
            .unwrap();
            output
        };
        assert_eq!(execute(folded), execute(continuations));
    }

    #[test]
    fn adjacency_and_provenance_are_retained() {
        let f = FunctionId::new(0);
        let entry = ContinuationId::new(1).unwrap();
        let body = vec![
            I::Set {
                dst: slot(0),
                value: 254,
            },
            I::Transfer {
                src: slot(0),
                targets: vec![FrameTransferTarget {
                    dst: slot(1),
                    factor: 255,
                }],
            },
            I::Set {
                dst: slot(0),
                value: 255,
            },
            I::Output { src: slot(0) },
            I::Transfer {
                src: slot(0),
                targets: vec![FrameTransferTarget {
                    dst: slot(1),
                    factor: 1,
                }],
            },
        ];
        let spans: Vec<_> = (0..body.len())
            .map(|i| {
                Some(SourceSpan {
                    file_id: 0,
                    start_byte: i as u64,
                    end_byte: i as u64 + 1,
                })
            })
            .collect();
        let original = Continuation::new(entry, f, body, Terminator::Halt)
            .with_source_spans(spans.clone(), None);
        let result = fold_continuations(vec![original.clone()]);
        assert_eq!(
            result[0].body()[0],
            I::Set {
                dst: slot(0),
                value: 0
            }
        );
        assert_eq!(
            result[0].body()[1],
            I::AddConst {
                dst: slot(1),
                value: 2
            }
        );
        assert_eq!(&result[0].body()[2..], &original.body()[2..]);
        assert_eq!(result[0].body_sources(), spans);
    }
}
