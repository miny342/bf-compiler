//! Shared operand traversal for storage allocation and graph cloning.
//!
//! Aggregate regions are presented as an element address at offset zero;
//! mappers must preserve the element-address shape and its offset. Scalar ABI
//! values may become virtual frame slots. Globals retain their identity.

use crate::{Address, AggregateRegion, FrameInstruction, Terminator, ValueOperand};

pub(crate) fn map_region(region: &mut AggregateRegion, map: &mut impl FnMut(&mut Address)) {
    let mut address = Address::ArrayElement {
        array: *region,
        index: 0,
    };
    map(&mut address);
    let Address::ArrayElement { array, .. } = address else {
        unreachable!()
    };
    *region = array;
}
pub(crate) fn map_operand(operand: &mut ValueOperand, map: &mut impl FnMut(&mut Address)) {
    match operand {
        ValueOperand::Cell(address) => map(address),
        ValueOperand::Array(region) | ValueOperand::Aggregate { region, .. } => {
            map_region(region, map)
        }
    }
}
pub(crate) fn map_body(body: &mut [FrameInstruction], map: &mut impl FnMut(&mut Address)) {
    for instruction in body {
        match instruction {
            FrameInstruction::Set { dst, .. }
            | FrameInstruction::AddConst { dst, .. }
            | FrameInstruction::Input { dst } => map(dst),
            FrameInstruction::Copy { src, dst } => {
                map(src);
                map(dst);
            }
            FrameInstruction::Compare {
                left, right, dst, ..
            } => {
                map(left);
                map(right);
                map(dst);
            }
            FrameInstruction::SubWithBorrow {
                left,
                right,
                difference,
                borrow,
                ..
            } => {
                map(left);
                map(right);
                map(difference);
                map(borrow);
            }
            FrameInstruction::Transfer { src, targets } => {
                map(src);
                for target in targets {
                    map(&mut target.dst);
                }
            }
            FrameInstruction::AggregateCopy { src, dst, .. } => {
                map_region(src, map);
                map_region(dst, map);
            }
            FrameInstruction::Output { src } => map(src),
            FrameInstruction::Loop { condition, body } => {
                map(condition);
                map_body(body, map);
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                map(condition);
                map_body(then_body, map);
                map_body(else_body, map);
            }
        }
    }
}
pub(crate) fn map_terminator(terminator: &mut Terminator, map: &mut impl FnMut(&mut Address)) {
    match terminator {
        Terminator::Branch { condition, .. } => map(condition),
        Terminator::Call { arguments, .. } => {
            for argument in arguments {
                map_operand(argument, map);
            }
        }
        Terminator::Return { value } => {
            if let Some(value) = value {
                map_operand(value, map);
            }
        }
        Terminator::ArrayLoad {
            array,
            index,
            destination,
            ..
        } => {
            map_region(array, map);
            map(index);
            map(destination);
        }
        Terminator::ArrayStore {
            array,
            index,
            value,
            ..
        } => {
            map_region(array, map);
            map(index);
            map(value);
        }
        Terminator::AggregateLoad {
            source,
            offset,
            destination,
            ..
        } => {
            map(&mut offset.low);
            map(&mut offset.high);
            map_region(source, map);
            map_operand(destination, map);
        }
        Terminator::AggregateStore {
            destination,
            offset,
            source,
            ..
        } => {
            map(&mut offset.low);
            map(&mut offset.high);
            map_region(destination, map);
            map_operand(source, map);
        }
        Terminator::Goto { .. } | Terminator::Abort | Terminator::Halt => {}
    }
}
