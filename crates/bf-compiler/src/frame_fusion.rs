//! Local value tracking for arithmetic fusion, before frame-slot allocation.
//!
//! Copies retain a value's identity; writes get a new identity. This recognizes
//! comparison/subtraction of the same snapshots without names or source syntax.
//! Calls, control flow, I/O and non-local storage are barriers. Each transformed
//! region preserves every original cell, including consumed temporary operands.

use std::collections::{HashMap, HashSet};

use crate::{
    Address, AggregateRegion, Continuation, FrameInstruction as I, FrameSlot, FunctionDescriptor,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Value {
    Constant(u8),
    Snapshot(usize),
}

#[derive(Default)]
struct Values {
    cells: HashMap<Address, Value>,
    next: usize,
}

impl Values {
    fn fresh(&mut self) -> Value {
        let value = Value::Snapshot(self.next);
        self.next += 1;
        value
    }
    fn get(&mut self, address: Address) -> Value {
        if let Some(&value) = self.cells.get(&address) {
            return value;
        }
        let value = self.fresh();
        self.cells.insert(address, value);
        value
    }
    fn set(&mut self, address: Address, value: Value) {
        self.cells.insert(address, value);
    }
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

fn accesses(instruction: &I) -> Option<(Vec<Address>, Vec<Address>)> {
    let (reads, writes) = match instruction {
        I::Set { dst, .. } => (vec![], vec![*dst]),
        I::AddConst { dst, .. } => (vec![*dst], vec![*dst]),
        I::Copy { src, dst } => (vec![*src], vec![*dst]),
        I::Transfer { src, targets } => {
            let addresses = std::iter::once(*src)
                .chain(targets.iter().map(|t| t.dst))
                .collect::<Vec<_>>();
            (addresses.clone(), addresses)
        }
        I::Compare {
            left, right, dst, ..
        } => (vec![*left, *right], vec![*left, *right, *dst]),
        I::SubWithBorrow {
            left,
            right,
            difference,
            borrow,
            ..
        } => (
            vec![*left, *right],
            vec![*left, *right, *difference, *borrow],
        ),
        _ => return None,
    };
    reads
        .iter()
        .chain(&writes)
        .all(|&a| local(a))
        .then_some((reads, writes))
}

/// Fuse local arithmetic while temporary slots still have their original identity.
/// The extra slots are reusable between regions and are zero on every exit.
pub(crate) fn fuse_function(
    descriptor: FunctionDescriptor,
    continuations: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    let base = descriptor.frame_slots();
    let mut extra = 0;
    let continuations = continuations
        .into_iter()
        .map(|c| {
            let fused = fuse_body(c.body(), base, &mut extra);
            let sources = fused
                .iter()
                .map(|(_, origin)| c.body_sources()[*origin])
                .collect();
            let body = fused
                .into_iter()
                .map(|(instruction, _)| instruction)
                .collect();
            Continuation::new(c.id(), c.function(), body, c.terminator().clone())
                .with_source_spans(sources, c.terminator_source())
        })
        .collect();
    (descriptor.with_frame_slots(base + extra), continuations)
}

fn fuse_body(body: &[I], base: usize, extra: &mut usize) -> Vec<(I, usize)> {
    let mut output = Vec::new();
    let mut start = 0;
    for (index, instruction) in body.iter().enumerate() {
        if accesses(instruction).is_some() {
            continue;
        }
        output.extend(
            fuse_region(&body[start..index], base, extra)
                .into_iter()
                .map(|(i, origin)| (i, start + origin)),
        );
        let instruction = match instruction {
            I::Loop { condition, body } => I::Loop {
                condition: *condition,
                body: fuse_body(body, base, extra)
                    .into_iter()
                    .map(|(i, _)| i)
                    .collect(),
            },
            I::Branch {
                condition,
                then_body,
                else_body,
            } => I::Branch {
                condition: *condition,
                then_body: fuse_body(then_body, base, extra)
                    .into_iter()
                    .map(|(i, _)| i)
                    .collect(),
                else_body: fuse_body(else_body, base, extra)
                    .into_iter()
                    .map(|(i, _)| i)
                    .collect(),
            },
            _ => instruction.clone(),
        };
        output.push((instruction, index));
        start = index + 1;
    }
    output.extend(
        fuse_region(&body[start..], base, extra)
            .into_iter()
            .map(|(i, origin)| (i, start + origin)),
    );
    output
}

fn fuse_region(body: &[I], base: usize, extra: &mut usize) -> Vec<(I, usize)> {
    let mut values = Values::default();
    let mut compares = HashMap::new();
    let mut replacements = HashMap::new();
    let mut scratch_ends = Vec::new();
    for (index, instruction) in body.iter().enumerate() {
        match instruction {
            I::Set { dst, value } => values.set(*dst, Value::Constant(*value)),
            I::Copy { src, dst } => {
                let value = values.get(*src);
                values.set(*dst, value);
            }
            I::AddConst { dst, value: 0 } => {
                values.get(*dst);
            }
            I::Compare {
                left, right, dst, ..
            } => {
                let pair = (values.get(*left), values.get(*right));
                compares.insert(pair, index);
                values.set(*left, Value::Constant(0));
                values.set(*right, Value::Constant(0));
                let result = values.fresh();
                values.set(*dst, result);
            }
            I::Transfer { src, targets } => {
                let source = values.get(*src);
                if let [target] = targets.as_slice()
                    && target.factor == 255
                    && let Some(compare) = compares.remove(&(values.get(target.dst), source))
                {
                    let I::Compare {
                        left,
                        right,
                        dst,
                        true_value,
                        false_value,
                    } = body[compare]
                    else {
                        unreachable!()
                    };
                    // Prefer the eventual subtraction destination. Moving its
                    // write earlier is safe only if no intervening instruction
                    // reads it, and its intervening writes are pure overwrites.
                    let direct = dst != target.dst
                        && body[compare + 1..index].iter().all(|i| {
                            let (reads, writes) = accesses(i).unwrap();
                            !reads.contains(&target.dst)
                                && (!writes.contains(&target.dst)
                                    || matches!(i, I::Set { .. } | I::Copy { .. }))
                        });
                    let difference = if direct {
                        for (offset, i) in body[compare + 1..index].iter().enumerate() {
                            if accesses(i).unwrap().1.contains(&target.dst) {
                                replacements.insert(compare + 1 + offset, vec![]);
                            }
                        }
                        replacements.insert(
                            index,
                            vec![I::Set {
                                dst: *src,
                                value: 0,
                            }],
                        );
                        target.dst
                    } else {
                        let scratch = scratch_ends
                            .iter()
                            .position(|&end| end < compare)
                            .unwrap_or(scratch_ends.len());
                        if scratch == scratch_ends.len() {
                            scratch_ends.push(index);
                        } else {
                            scratch_ends[scratch] = index;
                        }
                        let difference = Address::Frame(FrameSlot::new(base + scratch));
                        replacements.insert(
                            index,
                            vec![
                                I::Set {
                                    dst: *src,
                                    value: 0,
                                },
                                I::Copy {
                                    src: difference,
                                    dst: target.dst,
                                },
                                I::Set {
                                    dst: difference,
                                    value: 0,
                                },
                            ],
                        );
                        difference
                    };
                    replacements.insert(
                        compare,
                        vec![I::SubWithBorrow {
                            left,
                            right,
                            difference,
                            borrow: dst,
                            true_value,
                            false_value,
                        }],
                    );
                }
                for target in targets {
                    let before = values.get(target.dst);
                    let result = match (source, before) {
                        (Value::Constant(a), Value::Constant(b)) => {
                            Value::Constant(b.wrapping_add(a.wrapping_mul(target.factor)))
                        }
                        (_, Value::Constant(0)) if target.factor == 1 => source,
                        (Value::Constant(0), _) => before,
                        _ => values.fresh(),
                    };
                    values.set(target.dst, result);
                }
                values.set(*src, Value::Constant(0));
            }
            _ => {
                for address in accesses(instruction).unwrap().1 {
                    let value = values.fresh();
                    values.set(address, value);
                }
            }
        }
    }
    if replacements.is_empty() {
        return body
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, i)| (i, index))
            .collect();
    }
    *extra = (*extra).max(scratch_ends.len());
    let result = body
        .iter()
        .enumerate()
        .flat_map(|(index, instruction)| {
            replacements
                .remove(&index)
                .unwrap_or_else(|| vec![instruction.clone()])
                .into_iter()
                .map(move |i| (i, index))
        })
        .collect::<Vec<_>>();
    remove_overwritten_copies(result)
}

// Only remove writes whose value is overwritten before any read. Treat every
// cell as live at region exit; no assumptions about successor blocks are needed.
fn remove_overwritten_copies(body: Vec<(I, usize)>) -> Vec<(I, usize)> {
    let mut live: HashSet<_> = body
        .iter()
        .flat_map(|(i, _)| {
            let (reads, writes) = accesses(i).unwrap();
            reads.into_iter().chain(writes)
        })
        .collect();
    let mut result = Vec::new();
    for (instruction, origin) in body.into_iter().rev() {
        if matches!(&instruction, I::Set { dst, .. } | I::Copy { dst, .. } if !live.contains(dst)) {
            continue;
        }
        let (reads, writes) = accesses(&instruction).unwrap();
        for address in writes {
            live.remove(&address);
        }
        live.extend(reads);
        result.push((instruction, origin));
    }
    result.reverse();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContinuationId, ContinuationProgram, ContinuationRunOptions, FrameTransferTarget,
        FunctionId, Terminator, ValueType, run_continuations_with_io,
    };

    #[test]
    fn local_fusion_preserves_all_cells_with_intervening_writes_and_copies() {
        let address = |i| Address::Frame(FrameSlot::new(i));
        let function = FunctionId::new(0);
        let entry = ContinuationId::new(1).unwrap();
        let mut seed = 0x54d67ba9u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut body = Vec::new();
        for case in 0..2000 {
            for index in 0..8 {
                body.push(I::Set {
                    dst: address(index),
                    value: next() as u8,
                });
            }
            body.extend([
                I::Copy {
                    src: address(0),
                    dst: address(2),
                },
                I::Copy {
                    src: address(1),
                    dst: address(3),
                },
                I::Compare {
                    left: address(2),
                    right: address(3),
                    dst: address(4),
                    true_value: 173,
                    false_value: 91,
                },
            ]);
            // Some cases retain both snapshots, others modify one, alias a
            // temporary, observe a value, or consume a copy destructively.
            for _ in 0..case % 6 {
                let dst = address((next() % 8) as usize);
                let mut src = address((next() % 8) as usize);
                if src == dst {
                    src = address(8);
                }
                body.push(match next() % 4 {
                    0 => I::Copy { src, dst },
                    1 => I::Set {
                        dst,
                        value: next() as u8,
                    },
                    2 => I::Transfer {
                        src,
                        targets: vec![FrameTransferTarget { dst, factor: 1 }],
                    },
                    _ => I::Output { src },
                });
            }
            // Alternate fresh and reused destinations to exercise both direct
            // result placement and the saved-difference fallback.
            let dst = address(if case % 2 == 0 { 5 } else { 4 });
            body.extend([
                I::Copy {
                    src: address(0),
                    dst,
                },
                I::Copy {
                    src: address(1),
                    dst: address(3),
                },
                I::Transfer {
                    src: address(3),
                    targets: vec![FrameTransferTarget { dst, factor: 255 }],
                },
            ]);
            for index in 0..9 {
                body.push(I::Output {
                    src: address(index),
                });
            }
        }
        let descriptor = FunctionDescriptor::new(function, vec![], 9, ValueType::Void, entry);
        let continuations = vec![Continuation::new(entry, function, body, Terminator::Halt)];
        let before =
            ContinuationProgram::new(function, vec![descriptor.clone()], continuations.clone())
                .unwrap();
        let (descriptor, continuations) = fuse_function(descriptor, continuations);
        assert!(
            continuations[0]
                .body()
                .iter()
                .any(|i| matches!(i, I::SubWithBorrow { .. }))
        );
        let after = ContinuationProgram::new(function, vec![descriptor], continuations).unwrap();
        let execute = |program: &ContinuationProgram| {
            let mut output = Vec::new();
            run_continuations_with_io(
                program,
                &mut &b""[..],
                &mut output,
                ContinuationRunOptions::default(),
                |_| {},
            )
            .unwrap();
            output
        };
        assert_eq!(execute(&before), execute(&after));
    }
}
