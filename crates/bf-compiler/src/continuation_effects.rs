//! Operand effects shared by liveness, virtual storage cleanup and inline costs.
//! Structured instruction bodies are visited by the caller so branches and
//! loops retain their control-flow semantics. No hard operation is a local body.

use crate::{
    Address, AggregateRegion, FrameInstruction as I, FunctionId, Terminator, ValueOperand as V,
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Effect {
    Read(V),
    Write(V),
    /// Destructive consumption of an operand, possibly followed by a write to
    /// an aliasing result. Reads are always snapshots taken before these writes.
    Clobber(Address),
    /// A runtime-selected subrange; the untouched cells remain live.
    MayWrite(AggregateRegion),
    /// Physical ABI result transport, sized by the callee descriptor. Consumers
    /// tracking only virtual frame storage can ignore this implicit ABI effect.
    CallResult(FunctionId),
}

/// Whether deleting the instruction could suppress I/O or termination behavior.
/// Storage writes are handled separately through their destination liveness.
pub(crate) fn observable(instruction: &I) -> bool {
    match instruction {
        I::Input { .. } | I::Output { .. } | I::Branch { .. } | I::Loop { .. } => true,
        I::Set { .. }
        | I::AddConst { .. }
        | I::Copy { .. }
        | I::Transfer { .. }
        | I::Compare { .. }
        | I::SubWithBorrow { .. }
        | I::AggregateCopy { .. } => false,
    }
}

pub(crate) fn instruction(instruction: &I, mut visit: impl FnMut(Effect)) {
    use Effect::*;
    let c = V::Cell;
    match instruction {
        I::Set { dst, .. } | I::Input { dst } => visit(Write(c(*dst))),
        I::AddConst { dst, .. } => {
            visit(Read(c(*dst)));
            visit(Write(c(*dst)));
        }
        I::Copy { src, dst } => {
            visit(Read(c(*src)));
            visit(Write(c(*dst)));
        }
        I::Transfer { src, targets } => {
            visit(Read(c(*src)));
            visit(Clobber(*src));
            for target in targets {
                visit(Read(c(target.dst)));
                visit(Write(c(target.dst)));
            }
        }
        I::Compare {
            left, right, dst, ..
        } => {
            visit(Read(c(*left)));
            visit(Read(c(*right)));
            visit(Clobber(*left));
            visit(Clobber(*right));
            visit(Write(c(*dst)));
        }
        I::SubWithBorrow {
            left,
            right,
            difference,
            borrow,
            ..
        } => {
            visit(Read(c(*left)));
            visit(Read(c(*right)));
            visit(Clobber(*left));
            visit(Clobber(*right));
            visit(Write(c(*difference)));
            visit(Write(c(*borrow)));
        }
        I::AggregateCopy { src, dst, cells } => {
            visit(Read(V::aggregate(*src, *cells)));
            visit(Write(V::aggregate(*dst, *cells)));
        }
        I::Output { src } => visit(Read(c(*src))),
        I::Loop { condition, .. } => visit(Read(c(*condition))),
        I::Branch { condition, .. } => {
            visit(Read(c(*condition)));
            visit(Clobber(*condition));
        }
    }
}

fn prefix(value: V, cells: usize) -> V {
    match value {
        V::Array(region) => V::aggregate(region, cells),
        V::Aggregate { region, offset, .. } => V::Aggregate {
            region,
            offset,
            cells,
        },
        V::Cell(_) => value,
    }
}

pub(crate) fn terminator(terminal: &Terminator, mut visit: impl FnMut(Effect)) {
    use Effect::*;
    let c = V::Cell;
    match terminal {
        Terminator::Goto { .. } | Terminator::Abort | Terminator::Halt => {}
        Terminator::Branch { condition, .. } => {
            visit(Read(c(*condition)));
            visit(Clobber(*condition));
        }
        Terminator::Call {
            callee, arguments, ..
        } => {
            for argument in arguments {
                visit(Read(*argument));
            }
            visit(CallResult(*callee));
        }
        Terminator::Return { value } => {
            if let Some(value) = value {
                visit(Read(*value));
            }
        }
        Terminator::ArrayLoad {
            array,
            index,
            destination,
            ..
        } => {
            visit(Read(V::Array(*array)));
            visit(Read(c(*index)));
            visit(Write(c(*destination)));
        }
        Terminator::ArrayStore {
            array,
            index,
            value,
            ..
        } => {
            visit(Read(c(*index)));
            visit(Read(c(*value)));
            visit(MayWrite(*array));
        }
        Terminator::AggregateLoad {
            source,
            offset,
            destination,
            cells,
            ..
        } => {
            visit(Read(c(offset.low)));
            visit(Read(c(offset.high)));
            visit(Read(V::Array(*source)));
            visit(Write(prefix(*destination, *cells)));
            // A multi-cell portal updates the logical offset as it transports
            // the payload. These writes also interfere with the destination.
            if *cells > 1 {
                visit(Write(c(offset.low)));
                visit(Write(c(offset.high)));
            }
        }
        Terminator::AggregateStore {
            destination,
            offset,
            source,
            cells,
            ..
        } => {
            visit(Read(c(offset.low)));
            visit(Read(c(offset.high)));
            visit(Read(prefix(*source, *cells)));
            visit(MayWrite(*destination));
            if *cells > 1 {
                visit(Write(c(offset.low)));
                visit(Write(c(offset.high)));
            }
        }
    }
}
