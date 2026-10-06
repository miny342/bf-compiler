//! Bounded local affine graph reconstruction before frame allocation.
//! Values are immutable affine expressions in the inputs of a straight-line
//! region, with arithmetic modulo 256. Only values live at the region exit
//! are materialized, using destructive fan-out whenever the carrier is dead.
//! Calls, I/O, comparisons, dynamic aggregate operations, globals and control
//! boundaries end a region. No alias can cross those boundaries.

use std::collections::BTreeMap;

use crate::cir::effects;
use crate::cir::virtual_cleanup::{Live, body_entry, function_exits};
use crate::{
    Address, AggregateRegion, Continuation, FrameAggregateId, FrameInstruction as I, FrameSlot,
    FrameTransferTarget as Target, FunctionDescriptor, SourceSpan, ValueOperand as V,
};

const MAX_REGION: usize = 128;
const MAX_TERMS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Cell {
    Scalar(usize),
    Element(FrameAggregateId, usize),
}
impl Cell {
    fn from_address(a: Address) -> Option<Self> {
        match a {
            Address::Frame(s) => Some(Self::Scalar(s.index())),
            Address::ArrayElement {
                array: AggregateRegion::Frame(a),
                index,
            } => Some(Self::Element(a, index)),
            _ => None,
        }
    }
    fn address(self) -> Address {
        match self {
            Self::Scalar(s) => Address::Frame(FrameSlot::new(s)),
            Self::Element(a, index) => Address::ArrayElement {
                array: AggregateRegion::Frame(a),
                index,
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Expr {
    constant: u8,
    terms: BTreeMap<Cell, u8>,
}
impl Expr {
    fn input(c: Cell) -> Self {
        Self {
            constant: 0,
            terms: BTreeMap::from([(c, 1)]),
        }
    }
    fn add(&mut self, other: &Self, factor: u8) {
        self.constant = self
            .constant
            .wrapping_add(other.constant.wrapping_mul(factor));
        for (&cell, &coefficient) in &other.terms {
            let value = self
                .terms
                .get(&cell)
                .copied()
                .unwrap_or(0)
                .wrapping_add(coefficient.wrapping_mul(factor));
            if value == 0 {
                self.terms.remove(&cell);
            } else {
                self.terms.insert(cell, value);
            }
        }
    }
    fn keeps_input(&self, c: Cell) -> bool {
        self.terms.get(&c) == Some(&1)
    }
}

fn local(i: &I) -> bool {
    let cell = |a| Cell::from_address(a).is_some();
    match i {
        I::Set { dst, .. } | I::AddConst { dst, .. } => cell(*dst),
        I::Copy { src, dst } => cell(*src) && cell(*dst),
        I::Transfer { src, targets } => cell(*src) && targets.iter().all(|t| cell(t.dst)),
        _ => false,
    }
}

fn value(state: &BTreeMap<Cell, Expr>, c: Cell) -> Expr {
    state.get(&c).cloned().unwrap_or_else(|| Expr::input(c))
}

fn graph(body: &[I]) -> Option<BTreeMap<Cell, Expr>> {
    let mut state = BTreeMap::new();
    for i in body {
        match i {
            I::Set {
                dst,
                value: constant,
            } => {
                state.insert(
                    Cell::from_address(*dst)?,
                    Expr {
                        constant: *constant,
                        ..Expr::default()
                    },
                );
            }
            I::AddConst {
                dst,
                value: constant,
            } => {
                let dst = Cell::from_address(*dst)?;
                let mut expr = value(&state, dst);
                expr.constant = expr.constant.wrapping_add(*constant);
                state.insert(dst, expr);
            }
            I::Copy { src, dst } => {
                let dst = Cell::from_address(*dst)?;
                let expr = value(&state, Cell::from_address(*src)?);
                state.insert(dst, expr);
            }
            I::Transfer { src, targets } => {
                let src = Cell::from_address(*src)?;
                let input = value(&state, src);
                for t in targets {
                    let dst = Cell::from_address(t.dst)?;
                    let mut expr = value(&state, dst);
                    expr.add(&input, t.factor);
                    if expr.terms.len() > MAX_TERMS {
                        return None;
                    }
                    state.insert(dst, expr);
                }
                state.insert(src, Expr::default());
            }
            _ => return None,
        }
    }
    Some(state)
}

fn scratch(slots: &mut usize) -> Address {
    let a = Address::Frame(FrameSlot::new(*slots));
    *slots += 1;
    a
}

fn materialize(
    state: BTreeMap<Cell, Expr>,
    live: &Live,
    f: &FunctionDescriptor,
    slots: &mut usize,
) -> Vec<I> {
    let outputs: BTreeMap<_, _> = state
        .into_iter()
        .filter(|(c, expr)| live.observed(V::Cell(c.address()), f) && *expr != Expr::input(*c))
        .collect();
    let mut fanouts: BTreeMap<Cell, Vec<Target>> = BTreeMap::new();
    for (&dst, expr) in &outputs {
        for (&src, &factor) in &expr.terms {
            if src == dst && factor == 1 {
                continue;
            }
            fanouts.entry(src).or_default().push(Target {
                dst: dst.address(),
                factor,
            });
        }
    }
    let mut result = Vec::new();
    let mut snapshots = BTreeMap::new();
    // Any input whose cell will change needs a snapshot before initialization
    // or incoming contributions. This also resolves cycles and permutations.
    for &src in fanouts.keys() {
        if outputs.contains_key(&src) {
            let tmp = scratch(slots);
            result.push(I::Copy {
                src: src.address(),
                dst: tmp,
            });
            snapshots.insert(src, tmp);
        }
    }
    for (&dst, expr) in &outputs {
        if expr.keeps_input(dst) {
            if expr.constant != 0 {
                result.push(I::AddConst {
                    dst: dst.address(),
                    value: expr.constant,
                });
            }
        } else {
            let direct_copy = expr.constant == 0
                && expr.terms.len() == 1
                && expr
                    .terms
                    .iter()
                    .any(|(src, factor)| *factor == 1 && fanouts[src].len() == 1);
            if !direct_copy {
                result.push(I::Set {
                    dst: dst.address(),
                    value: expr.constant,
                });
            }
        }
    }
    for (src, targets) in fanouts {
        let carrier = snapshots
            .get(&src)
            .copied()
            .unwrap_or_else(|| src.address());
        let needs_preserving =
            !snapshots.contains_key(&src) && live.observed(V::Cell(src.address()), f);
        // Copy can be coalesced by the ordinary allocator. Keep it explicit
        // for a simple assignment rather than lowering it to a transfer.
        if targets.len() == 1 && targets[0].factor == 1 {
            let dst = Cell::from_address(targets[0].dst).unwrap();
            let expr = &outputs[&dst];
            if expr.terms.len() == 1 && expr.constant == 0 && !expr.keeps_input(dst) {
                result.push(I::Copy {
                    src: carrier,
                    dst: dst.address(),
                });
                continue;
            }
        }
        if needs_preserving {
            let tmp = scratch(slots);
            result.push(I::Set { dst: tmp, value: 0 });
            let mut targets = targets;
            targets.push(Target {
                dst: tmp,
                factor: 1,
            });
            result.push(I::Transfer {
                src: carrier,
                targets,
            });
            result.push(I::Copy {
                src: tmp,
                dst: carrier,
            });
        } else {
            result.push(I::Transfer {
                src: carrier,
                targets,
            });
        }
    }
    result
}

/// A rough static estimate of value-proportional loops, not a runtime claim.
/// A live-source Copy costs a save plus restore; a dead-source Copy can move.
fn cost(body: &[I], mut live: Live, f: &FunctionDescriptor) -> (usize, usize, usize) {
    let mut loops = 0;
    let mut width = 0;
    for i in body.iter().rev() {
        match i {
            I::Copy { src, dst } if src != dst => {
                loops += if live.observed(V::Cell(*src), f) {
                    2
                } else {
                    1
                };
                width += 1;
            }
            I::Transfer { targets, .. } if !targets.is_empty() => {
                loops += 1;
                width += targets.len();
                // A merged large coefficient might reduce loop count while
                // growing emitted arithmetic. Count its signed byte width.
                width += targets
                    .iter()
                    .map(|t| usize::from(t.factor.min(t.factor.wrapping_neg())))
                    .sum::<usize>();
            }
            _ => {}
        }
        let mut e = Vec::new();
        effects::instruction(i, |effect| e.push(effect));
        live.effects(&e, f);
    }
    (loops, width, body.len())
}

fn region(body: &[I], live: &Live, f: &FunctionDescriptor, slots: &mut usize) -> Vec<I> {
    if body.len() < 2 {
        return body.to_vec();
    }
    let Some(state) = graph(body) else {
        return body.to_vec();
    };
    let initial_slots = *slots;
    let candidate = materialize(state, live, f, slots);
    let before = cost(body, live.clone(), f);
    let after = cost(&candidate, live.clone(), f);
    // Avoid unconstrained fan-out and coefficient explosion. This heuristic
    // remains deliberately independent of interpreter special instructions.
    if after < before && after.1 <= before.1 + 4 && candidate.len() <= body.len() + 2 {
        candidate
    } else {
        *slots = initial_slots;
        body.to_vec()
    }
}

fn optimize_body(
    body: &[I],
    sources: &[Option<SourceSpan>],
    mut live: Live,
    f: &FunctionDescriptor,
    slots: &mut usize,
) -> Vec<(I, usize)> {
    let mut parts = Vec::new();
    let mut end = body.len();
    while end > 0 {
        let index = end - 1;
        if local(&body[index]) {
            let mut start = index;
            while start > 0
                && end - start < MAX_REGION
                && local(&body[start - 1])
                && sources.get(start - 1).copied().flatten().map(|s| s.file_id)
                    == sources.get(index).copied().flatten().map(|s| s.file_id)
            {
                start -= 1;
            }
            let original = &body[start..end];
            // Scratch values never cross a region boundary. Reuse the same
            // virtual pool, including on the flat public-CIR import path.
            let mut region_slots = f.frame_slots();
            let candidate = region(original, &live, f, &mut region_slots);
            *slots = (*slots).max(region_slots);
            // A rewritten fan-out may have several contributing source spans.
            // Attribute new instructions to the region's final instruction.
            let output = if candidate == original {
                candidate
                    .into_iter()
                    .enumerate()
                    .map(|(i, op)| (op, start + i))
                    .collect()
            } else {
                candidate.into_iter().map(|op| (op, index)).collect()
            };
            parts.push(output);
            live = body_entry(original, live, f);
            end = start;
            continue;
        }
        let rewritten = match &body[index] {
            I::Branch {
                condition,
                then_body,
                else_body,
            } => {
                let mut exit = live.clone();
                exit.write(V::Cell(*condition), f);
                I::Branch {
                    condition: *condition,
                    then_body: optimize_body(then_body, &[], exit.clone(), f, slots)
                        .into_iter()
                        .map(|(i, _)| i)
                        .collect(),
                    else_body: optimize_body(else_body, &[], exit, f, slots)
                        .into_iter()
                        .map(|(i, _)| i)
                        .collect(),
                }
            }
            I::Loop {
                condition,
                body: inner,
            } => {
                let header = body_entry(&body[index..end], live.clone(), f);
                I::Loop {
                    condition: *condition,
                    body: optimize_body(inner, &[], header, f, slots)
                        .into_iter()
                        .map(|(i, _)| i)
                        .collect(),
                }
            }
            i => i.clone(),
        };
        parts.push(vec![(rewritten, index)]);
        live = body_entry(&body[index..end], live, f);
        end = index;
    }
    parts.into_iter().rev().flatten().collect()
}

pub(crate) fn optimize(
    f: FunctionDescriptor,
    nodes: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    let exits = function_exits(&f, &nodes);
    let mut slots = f.frame_slots();
    let rewritten = nodes
        .into_iter()
        .zip(exits)
        .map(|(c, live)| {
            let result = optimize_body(c.body(), c.body_sources(), live, &f, &mut slots);
            let sources = result
                .iter()
                .map(|(_, origin)| c.body_sources()[*origin])
                .collect();
            let body = result.into_iter().map(|(op, _)| op).collect();
            Continuation::new(c.id(), c.function(), body, c.terminator().clone())
                .with_source_spans(sources, c.terminator_source())
        })
        .collect();
    (f.with_frame_slots(slots), rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContinuationId, ContinuationProgram, FrameAggregateDescriptor, FunctionId, Terminator,
        ValueType, run_continuations_with_io,
    };

    fn slot(n: usize) -> Address {
        Address::Frame(FrameSlot::new(n))
    }
    fn cell(n: usize) -> Address {
        Address::ArrayElement {
            array: AggregateRegion::Frame(FrameAggregateId::new(0)),
            index: n,
        }
    }
    fn transfer(src: Address, dst: Address, factor: u8) -> I {
        I::Transfer {
            src,
            targets: vec![Target { dst, factor }],
        }
    }

    fn program(body: Vec<I>) -> ContinuationProgram {
        let f = FunctionId::new(0);
        let entry = ContinuationId::new(1).unwrap();
        ContinuationProgram::new(
            f,
            vec![FunctionDescriptor::new_aggregates(
                f,
                vec![],
                12,
                vec![FrameAggregateDescriptor::new(FrameAggregateId::new(0), 6)],
                0,
                ValueType::Void,
                entry,
            )],
            vec![Continuation::new(entry, f, body, Terminator::Halt)],
        )
        .unwrap()
    }
    fn optimized(p: &ContinuationProgram) -> ContinuationProgram {
        let (f, nodes) = optimize(p.functions()[0].clone(), p.continuations().to_vec());
        ContinuationProgram::new(p.main(), vec![f], nodes).unwrap()
    }
    fn run(p: &ContinuationProgram, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        run_continuations_with_io(p, &mut &input[..], &mut output, Default::default(), |_| {})
            .unwrap();
        output
    }

    #[test]
    fn cancelling_arithmetic_eliminates_nonadjacent_intermediates() {
        // r = (a+b)-b, where a and b remain live; fields are first-class cells.
        let p = program(vec![
            I::Input { dst: cell(0) },
            I::Input { dst: slot(1) },
            I::Copy {
                src: cell(0),
                dst: slot(2),
            },
            I::Copy {
                src: slot(1),
                dst: slot(3),
            },
            transfer(slot(3), slot(2), 1),
            I::Set {
                dst: slot(7),
                value: 13,
            },
            I::Copy {
                src: slot(1),
                dst: cell(2),
            },
            transfer(cell(2), slot(2), 255),
            I::Output { src: slot(2) },
            I::Output { src: cell(0) },
            I::Output { src: slot(1) },
        ]);
        let q = optimized(&p);
        assert!(
            !q.continuations()[0]
                .body()
                .iter()
                .any(|i| matches!(i, I::Transfer { .. }))
        );
        for a in 0..=255 {
            for b in [0, 1, 15, 127, 128, 254, 255] {
                assert_eq!(run(&p, &[a, b]), run(&q, &[a, b]));
            }
        }
    }

    #[test]
    fn combined_live_source_fanout_is_one_transfer() {
        let p = program(vec![
            I::Input { dst: slot(0) },
            I::Input { dst: cell(0) },
            I::Input { dst: cell(1) },
            I::Copy {
                src: slot(0),
                dst: slot(1),
            },
            transfer(slot(1), cell(0), 1),
            I::Copy {
                src: slot(0),
                dst: slot(2),
            },
            transfer(slot(2), cell(1), 1),
            I::Output { src: cell(0) },
            I::Output { src: cell(1) },
            I::Output { src: slot(0) },
        ]);
        let q = optimized(&p);
        assert!(
            q.continuations()[0]
                .body()
                .iter()
                .any(|i| matches!(i, I::Transfer { targets, .. } if targets.len() >= 2))
        );
        for b in 0..=255 {
            assert_eq!(run(&p, &[b, 254, 255]), run(&q, &[b, 254, 255]));
        }
    }

    #[test]
    fn randomized_aliases_coefficients_branches_and_loops() {
        let mut seed = 0xacf12871u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        let address = |i| if i < 6 { slot(i) } else { cell(i - 6) };
        for _ in 0..192 {
            let mut inner = Vec::new();
            for _ in 0..48 {
                let src = address(next() % 12);
                let dst = address(next() % 12);
                inner.push(match next() % 4 {
                    0 => I::Set {
                        dst,
                        value: next() as u8,
                    },
                    1 => I::AddConst {
                        dst,
                        value: next() as u8,
                    },
                    2 => I::Copy { src, dst },
                    _ => I::Transfer {
                        src,
                        targets: if src == dst {
                            vec![]
                        } else {
                            vec![Target {
                                dst,
                                factor: [1, 2, 16, 127, 128, 254, 255][next() % 7],
                            }]
                        },
                    },
                });
            }
            inner.push(I::Branch {
                condition: slot(2),
                then_body: vec![
                    I::Copy {
                        src: cell(0),
                        dst: slot(2),
                    },
                    transfer(slot(2), cell(1), 255),
                ],
                else_body: vec![I::Copy {
                    src: slot(4),
                    dst: slot(5),
                }],
            });
            inner.push(I::AddConst {
                dst: slot(11),
                value: 255,
            });
            let mut body = (0..12)
                .map(|i| I::Input { dst: address(i) })
                .collect::<Vec<_>>();
            body.push(I::Set {
                dst: slot(11),
                value: 3,
            });
            body.push(I::Loop {
                condition: slot(11),
                body: inner,
            });
            body.extend((0..12).map(|i| I::Output { src: address(i) }));
            let p = program(body);
            let q = optimized(&p);
            let input: Vec<_> = (0..12).map(|_| next() as u8).collect();
            assert_eq!(run(&p, &input), run(&q, &input));
        }
    }

    #[test]
    fn cyclic_inputs_are_snapshotted_before_fanout() {
        let f = program(vec![]).functions()[0].clone();
        let mut live = Live::default();
        live.read(V::Cell(slot(0)), &f);
        live.read(V::Cell(slot(1)), &f);
        let state = BTreeMap::from([
            (Cell::Scalar(0), Expr::input(Cell::Scalar(1))),
            (Cell::Scalar(1), Expr::input(Cell::Scalar(0))),
        ]);
        let mut slots = 12;
        let ops = materialize(state, &live, &f, &mut slots);
        let mut body = vec![I::Input { dst: slot(0) }, I::Input { dst: slot(1) }];
        body.extend(ops);
        body.extend([I::Output { src: slot(0) }, I::Output { src: slot(1) }]);
        let p = program(vec![]);
        let q = ContinuationProgram::new(
            p.main(),
            vec![f.with_frame_slots(slots)],
            vec![Continuation::new(
                p.continuations()[0].id(),
                p.main(),
                body,
                Terminator::Halt,
            )],
        )
        .unwrap();
        for a in 0..=255 {
            assert_eq!(run(&q, &[a, a.wrapping_add(73)]), [a.wrapping_add(73), a]);
        }
    }

    #[test]
    fn dense_expressions_fall_back_without_allocating_scratch() {
        let mut body = (0..17)
            .map(|n| I::Input { dst: slot(n) })
            .collect::<Vec<_>>();
        body.push(I::Set {
            dst: slot(17),
            value: 0,
        });
        for n in 0..17 {
            body.push(I::Copy {
                src: slot(n),
                dst: slot(18),
            });
            body.push(transfer(slot(18), slot(17), 1));
        }
        body.push(I::Output { src: slot(17) });
        body.extend((0..17).map(|n| I::Output { src: slot(n) }));
        let template = program(vec![]);
        let p = ContinuationProgram::new(
            template.main(),
            vec![template.functions()[0].clone().with_frame_slots(19)],
            vec![Continuation::new(
                template.continuations()[0].id(),
                template.main(),
                body,
                Terminator::Halt,
            )],
        )
        .unwrap();
        let q = optimized(&p);
        assert_eq!(p, q);
        assert_eq!(run(&p, &[255; 17]), run(&q, &[255; 17]));
    }

    #[test]
    fn separated_regions_share_a_bounded_scratch_pool() {
        let mut body = Vec::new();
        for _ in 0..96 {
            body.extend([
                I::Input { dst: slot(0) },
                I::Input { dst: slot(2) },
                I::Copy {
                    src: slot(0),
                    dst: slot(1),
                },
                transfer(slot(1), slot(2), 1),
                I::Output { src: slot(2) },
                I::Output { src: slot(0) },
            ]);
        }
        let p = program(body);
        let q = optimized(&p);
        assert_eq!(
            q.functions()[0].frame_slots(),
            p.functions()[0].frame_slots() + 1
        );
        let input = (0..192).map(|n| (n * 79) as u8).collect::<Vec<_>>();
        assert_eq!(run(&p, &input), run(&q, &input));
    }
}
