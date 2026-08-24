use crate::continuation_ir::{
    Address, Continuation, ContinuationId, ContinuationIrError, ContinuationProgram,
    FrameInstruction, FrameSlot, FrameTransferTarget, FunctionDescriptor, FunctionId, Terminator,
    ValueType,
};
use crate::{Instruction, Program};

/// Wrap the current flat, main-only cell IR in a continuation program.
///
/// `CellId` indices become slots in the main activation. Structured control
/// flow remains inside the continuation; user calls are introduced only by
/// the HIR-to-continuation lowering pass.
pub(crate) fn adapt_flat_program(
    program: &Program,
) -> Result<ContinuationProgram, ContinuationIrError> {
    let main = FunctionId::new(0);
    let entry = ContinuationId::new(1).expect("one is a valid continuation ID");
    let descriptor = FunctionDescriptor::new(
        main,
        Vec::new(),
        program.cell_count(),
        ValueType::Void,
        entry,
    );
    let continuation = Continuation::new(
        entry,
        main,
        adapt_instructions(program.instructions()),
        Terminator::Halt,
    );
    ContinuationProgram::new(main, vec![descriptor], vec![continuation])
}

fn adapt_instructions(instructions: &[Instruction]) -> Vec<FrameInstruction> {
    instructions.iter().map(adapt_instruction).collect()
}

fn adapt_instruction(instruction: &Instruction) -> FrameInstruction {
    match instruction {
        Instruction::Set { dst, value } => FrameInstruction::Set {
            dst: address(*dst),
            value: *value,
        },
        Instruction::AddConst { dst, value } => FrameInstruction::AddConst {
            dst: address(*dst),
            value: *value,
        },
        Instruction::Transfer { src, targets } => FrameInstruction::Transfer {
            src: address(*src),
            targets: targets
                .iter()
                .map(|target| FrameTransferTarget {
                    dst: address(target.dst),
                    factor: target.factor,
                })
                .collect(),
        },
        Instruction::Input { dst } => FrameInstruction::Input { dst: address(*dst) },
        Instruction::Output { src } => FrameInstruction::Output { src: address(*src) },
        Instruction::Loop { condition, body } => FrameInstruction::Loop {
            condition: address(*condition),
            body: adapt_instructions(body),
        },
        Instruction::Branch {
            condition,
            then_body,
            else_body,
        } => FrameInstruction::Branch {
            condition: address(*condition),
            then_body: adapt_instructions(then_body),
            else_body: adapt_instructions(else_body),
        },
    }
}

fn address(cell: crate::CellId) -> Address {
    Address::Frame(FrameSlot::new(cell.index()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellId, TransferTarget};

    #[test]
    fn maps_flat_cells_and_nested_control_to_main_frame_slots() {
        let source = CellId::new(0);
        let destination = CellId::new(1);
        let flat = Program::new(
            2,
            vec![Instruction::Loop {
                condition: source,
                body: vec![Instruction::Transfer {
                    src: source,
                    targets: vec![TransferTarget {
                        dst: destination,
                        factor: 1,
                    }],
                }],
            }],
        )
        .unwrap();

        let adapted = adapt_flat_program(&flat).unwrap();
        assert_eq!(adapted.functions()[0].frame_slots(), 2);
        assert_eq!(adapted.continuations().len(), 1);
        assert!(matches!(
            adapted.continuations()[0].body(),
            [FrameInstruction::Loop {
                condition: Address::Frame(slot),
                ..
            }] if *slot == FrameSlot::new(0)
        ));
        assert_eq!(adapted.continuations()[0].terminator(), &Terminator::Halt);
    }
}
