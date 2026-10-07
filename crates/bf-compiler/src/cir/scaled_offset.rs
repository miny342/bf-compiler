//! Small constant strides without a carry test on every source decrement.

use std::collections::HashMap;

use crate::cir::virtual_cleanup::{Live, body_entry, function_exits};
use crate::{
    Address, Continuation, FrameInstruction as I, FrameSlot, FrameTransferTarget,
    FunctionDescriptor, LogicalOffset, ValueOperand as V,
};

struct Scaled {
    source: Address,
    offset: LogicalOffset,
    stride: u16,
    dead: Vec<Address>,
    restore: Option<Address>,
}

// Recognize the frontend and public-CIR adapter's exact carry loops. This is
// deliberately after inline selection: the shorter expansion must not change
// which calls are inlined, undoing its savings with extra argument copies.
fn match_loop(instruction: &I) -> Option<Scaled> {
    let I::Loop {
        condition: source,
        body,
    } = instruction
    else {
        return None;
    };
    if let [
        I::Set {
            dst: left,
            value: 0,
        },
        I::Transfer { src: low, targets },
        I::Transfer {
            src: restore,
            targets: restored,
        },
        I::Set {
            dst: right,
            value: threshold,
        },
        I::Compare {
            left: lhs,
            right: rhs,
            dst: carry,
            true_value: 0,
            false_value: 1,
        },
        I::AddConst {
            dst: added,
            value: stride,
        },
        I::Branch {
            condition,
            then_body,
            else_body,
        },
        I::AddConst {
            dst: consumed,
            value: 255,
        },
    ] = body.as_slice()
        && (2..=8).contains(stride)
        && *threshold == 0u8.wrapping_sub(*stride)
        && targets.as_slice()
            == [
                FrameTransferTarget {
                    dst: *left,
                    factor: 1,
                },
                FrameTransferTarget {
                    dst: *restore,
                    factor: 1,
                },
            ]
        && restored.as_slice()
            == [FrameTransferTarget {
                dst: *low,
                factor: 1,
            }]
        && lhs == left
        && rhs == right
        && added == low
        && condition == carry
        && consumed == source
        && else_body.is_empty()
        && let [
            I::AddConst {
                dst: high,
                value: 1,
            },
        ] = then_body.as_slice()
    {
        return Some(Scaled {
            source: *source,
            offset: LogicalOffset::new(*low, *high),
            stride: u16::from(*stride),
            dead: vec![*left, *right, *carry],
            restore: Some(*restore),
        });
    }
    let (first, steps) = body.split_first()?;
    if *first
        != (I::AddConst {
            dst: *source,
            value: 255,
        })
        || steps.len() % 3 != 0
        || !(2..=8).contains(&(steps.len() / 3))
    {
        return None;
    }
    let mut operands = None;
    for step in steps.as_chunks::<3>().0 {
        let [
            I::AddConst { dst: low, value: 1 },
            I::Copy { src, dst: test },
            I::Branch {
                condition,
                then_body,
                else_body,
            },
        ] = step
        else {
            return None;
        };
        let [
            I::AddConst {
                dst: high,
                value: 1,
            },
        ] = else_body.as_slice()
        else {
            return None;
        };
        if src != low || condition != test || !then_body.is_empty() {
            return None;
        }
        let current = (*low, *high, *test);
        if operands.is_some_and(|old| old != current) {
            return None;
        }
        operands = Some(current);
    }
    let (low, high, test) = operands?;
    Some(Scaled {
        source: *source,
        offset: LogicalOffset::new(low, high),
        stride: (steps.len() / 3) as u16,
        dead: vec![test],
        restore: None,
    })
}

pub(crate) fn optimize(
    f: FunctionDescriptor,
    nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    // A frontend restore temporary begins at zero and is used only by its
    // matched copy. Inlined activation clears may also write zero to it.
    // Count all other uses, including terminators; parameters are never fresh.
    let mut uses = HashMap::<Address, usize>::new();
    fn record(body: &[I], uses: &mut HashMap<Address, usize>, eligible: &mut bool) {
        for i in body {
            match i {
                I::Set { value: 0, .. } => {}
                I::Loop { condition, body } => {
                    *uses.entry(*condition).or_default() += 1;
                    *eligible |= match_loop(i).is_some();
                    record(body, uses, eligible);
                }
                I::Branch {
                    condition,
                    then_body,
                    else_body,
                } => {
                    *uses.entry(*condition).or_default() += 1;
                    record(then_body, uses, eligible);
                    record(else_body, uses, eligible);
                }
                _ => crate::cir::operands::map_body(&mut [i.clone()], &mut |a| {
                    *uses.entry(*a).or_default() += 1;
                }),
            }
        }
    }
    let mut eligible = false;
    for c in &nodes {
        record(c.body(), &mut uses, &mut eligible);
        crate::cir::operands::map_terminator(&mut c.terminator().clone(), &mut |a| {
            *uses.entry(*a).or_default() += 1;
        });
    }
    for p in f.parameter_locations() {
        if let crate::ParameterLocation::Cell(slot) = p {
            *uses.entry(Address::Frame(*slot)).or_default() += 1;
        }
    }
    if !eligible {
        return (f, nodes);
    }
    let exits = function_exits(&f, &nodes);
    let scratch: Vec<_> = (f.frame_slots()..f.frame_slots() + 10)
        .map(|n| Address::Frame(FrameSlot::new(n)))
        .collect();
    let mut needed = 0;
    fn rewrite(
        body: &[I],
        exit: Live,
        f: &FunctionDescriptor,
        uses: &HashMap<Address, usize>,
        scratch: &[Address],
        needed: &mut usize,
    ) -> Vec<I> {
        let mut live = exit;
        let mut after = Vec::with_capacity(body.len());
        for i in body.iter().rev() {
            after.push(live.clone());
            live = body_entry(std::slice::from_ref(i), live, f);
        }
        after.reverse();
        let mut result = Vec::new();
        for (n, (i, live)) in body.iter().zip(after).enumerate() {
            if let Some(scaled) = match_loop(i)
                && scaled.dead.iter().all(|&a| !live.observed(V::Cell(a), f))
                && scaled
                    .restore
                    .is_none_or(|a| matches!(a, Address::Frame(_)) && uses.get(&a) == Some(&2))
                && let Some(low) = known_byte(&body[..n], scaled.offset.low)
            {
                let mut distinct = vec![scaled.source, scaled.offset.low, scaled.offset.high];
                distinct.extend(scaled.dead.iter().copied());
                distinct.extend(scaled.restore);
                if distinct
                    .iter()
                    .enumerate()
                    .all(|(n, a)| !distinct[..n].contains(a))
                {
                    let count = usize::from((u16::from(low) + 255 * scaled.stride) / 256) + 2;
                    let mut work = scaled.dead.clone();
                    work.extend(scaled.restore);
                    // Imported flat CIR already reserves arithmetic scratch at
                    // the end of its aggregate. Reuse nearby dead elements
                    // before growing the scalar frame; keep every live alias.
                    if scaled.restore.is_none()
                        && let Address::ArrayElement {
                            array: crate::AggregateRegion::Frame(a),
                            index,
                        } = work[0]
                    {
                        let cells = f.frame_aggregate(a).expect("validated region").cells();
                        for index in index + 1..(index + 10).min(cells) {
                            let candidate = Address::ArrayElement {
                                array: crate::AggregateRegion::Frame(a),
                                index,
                            };
                            if work.len() >= count {
                                break;
                            }
                            if !distinct.contains(&candidate)
                                && !live.observed(V::Cell(candidate), f)
                            {
                                work.push(candidate);
                            }
                        }
                    }
                    let extra = count.saturating_sub(work.len());
                    work.truncate(count);
                    work.extend_from_slice(&scratch[..extra]);
                    let mut expansion = Vec::new();
                    if append_small_scaled_offset(
                        scaled.source,
                        scaled.offset,
                        scaled.stride,
                        low,
                        &work,
                        &mut expansion,
                    ) {
                        result.push(I::Loop {
                            condition: scaled.source,
                            body: expansion,
                        });
                        *needed = (*needed).max(extra);
                        continue;
                    }
                }
            }
            result.push(match i {
                I::Branch {
                    condition,
                    then_body,
                    else_body,
                } => {
                    let mut exit = live;
                    exit.write(V::Cell(*condition), f);
                    I::Branch {
                        condition: *condition,
                        then_body: rewrite(then_body, exit.clone(), f, uses, scratch, needed),
                        else_body: rewrite(else_body, exit, f, uses, scratch, needed),
                    }
                }
                I::Loop { condition, body } => I::Loop {
                    condition: *condition,
                    body: rewrite(
                        body,
                        body_entry(std::slice::from_ref(i), live, f),
                        f,
                        uses,
                        scratch,
                        needed,
                    ),
                },
                _ => i.clone(),
            });
        }
        result
    }
    let rewritten = nodes
        .iter()
        .zip(exits)
        .map(|(c, exit)| {
            let body = rewrite(c.body(), exit, &f, &uses, &scratch, &mut needed);
            Continuation::new(c.id(), c.function(), body, c.terminator().clone())
                .with_source_spans(c.body_sources().to_vec(), c.terminator_source())
        })
        .collect();
    let slots = f.frame_slots() + needed;
    (f.with_frame_slots(slots), rewritten)
}

/// Add source * stride to a word whose low byte is known, consuming source.
/// There are at most eight carry thresholds. Larger strides and overlapping
/// operands keep their caller's generic lowering.
fn append_small_scaled_offset(
    source: Address,
    offset: LogicalOffset,
    stride: u16,
    initial_low: u8,
    scratch: &[Address],
    body: &mut Vec<I>,
) -> bool {
    if !(2..=8).contains(&stride) || scratch.len() < 3 {
        return false;
    }
    let addresses: Vec<_> = [source, offset.low, offset.high]
        .into_iter()
        .chain(scratch.iter().copied())
        .collect();
    if addresses
        .iter()
        .enumerate()
        .any(|(i, a)| addresses[..i].contains(a))
    {
        return false;
    }
    let maximum_carry = (u16::from(initial_low) + u16::from(u8::MAX) * stride) / 256;
    let n = usize::from(maximum_carry);
    if scratch.len() < n + 2 {
        return false;
    }
    let right = scratch[n];
    let carry = scratch[n + 1];
    let mut targets = vec![FrameTransferTarget {
        dst: offset.low,
        factor: stride as u8,
    }];
    body.extend(scratch[..n].iter().map(|&dst| I::Set { dst, value: 0 }));
    targets.extend(
        scratch[..n]
            .iter()
            .map(|&dst| FrameTransferTarget { dst, factor: 1 }),
    );
    body.push(I::Transfer {
        src: source,
        targets,
    });
    let mut checks = Vec::new();
    for k in 1..=maximum_carry {
        let threshold = (256 * k - u16::from(initial_low)).div_ceil(stride) as u8;
        checks.extend([
            I::Set {
                dst: right,
                value: threshold,
            },
            I::Compare {
                left: scratch[usize::from(k - 1)],
                right,
                dst: carry,
                true_value: 0,
                false_value: 1,
            },
            I::Branch {
                condition: carry,
                then_body: vec![I::AddConst {
                    dst: offset.high,
                    value: 1,
                }],
                else_body: vec![],
            },
        ]);
    }
    // Most compiler indices stay below the first carry threshold. Skip the
    // remaining comparisons in that case, clearing their unused snapshots.
    let remaining = checks.split_off(3);
    if let I::Branch {
        then_body,
        else_body,
        ..
    } = &mut checks[2]
    {
        then_body.extend(remaining);
        else_body.extend(scratch[1..n].iter().map(|&dst| I::Set { dst, value: 0 }));
    }
    body.extend(checks);
    true
}

/// A bounded block-local proof, with no assumptions about entry or loop state.
/// Structured bodies and aggregate writes are conservative barriers.
fn known_byte(body: &[I], address: Address) -> Option<u8> {
    let mut delta = 0u8;
    for instruction in body.iter().rev().take(64) {
        match instruction {
            I::Set { dst, value } if *dst == address => return Some(value.wrapping_add(delta)),
            I::AddConst { dst, value } if *dst == address => delta = delta.wrapping_add(*value),
            I::Loop { .. } | I::Branch { .. } | I::AggregateCopy { .. } => return None,
            _ => {
                let mut written = false;
                crate::cir::effects::instruction(instruction, |effect| {
                    written |= matches!(effect,
                        crate::cir::effects::Effect::Write(crate::ValueOperand::Cell(a))
                        | crate::cir::effects::Effect::Clobber(a) if a == address);
                });
                if written {
                    return None;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Continuation, ContinuationId, ContinuationProgram, FrameSlot, FunctionDescriptor,
        FunctionId, Terminator, ValueType,
    };

    fn slot(n: usize) -> Address {
        Address::Frame(FrameSlot::new(n))
    }

    fn frontend_loop(stride: u8) -> I {
        I::Loop {
            condition: slot(0),
            body: vec![
                I::Set {
                    dst: slot(3),
                    value: 0,
                },
                I::Transfer {
                    src: slot(1),
                    targets: vec![
                        FrameTransferTarget {
                            dst: slot(3),
                            factor: 1,
                        },
                        FrameTransferTarget {
                            dst: slot(5),
                            factor: 1,
                        },
                    ],
                },
                I::Transfer {
                    src: slot(5),
                    targets: vec![FrameTransferTarget {
                        dst: slot(1),
                        factor: 1,
                    }],
                },
                I::Set {
                    dst: slot(4),
                    value: 0u8.wrapping_sub(stride),
                },
                I::Compare {
                    left: slot(3),
                    right: slot(4),
                    dst: slot(6),
                    true_value: 0,
                    false_value: 1,
                },
                I::AddConst {
                    dst: slot(1),
                    value: stride,
                },
                I::Branch {
                    condition: slot(6),
                    then_body: vec![I::AddConst {
                        dst: slot(2),
                        value: 1,
                    }],
                    else_body: vec![],
                },
                I::AddConst {
                    dst: slot(0),
                    value: 255,
                },
            ],
        }
    }

    #[test]
    fn final_pass_reuses_private_scratch_and_keeps_observed_or_dirty_loops() {
        let main = FunctionId::new(0);
        let id = ContinuationId::new(1).unwrap();
        for stride in 2..=9u8 {
            for low in [0, 127, 255] {
                // Two blocked cases: a dirty restore, and scratch observed after
                // a zero-source execution (where the old loop leaves it alone).
                for blocked in 0..=2 {
                    let mut body = vec![
                        I::Input { dst: slot(0) },
                        I::Input { dst: slot(2) },
                        I::Set {
                            dst: slot(1),
                            value: low,
                        },
                        I::Set {
                            dst: slot(6),
                            value: 231,
                        },
                    ];
                    if blocked == 1 {
                        body.push(I::Set {
                            dst: slot(5),
                            value: 173,
                        });
                    }
                    body.push(frontend_loop(stride));
                    body.extend([1, 2, 0].map(|n| I::Output { src: slot(n) }));
                    if blocked == 2 {
                        body.push(I::Output { src: slot(6) });
                    }
                    body.push(I::Input { dst: slot(7) });
                    let f = FunctionDescriptor::new(main, vec![], 8, ValueType::Void, id);
                    let nodes = vec![Continuation::new(
                        id,
                        main,
                        vec![
                            I::Input { dst: slot(7) },
                            I::Loop {
                                condition: slot(7),
                                body,
                            },
                        ],
                        Terminator::Halt,
                    )];
                    let (optimized, rewritten) = optimize(f.clone(), nodes.clone());
                    assert_eq!(rewritten != nodes, stride <= 8 && blocked == 0);
                    if stride <= 3 && low == 0 && blocked == 0 {
                        assert_eq!(optimized.frame_slots(), 8);
                    }
                    let original = ContinuationProgram::new(main, vec![f], nodes).unwrap();
                    let candidate =
                        ContinuationProgram::new(main, vec![optimized], rewritten).unwrap();
                    let mut input = Vec::new();
                    for i in 0..=255u8 {
                        input.extend([1, i, i.wrapping_mul(97)]);
                    }
                    input.push(0);
                    let run = |p: &ContinuationProgram| {
                        let mut output = Vec::new();
                        crate::run_continuations_with_io(
                            p,
                            &mut &input[..],
                            &mut output,
                            Default::default(),
                            |_| {},
                        )
                        .unwrap();
                        output
                    };
                    assert_eq!(
                        run(&original),
                        run(&candidate),
                        "stride={stride},low={low},blocked={blocked}"
                    );
                }
            }
        }
    }

    #[test]
    fn adapter_loops_are_recognized_and_restore_parameters_are_not_fresh() {
        let main = FunctionId::new(0);
        let id = ContinuationId::new(1).unwrap();
        for stride in 2..=9 {
            let mut inner = vec![I::AddConst {
                dst: slot(0),
                value: 255,
            }];
            for _ in 0..stride {
                inner.extend([
                    I::AddConst {
                        dst: slot(1),
                        value: 1,
                    },
                    I::Copy {
                        src: slot(1),
                        dst: slot(3),
                    },
                    I::Branch {
                        condition: slot(3),
                        then_body: vec![],
                        else_body: vec![I::AddConst {
                            dst: slot(2),
                            value: 1,
                        }],
                    },
                ]);
            }
            let f = FunctionDescriptor::new(main, vec![], 7, ValueType::Void, id);
            let nodes = vec![Continuation::new(
                id,
                main,
                vec![
                    I::Set {
                        dst: slot(1),
                        value: 127,
                    },
                    I::Input { dst: slot(0) },
                    I::Input { dst: slot(2) },
                    I::Loop {
                        condition: slot(0),
                        body: inner,
                    },
                    I::Output { src: slot(1) },
                    I::Output { src: slot(2) },
                ],
                Terminator::Halt,
            )];
            let (_, rewritten) = optimize(f, nodes.clone());
            assert_eq!(rewritten != nodes, stride <= 8);
        }
        let f = FunctionDescriptor::new(main, vec![FrameSlot::new(5)], 7, ValueType::Void, id);
        let nodes = vec![Continuation::new(
            id,
            main,
            vec![
                I::Set {
                    dst: slot(1),
                    value: 0,
                },
                frontend_loop(3),
                I::Output { src: slot(1) },
                I::Output { src: slot(2) },
            ],
            Terminator::Halt,
        )];
        let (_, rewritten) = optimize(f, nodes.clone());
        assert_eq!(rewritten, nodes);
    }

    #[test]
    fn all_small_strides_low_bytes_and_sources_match_word_arithmetic() {
        let main = FunctionId::new(0);
        let id = ContinuationId::new(1).unwrap();
        for stride in 2..=8 {
            for initial_low in 0..=255u8 {
                let count = usize::from((u16::from(initial_low) + 255 * stride) / 256) + 2;
                let scratch: Vec<_> = (3..3 + count).map(slot).collect();
                let guard = slot(3 + count);
                let mut body = vec![
                    I::Input { dst: slot(0) },
                    I::Input { dst: slot(2) },
                    I::Set {
                        dst: slot(1),
                        value: initial_low,
                    },
                ];
                body.extend(scratch.iter().map(|&dst| I::Set { dst, value: 173 }));
                assert!(append_small_scaled_offset(
                    slot(0),
                    LogicalOffset::new(slot(1), slot(2)),
                    stride,
                    initial_low,
                    &scratch,
                    &mut body
                ));
                body.extend(
                    [slot(1), slot(2), slot(0)]
                        .into_iter()
                        .chain(scratch.iter().copied())
                        .map(|src| I::Output { src }),
                );
                body.push(I::Input { dst: guard });
                let program = ContinuationProgram::new(
                    main,
                    vec![FunctionDescriptor::new(
                        main,
                        vec![],
                        4 + count,
                        ValueType::Void,
                        id,
                    )],
                    vec![Continuation::new(
                        id,
                        main,
                        vec![
                            I::Input { dst: guard },
                            I::Loop {
                                condition: guard,
                                body,
                            },
                        ],
                        Terminator::Halt,
                    )],
                )
                .unwrap();
                let mut input = Vec::new();
                let mut expected = Vec::new();
                for source in 0..=255u8 {
                    let initial_high = source.wrapping_mul(37).wrapping_add(initial_low);
                    input.extend([1, source, initial_high]);
                    let word = u16::from_le_bytes([initial_low, initial_high])
                        .wrapping_add(u16::from(source) * stride);
                    expected.extend(word.to_le_bytes());
                    expected.extend(std::iter::repeat_n(0, count + 1));
                }
                input.push(0);
                let mut actual = Vec::new();
                crate::run_continuations_with_io(
                    &program,
                    &mut &input[..],
                    &mut actual,
                    Default::default(),
                    |_| {},
                )
                .unwrap();
                assert_eq!(actual, expected, "stride={stride}, low={initial_low}");
            }
        }
    }

    #[test]
    fn aliases_and_large_strides_leave_the_output_unchanged() {
        for (stride, source, offset) in [
            (1, slot(0), LogicalOffset::new(slot(1), slot(2))),
            (9, slot(0), LogicalOffset::new(slot(1), slot(2))),
            (256, slot(0), LogicalOffset::new(slot(1), slot(2))),
            (3, slot(1), LogicalOffset::new(slot(1), slot(2))),
            (3, slot(2), LogicalOffset::new(slot(1), slot(2))),
            (3, slot(0), LogicalOffset::new(slot(1), slot(1))),
            (3, slot(3), LogicalOffset::new(slot(1), slot(2))),
        ] {
            let mut body = vec![I::Output { src: slot(0) }];
            let before = body.clone();
            assert!(!append_small_scaled_offset(
                source,
                offset,
                stride,
                0,
                &[slot(3), slot(4), slot(5)],
                &mut body
            ));
            assert_eq!(body, before);
        }
    }

    #[test]
    fn constant_proof_stops_at_mutation_and_control_boundaries() {
        let prefix = vec![
            I::Set {
                dst: slot(1),
                value: 250,
            },
            I::AddConst {
                dst: slot(1),
                value: 9,
            },
            I::Input { dst: slot(0) },
        ];
        assert_eq!(known_byte(&prefix, slot(1)), Some(3));
        for instruction in [
            I::Input { dst: slot(1) },
            I::Copy {
                src: slot(0),
                dst: slot(1),
            },
            I::Transfer {
                src: slot(1),
                targets: vec![],
            },
            I::Compare {
                left: slot(1),
                right: slot(2),
                dst: slot(3),
                true_value: 1,
                false_value: 0,
            },
            I::Loop {
                condition: slot(0),
                body: vec![],
            },
            I::Branch {
                condition: slot(0),
                then_body: vec![],
                else_body: vec![],
            },
        ] {
            let mut body = prefix.clone();
            body.push(instruction);
            assert_eq!(known_byte(&body, slot(1)), None);
        }
        let mut body = prefix;
        body.extend((0..64).map(|_| I::Output { src: slot(0) }));
        assert_eq!(known_byte(&body, slot(1)), None);
    }
}
