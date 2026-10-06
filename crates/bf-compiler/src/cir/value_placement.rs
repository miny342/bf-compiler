//! Keep a consumed scalar's saved value at its carrier until a CFG boundary.
//!
//! For `Transfer B -> ..., T; Copy T -> B`, give the restored B a fresh
//! virtual slot and redirect later reads and writes. Frame allocation can
//! coalesce T with that slot, eliminating the restoration without changing
//! other definitions. Structured control and continuation exits restore live
//! values to canonical slots. Compare/borrow operands and control guards stay
//! pinned, and aggregate fields retain their layout.

use std::collections::BTreeSet;

use crate::cir::virtual_cleanup::{Live, body_entry, clean_function, function_exits};
use crate::{
    Address, Continuation, FrameInstruction as I, FrameSlot, FunctionDescriptor, SourceSpan,
    ValueOperand as V,
};

fn collect_pins(body: &[I], pins: &mut BTreeSet<FrameSlot>) {
    let mut pin = |a| {
        if let Address::Frame(s) = a {
            pins.insert(s);
        }
    };
    for i in body {
        match i {
            I::Compare {
                left, right, dst, ..
            } => {
                pin(*left);
                pin(*right);
                pin(*dst);
            }
            I::SubWithBorrow {
                left,
                right,
                difference,
                borrow,
                ..
            } => {
                pin(*left);
                pin(*right);
                pin(*difference);
                pin(*borrow);
            }
            I::Loop { condition, .. } | I::Branch { condition, .. } => pin(*condition),
            _ => {}
        }
    }
    for i in body {
        match i {
            I::Loop { body, .. } => collect_pins(body, pins),
            I::Branch {
                then_body,
                else_body,
                ..
            } => {
                collect_pins(then_body, pins);
                collect_pins(else_body, pins);
            }
            _ => {}
        }
    }
}

struct Versions<'a> {
    f: &'a FunctionDescriptor,
    pins: &'a BTreeSet<FrameSlot>,
    slots: &'a mut usize,
    current: Vec<FrameSlot>,
}
impl<'a> Versions<'a> {
    fn new(f: &'a FunctionDescriptor, pins: &'a BTreeSet<FrameSlot>, slots: &'a mut usize) -> Self {
        Self {
            f,
            pins,
            slots,
            current: (0..f.frame_slots()).map(FrameSlot::new).collect(),
        }
    }
    fn read(&self, a: Address) -> Address {
        match a {
            Address::Frame(s) => Address::Frame(self.current[s.index()]),
            _ => a,
        }
    }
    fn define(&mut self, a: Address) -> Address {
        match a {
            Address::Frame(s) if !self.pins.contains(&s) => {
                let version = FrameSlot::new(*self.slots);
                *self.slots += 1;
                self.current[s.index()] = version;
                Address::Frame(version)
            }
            _ => a,
        }
    }
    fn flush(
        &mut self,
        live: &Live,
        output: &mut Vec<(I, Option<SourceSpan>)>,
        source: Option<SourceSpan>,
    ) {
        // Versions are unique: changed locals always reside in fresh slots,
        // never in another canonical slot. These copies therefore cannot
        // overwrite the source of another pending boundary assignment.
        for index in 0..self.current.len() {
            let canonical = FrameSlot::new(index);
            let actual = self.current[index];
            if actual != canonical && live.observed(V::Cell(Address::Frame(canonical)), self.f) {
                output.push((
                    I::Copy {
                        src: Address::Frame(actual),
                        dst: Address::Frame(canonical),
                    },
                    source,
                ));
            }
            self.current[index] = canonical;
        }
    }
}

fn body(
    instructions: &[I],
    sources: &[Option<SourceSpan>],
    exit: Live,
    f: &FunctionDescriptor,
    pins: &BTreeSet<FrameSlot>,
    slots: &mut usize,
    boundary_source: Option<SourceSpan>,
) -> Vec<(I, Option<SourceSpan>)> {
    let mut live = exit.clone();
    let mut after = Vec::with_capacity(instructions.len());
    for i in instructions.iter().rev() {
        after.push(live.clone());
        live = body_entry(std::slice::from_ref(i), live, f);
    }
    after.reverse();
    let mut state = Versions::new(f, pins, slots);
    let mut output = Vec::new();
    for (index, i) in instructions.iter().enumerate() {
        let source = sources.get(index).copied().flatten();
        let rewritten = match i {
            I::Branch {
                condition,
                then_body,
                else_body,
            } => {
                let before = body_entry(std::slice::from_ref(i), after[index].clone(), f);
                state.flush(&before, &mut output, source);
                let mut branch_exit = after[index].clone();
                branch_exit.write(V::Cell(*condition), f);
                I::Branch {
                    condition: *condition,
                    then_body: body(
                        then_body,
                        &[],
                        branch_exit.clone(),
                        f,
                        pins,
                        state.slots,
                        source,
                    )
                    .into_iter()
                    .map(|(i, _)| i)
                    .collect(),
                    else_body: body(else_body, &[], branch_exit, f, pins, state.slots, source)
                        .into_iter()
                        .map(|(i, _)| i)
                        .collect(),
                }
            }
            I::Loop {
                condition,
                body: inner,
            } => {
                let header = body_entry(std::slice::from_ref(i), after[index].clone(), f);
                state.flush(&header, &mut output, source);
                I::Loop {
                    condition: *condition,
                    body: body(inner, &[], header, f, pins, state.slots, source)
                        .into_iter()
                        .map(|(i, _)| i)
                        .collect(),
                }
            }
            _ => {
                let mut renamed = i.clone();
                if let I::Copy {
                    src: Address::Frame(src),
                    dst: Address::Frame(dst),
                } = i
                    && !pins.contains(dst)
                    && index > 0
                    && matches!(&instructions[index - 1], I::Transfer { src: previous, targets }
                        if *previous == Address::Frame(*dst)
                            && targets.iter().any(|t| t.dst == Address::Frame(*src) && t.factor == 1))
                {
                    // B has just been consumed into T. Give its restored value
                    // a new identity, so T and this identity can share a cell.
                    renamed = I::Copy {
                        src: state.read(Address::Frame(*src)),
                        dst: state.define(Address::Frame(*dst)),
                    };
                } else {
                    crate::cir::operands::map_body(std::slice::from_mut(&mut renamed), &mut |a| {
                        *a = state.read(*a)
                    });
                }
                renamed
            }
        };
        output.push((rewritten, source));
    }
    state.flush(&exit, &mut output, boundary_source);
    output
}

pub(crate) fn finish(
    f: FunctionDescriptor,
    nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    fn remove(body: &[I]) -> Vec<(I, usize)> {
        body.iter()
            .enumerate()
            .filter_map(|(index, op)| match op {
                I::Copy { src, dst } if src == dst => None,
                I::Loop { condition, body } => Some((
                    I::Loop {
                        condition: *condition,
                        body: remove(body).into_iter().map(|(i, _)| i).collect(),
                    },
                    index,
                )),
                I::Branch {
                    condition,
                    then_body,
                    else_body,
                } => Some((
                    I::Branch {
                        condition: *condition,
                        then_body: remove(then_body).into_iter().map(|(i, _)| i).collect(),
                        else_body: remove(else_body).into_iter().map(|(i, _)| i).collect(),
                    },
                    index,
                )),
                op => Some((op.clone(), index)),
            })
            .collect()
    }
    (
        f,
        nodes
            .into_iter()
            .map(|c| {
                let ops = remove(c.body());
                let sources = ops.iter().map(|(_, i)| c.body_sources()[*i]).collect();
                Continuation::new(
                    c.id(),
                    c.function(),
                    ops.into_iter().map(|(i, _)| i).collect(),
                    c.terminator().clone(),
                )
                .with_source_spans(sources, c.terminator_source())
            })
            .collect(),
    )
}

pub(crate) fn optimize(
    f: FunctionDescriptor,
    nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    let mut pins = BTreeSet::new();
    for c in &nodes {
        collect_pins(c.body(), &mut pins);
    }
    let exits = function_exits(&f, &nodes);
    let mut slots = f.frame_slots();
    let mut result = Vec::new();
    for (c, exit) in nodes.into_iter().zip(exits) {
        let rewritten = body(
            c.body(),
            c.body_sources(),
            exit,
            &f,
            &pins,
            &mut slots,
            c.terminator_source(),
        );
        let (instructions, sources) = rewritten.into_iter().unzip();
        result.push(
            Continuation::new(c.id(), c.function(), instructions, c.terminator().clone())
                .with_source_spans(sources, c.terminator_source()),
        );
    }
    let f = f.with_frame_slots(slots);
    clean_function(&f, &result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContinuationId, ContinuationProgram, FrameTransferTarget as T, FunctionId, Terminator,
        ValueType, run_continuations_with_io,
    };
    fn slot(n: usize) -> Address {
        Address::Frame(FrameSlot::new(n))
    }
    fn id(n: u16) -> ContinuationId {
        ContinuationId::new(n).unwrap()
    }
    fn program(nodes: Vec<Continuation>) -> ContinuationProgram {
        let f = FunctionId::new(0);
        ContinuationProgram::new(
            f,
            vec![FunctionDescriptor::new(
                f,
                vec![],
                8,
                ValueType::Void,
                id(1),
            )],
            nodes,
        )
        .unwrap()
    }
    fn rewritten(p: &ContinuationProgram) -> ContinuationProgram {
        let (f, nodes) = optimize(p.functions()[0].clone(), p.continuations().to_vec());
        ContinuationProgram::new(p.main(), vec![f], nodes).unwrap()
    }
    fn run(p: &ContinuationProgram, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        run_continuations_with_io(p, &mut &input[..], &mut output, Default::default(), |_| {})
            .unwrap();
        output
    }
    fn allocated(p: &ContinuationProgram) -> ContinuationProgram {
        let (f, nodes) = crate::cir::frame_allocation::allocate(
            p.functions()[0].clone(),
            p.continuations().to_vec(),
        );
        ContinuationProgram::new(p.main(), vec![f], nodes).unwrap()
    }
    #[test]
    fn live_carrier_restore_disappears_before_output() {
        let f = FunctionId::new(0);
        let p = program(vec![Continuation::new(
            id(1),
            f,
            vec![
                I::Input { dst: slot(0) },
                I::Input { dst: slot(1) },
                I::Set {
                    dst: slot(2),
                    value: 0,
                },
                I::Transfer {
                    src: slot(0),
                    targets: vec![
                        T {
                            dst: slot(1),
                            factor: 1,
                        },
                        T {
                            dst: slot(2),
                            factor: 1,
                        },
                    ],
                },
                I::Copy {
                    src: slot(2),
                    dst: slot(0),
                },
                I::Output { src: slot(1) },
                I::Output { src: slot(0) },
            ],
            Terminator::Halt,
        )]);
        let q = rewritten(&p);
        for b in 0..=255 {
            for a in [0, 1, 127, 128, 254, 255] {
                assert_eq!(run(&p, &[b, a]), run(&q, &[b, a]));
            }
        }
        let q = allocated(&q);
        assert!(
            !q.continuations()[0]
                .body()
                .iter()
                .any(|i| matches!(i,I::Copy{src,dst} if src!=dst))
        );
        let bf0 = crate::compile_continuations(&allocated(&p)).unwrap();
        let bf1 = crate::compile_continuations(&q).unwrap();
        let before = bf_interpreter::run_with_stats(bf0.as_bytes(), &[255, 254]).unwrap();
        let after = bf_interpreter::run_with_stats(bf1.as_bytes(), &[255, 254]).unwrap();
        assert_eq!(before.output, after.output);
        assert!(after.stats.executed_rle_instructions < before.stats.executed_rle_instructions);
    }
    #[test]
    fn canonical_values_survive_cfg_edges_and_loop_back_edges() {
        let f = FunctionId::new(0);
        let p = program(vec![
            Continuation::new(
                id(1),
                f,
                vec![
                    I::Input { dst: slot(0) },
                    I::Input { dst: slot(1) },
                    I::Set {
                        dst: slot(2),
                        value: 0,
                    },
                    I::Transfer {
                        src: slot(0),
                        targets: vec![
                            T {
                                dst: slot(1),
                                factor: 1,
                            },
                            T {
                                dst: slot(2),
                                factor: 1,
                            },
                        ],
                    },
                    I::Copy {
                        src: slot(2),
                        dst: slot(0),
                    },
                    I::Set {
                        dst: slot(7),
                        value: 3,
                    },
                ],
                Terminator::Goto { target: id(2) },
            ),
            Continuation::new(
                id(2),
                f,
                vec![
                    I::Loop {
                        condition: slot(7),
                        body: vec![
                            I::Set {
                                dst: slot(4),
                                value: 0,
                            },
                            I::Transfer {
                                src: slot(0),
                                targets: vec![
                                    T {
                                        dst: slot(1),
                                        factor: 255,
                                    },
                                    T {
                                        dst: slot(4),
                                        factor: 1,
                                    },
                                ],
                            },
                            I::Copy {
                                src: slot(4),
                                dst: slot(0),
                            },
                            I::Output { src: slot(1) },
                            I::AddConst {
                                dst: slot(7),
                                value: 255,
                            },
                        ],
                    },
                    I::Output { src: slot(0) },
                ],
                Terminator::Halt,
            ),
        ]);
        let q = rewritten(&p);
        assert_eq!(
            p.continuations()
                .iter()
                .map(|c| c.terminator())
                .collect::<Vec<_>>(),
            q.continuations()
                .iter()
                .map(|c| c.terminator())
                .collect::<Vec<_>>()
        );
        for b in 0..=255 {
            assert_eq!(run(&p, &[b, 254]), run(&q, &[b, 254]));
        }
    }
    #[test]
    fn snapshots_and_destructive_aliases_match_for_randomized_regions() {
        let f = FunctionId::new(0);
        let mut seed = 0xf39c40b1u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for _ in 0..160 {
            let mut ops = (0..6)
                .map(|n| I::Input { dst: slot(n) })
                .collect::<Vec<_>>();
            for _ in 0..64 {
                let src = slot(next() % 6);
                let dst = slot(next() % 6);
                let op = match next() % 6 {
                    0 => I::Copy { src, dst },
                    1 => I::Set {
                        dst,
                        value: next() as u8,
                    },
                    2 => I::AddConst {
                        dst,
                        value: next() as u8,
                    },
                    3 => I::Transfer {
                        src,
                        targets: if src == dst {
                            vec![]
                        } else {
                            vec![T {
                                dst,
                                factor: [1, 2, 16, 128, 255][next() % 5],
                            }]
                        },
                    },
                    4 => I::Output { src },
                    _ => {
                        ops.push(I::Set {
                            dst: slot(7),
                            value: 0,
                        });
                        ops.push(I::Transfer {
                            src,
                            targets: if src == dst {
                                vec![T {
                                    dst: slot(7),
                                    factor: 1,
                                }]
                            } else {
                                vec![
                                    T { dst, factor: 255 },
                                    T {
                                        dst: slot(7),
                                        factor: 1,
                                    },
                                ]
                            },
                        });
                        I::Copy {
                            src: slot(7),
                            dst: src,
                        }
                    }
                };
                ops.push(op);
            }
            ops.push(I::Set {
                dst: slot(6),
                value: (next() % 2) as u8,
            });
            ops.push(I::Branch {
                condition: slot(6),
                then_body: vec![I::Copy {
                    src: slot(2),
                    dst: slot(4),
                }],
                else_body: vec![I::Copy {
                    src: slot(3),
                    dst: slot(5),
                }],
            });
            ops.extend((0..6).map(|n| I::Output { src: slot(n) }));
            let p = program(vec![Continuation::new(id(1), f, ops, Terminator::Halt)]);
            let q = rewritten(&p);
            let input = (0..6).map(|_| next() as u8).collect::<Vec<_>>();
            assert_eq!(run(&p, &input), run(&q, &input));
        }
    }
}
