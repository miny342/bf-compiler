use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::continuation_ir::{
    Address, Continuation, ContinuationId, ContinuationProgram, FrameInstruction, FrameSlot,
    FrameTransferTarget, FunctionDescriptor, FunctionId, Terminator,
};
use crate::frame_layout::{AbiConfig, AbiField, FrameLayout, FrameLayoutError};
use crate::{BfInstruction, BfProgram};

/// Compile continuation IR with the default ABI configuration.
pub fn compile_continuations(program: &ContinuationProgram) -> Result<String, AbiCodegenError> {
    Ok(lower_continuations(program)?.to_source())
}

/// Lower continuation IR to BF IR with the default ABI configuration.
pub fn lower_continuations(program: &ContinuationProgram) -> Result<BfProgram, AbiCodegenError> {
    lower_continuations_with_config(program, AbiConfig::default())
}

/// Lower continuation IR using an explicitly selected ABI chunk geometry.
pub fn lower_continuations_with_config(
    program: &ContinuationProgram,
    config: AbiConfig,
) -> Result<BfProgram, AbiCodegenError> {
    let layouts = build_layouts(program, config)?;
    let mut emitter = AbiEmitter::new(program, &layouts, config);
    emitter.initialize_main()?;
    emitter.emit_dispatcher()?;
    Ok(BfProgram::new(emitter.output))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiCodegenError {
    Layout(FrameLayoutError),
    MissingFunctionLayout { function: FunctionId },
}

impl fmt::Display for AbiCodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => error.fmt(f),
            Self::MissingFunctionLayout { function } => {
                write!(f, "function {} has no ABI frame layout", function.index())
            }
        }
    }
}

impl Error for AbiCodegenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layout(error) => Some(error),
            Self::MissingFunctionLayout { .. } => None,
        }
    }
}

impl From<FrameLayoutError> for AbiCodegenError {
    fn from(error: FrameLayoutError) -> Self {
        Self::Layout(error)
    }
}

#[derive(Debug)]
struct FunctionLayout {
    frame: FrameLayout,
    branch_temporary_start: usize,
}

fn build_layouts(
    program: &ContinuationProgram,
    config: AbiConfig,
) -> Result<HashMap<FunctionId, FunctionLayout>, AbiCodegenError> {
    let mut layouts = HashMap::with_capacity(program.functions().len());
    for function in program.functions() {
        let branch_temporaries = program
            .continuations()
            .iter()
            .filter(|continuation| continuation.function() == function.id())
            .map(|continuation| maximum_branch_depth(continuation.body()))
            .max()
            .unwrap_or(0);
        let value_cells = function
            .frame_slots()
            .checked_add(branch_temporaries)
            .ok_or(FrameLayoutError::SizeOverflow)?;
        let frame = FrameLayout::new(config, value_cells, 0)?;
        layouts.insert(
            function.id(),
            FunctionLayout {
                frame,
                branch_temporary_start: function.frame_slots(),
            },
        );
    }
    Ok(layouts)
}

fn maximum_branch_depth(instructions: &[FrameInstruction]) -> usize {
    instructions
        .iter()
        .map(|instruction| match instruction {
            FrameInstruction::Loop { body, .. } => maximum_branch_depth(body),
            FrameInstruction::Branch {
                then_body,
                else_body,
                ..
            } => 1 + maximum_branch_depth(then_body).max(maximum_branch_depth(else_body)),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

struct AbiEmitter<'a> {
    program: &'a ContinuationProgram,
    layouts: &'a HashMap<FunctionId, FunctionLayout>,
    config: AbiConfig,
    output: Vec<BfInstruction>,
    /// Physical during initialization; context-base-relative in dispatcher.
    position: isize,
    branch_temporary_depth: usize,
}

impl<'a> AbiEmitter<'a> {
    fn new(
        program: &'a ContinuationProgram,
        layouts: &'a HashMap<FunctionId, FunctionLayout>,
        config: AbiConfig,
    ) -> Self {
        Self {
            program,
            layouts,
            config,
            output: Vec::new(),
            position: 0,
            branch_temporary_depth: 0,
        }
    }

    fn initialize_main(&mut self) -> Result<(), AbiCodegenError> {
        let function = self.function(self.program.main())?;
        let function_id = function.id();
        let entry = function.entry();
        let frame = self.layout(function_id)?.frame.clone();
        frame.validate_main_capacity(0)?;

        let stride = self.config.stride();
        let frame_bottom = stride;
        for chunk in 0..frame.frame_chunks() {
            self.set_raw((frame_bottom + chunk * stride) as isize, 1);
        }
        let context_base = frame_bottom + (frame.frame_chunks() - frame.context_chunks()) * stride;
        self.set_raw(
            (context_base as isize) + frame.abi_offset(AbiField::Active),
            1,
        );
        self.set_pc_raw(context_base as isize, entry);
        self.move_to(context_base as isize + frame.abi_offset(AbiField::Active));

        // Rebase bookkeeping without moving the runtime pointer.
        self.position = frame.abi_offset(AbiField::Active);
        Ok(())
    }

    fn emit_dispatcher(&mut self) -> Result<(), AbiCodegenError> {
        let body = self.capture(|emitter| {
            emitter.move_to(0);
            for continuation in emitter.program.continuations() {
                emitter.emit_dispatch_case(continuation)?;
            }
            emitter.move_abi_field(AbiField::NextPcLow, AbiField::PcLow);
            emitter.move_abi_field(AbiField::NextPcHigh, AbiField::PcHigh);
            let active = emitter.current_abi_offset(AbiField::Active)?;
            emitter.move_to(active);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        Ok(())
    }

    fn emit_dispatch_case(&mut self, continuation: &Continuation) -> Result<(), AbiCodegenError> {
        let id = continuation.id().get();
        self.copy_abi_field(AbiField::PcLow, AbiField::Condition)?;
        self.add_abi_field(AbiField::Condition, 0_u8.wrapping_sub(id as u8))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;

        self.copy_abi_field(AbiField::PcHigh, AbiField::Condition)?;
        self.add_abi_field(AbiField::Condition, 0_u8.wrapping_sub((id >> 8) as u8))?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;

        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.clear_abi_field(AbiField::PcLow)?;
            emitter.clear_abi_field(AbiField::PcHigh)?;
            emitter.branch_temporary_depth = 0;
            emitter.emit_all(continuation.body(), continuation.function())?;
            emitter.emit_terminator(continuation)?;
            let next_branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(next_branch);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
        Ok(())
    }

    fn clear_branch_on_nonzero(&mut self, condition: AbiField) -> Result<(), AbiCodegenError> {
        let condition = self.current_abi_offset(condition)?;
        self.move_to(condition);
        let body = self.capture(|emitter| {
            emitter.clear_current();
            emitter.clear_abi_field(AbiField::Branch)?;
            emitter.move_to(condition);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
        Ok(())
    }

    fn emit_all(
        &mut self,
        instructions: &[FrameInstruction],
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        for instruction in instructions {
            self.emit_instruction(instruction, function)?;
        }
        Ok(())
    }

    fn emit_instruction(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        match instruction {
            FrameInstruction::Set { dst, value } => {
                let dst = self.address_offset(*dst, function)?;
                self.set(dst, *value);
            }
            FrameInstruction::AddConst { dst, value } => {
                let dst = self.address_offset(*dst, function)?;
                self.move_to(dst);
                self.adjust(*value);
                self.move_to(0);
            }
            FrameInstruction::Transfer { src, targets } => {
                self.transfer(*src, targets, function)?;
            }
            FrameInstruction::Input { dst } => {
                let dst = self.address_offset(*dst, function)?;
                self.move_to(dst);
                self.output.push(BfInstruction::Input);
                self.move_to(0);
            }
            FrameInstruction::Output { src } => {
                let src = self.address_offset(*src, function)?;
                self.move_to(src);
                self.output.push(BfInstruction::Output);
                self.move_to(0);
            }
            FrameInstruction::Loop { condition, body } => {
                let condition = self.address_offset(*condition, function)?;
                self.move_to(condition);
                let body = self.capture(|emitter| {
                    emitter.emit_all(body, function)?;
                    emitter.move_to(condition);
                    Ok(())
                })?;
                self.output.push(BfInstruction::Loop(body));
                self.move_to(0);
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => self.emit_structured_branch(*condition, then_body, else_body, function)?,
        }
        Ok(())
    }

    fn emit_structured_branch(
        &mut self,
        condition: Address,
        then_body: &[FrameInstruction],
        else_body: &[FrameInstruction],
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let condition = self.address_offset(condition, function)?;
        let flag = self.acquire_branch_temporary(function)?;
        self.set(flag, 1);

        self.move_to(condition);
        let then_loop = self.capture(|emitter| {
            emitter.clear(condition);
            emitter.emit_all(then_body, function)?;
            emitter.clear(condition);
            emitter.clear(flag);
            emitter.move_to(condition);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(then_loop));

        self.move_to(flag);
        let else_loop = self.capture(|emitter| {
            emitter.clear(flag);
            emitter.emit_all(else_body, function)?;
            emitter.clear(condition);
            emitter.clear(flag);
            emitter.move_to(flag);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(else_loop));
        self.move_to(0);
        self.branch_temporary_depth -= 1;
        Ok(())
    }

    fn emit_terminator(&mut self, continuation: &Continuation) -> Result<(), AbiCodegenError> {
        match continuation.terminator() {
            Terminator::Goto { target } => self.set_next_pc(*target)?,
            Terminator::Branch {
                condition,
                then_target,
                else_target,
            } => {
                self.set_next_pc(*else_target)?;
                let condition = self.address_offset(*condition, continuation.function())?;
                self.move_to(condition);
                let body = self.capture(|emitter| {
                    emitter.clear_current();
                    emitter.move_to(0);
                    emitter.set_next_pc(*then_target)?;
                    emitter.move_to(condition);
                    Ok(())
                })?;
                self.output.push(BfInstruction::Loop(body));
                self.move_to(0);
            }
            Terminator::Call {
                callee,
                arguments,
                return_to,
            } => self.emit_call(continuation.function(), *callee, arguments, *return_to)?,
            Terminator::Return { value } => self.emit_return(continuation.function(), *value)?,
            Terminator::Halt => self.clear_abi_field(AbiField::Active)?,
        }
        Ok(())
    }

    fn emit_call(
        &mut self,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[Address],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let callee_function = self.function(callee)?;
        let parameters = callee_function.parameters().to_vec();
        let entry = callee_function.entry();
        let callee_frame = self.layout(callee)?.frame.clone();
        let caller_context_chunks = self.layout(caller)?.frame.context_chunks();
        let stride = self.config.stride();
        let callee_context_delta = (callee_frame.frame_chunks() * stride) as isize;
        let caller_frontier = (caller_context_chunks * stride) as isize;

        // Returned frames are all-zero, so allocation only marks their heads.
        // The first callee head is the caller's current frontier.
        for chunk in 0..callee_frame.frame_chunks() {
            self.set(caller_frontier + (chunk * stride) as isize, 1);
        }

        // Context initialization deliberately uses only the common ABI fields;
        // parameters have ordinary FrameSlot storage below this context.
        for field in AbiField::ALL {
            self.clear(callee_context_delta + callee_frame.abi_offset(field));
        }
        self.set(
            callee_context_delta + callee_frame.abi_offset(AbiField::Active),
            1,
        );
        self.set_pc_at(
            callee_context_delta,
            AbiField::NextPcLow,
            AbiField::NextPcHigh,
            entry,
        );
        self.set_pc_at(
            callee_context_delta,
            AbiField::ReturnPcLow,
            AbiField::ReturnPcHigh,
            return_to,
        );

        // Each copy restores its caller source. Repeating an Address for
        // multiple parameters therefore has the same value semantics as the
        // source language's left-to-right, already-evaluated argument list.
        let restore = self.current_abi_offset(AbiField::Restore)?;
        for (&argument, &parameter) in arguments.iter().zip(&parameters) {
            let src = self.address_offset(argument, caller)?;
            let dst = callee_context_delta + callee_frame.frame_offset(parameter);
            self.copy(src, dst, restore);
        }

        self.migrate_context(callee_context_delta);
        Ok(())
    }

    fn emit_return(
        &mut self,
        callee: FunctionId,
        value: Option<Address>,
    ) -> Result<(), AbiCodegenError> {
        let callee_frame = self.layout(callee)?.frame.clone();
        let stride = self.config.stride();
        let caller_delta = -((callee_frame.frame_chunks() * stride) as isize);
        let caller_value = caller_delta + callee_frame.abi_offset(AbiField::Value);

        if let Some(value) = value {
            let value = self.address_offset(value, callee)?;
            let callee_value = callee_frame.abi_offset(AbiField::Value);
            if value != callee_value {
                self.move_value(value, callee_value);
            }
            self.move_value(callee_value, caller_value);
        } else {
            self.clear(caller_value);
        }

        self.move_value(
            callee_frame.abi_offset(AbiField::ReturnPcLow),
            caller_delta + callee_frame.abi_offset(AbiField::NextPcLow),
        );
        self.move_value(
            callee_frame.abi_offset(AbiField::ReturnPcHigh),
            caller_delta + callee_frame.abi_offset(AbiField::NextPcHigh),
        );

        // The bottom head is below the context by every non-context chunk.
        // Clearing data as well as flags establishes the allocation invariant
        // needed by the next activation, including recursive calls.
        let frame_bottom =
            -(((callee_frame.frame_chunks() - callee_frame.context_chunks()) * stride) as isize);
        for chunk in 0..callee_frame.frame_chunks() {
            let head = frame_bottom + (chunk * stride) as isize;
            self.clear(head);
            for data in 0..self.config.chunk_cells() {
                self.clear(head + 1 + data as isize);
            }
        }

        self.migrate_context(caller_delta);
        Ok(())
    }

    fn transfer(
        &mut self,
        src: Address,
        targets: &[FrameTransferTarget],
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let src = self.address_offset(src, function)?;
        if targets.is_empty() {
            self.clear(src);
            return Ok(());
        }
        let targets = targets
            .iter()
            .map(|target| Ok((self.address_offset(target.dst, function)?, target.factor)))
            .collect::<Result<Vec<_>, AbiCodegenError>>()?;
        self.move_to(src);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            for &(dst, factor) in &targets {
                emitter.move_to(dst);
                emitter.adjust(factor);
            }
            emitter.move_to(src);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
        Ok(())
    }

    fn acquire_branch_temporary(&mut self, function: FunctionId) -> Result<isize, AbiCodegenError> {
        let layout = self.layout(function)?;
        let start = layout.branch_temporary_start;
        let frame = layout.frame.clone();
        let slot = FrameSlot::new(start + self.branch_temporary_depth);
        self.branch_temporary_depth += 1;
        Ok(frame.frame_offset(slot))
    }

    fn address_offset(
        &self,
        address: Address,
        function: FunctionId,
    ) -> Result<isize, AbiCodegenError> {
        let layout = self.layout(function)?;
        Ok(match address {
            Address::Frame(slot) => layout.frame.frame_offset(slot),
            Address::AbiValue => layout.frame.abi_offset(AbiField::Value),
        })
    }

    fn current_abi_offset(&self, field: AbiField) -> Result<isize, AbiCodegenError> {
        // Dispatch helpers use the common context geometry, so any layout is
        // sufficient. Using main also covers an empty function collection.
        Ok(self.layout(self.program.main())?.frame.abi_offset(field))
    }

    fn function(&self, id: FunctionId) -> Result<&FunctionDescriptor, AbiCodegenError> {
        self.program
            .function(id)
            .ok_or(AbiCodegenError::MissingFunctionLayout { function: id })
    }

    fn layout(&self, id: FunctionId) -> Result<&FunctionLayout, AbiCodegenError> {
        self.layouts
            .get(&id)
            .ok_or(AbiCodegenError::MissingFunctionLayout { function: id })
    }

    fn set_next_pc(&mut self, id: ContinuationId) -> Result<(), AbiCodegenError> {
        self.set_abi_field(AbiField::NextPcLow, id.get() as u8)?;
        self.set_abi_field(AbiField::NextPcHigh, (id.get() >> 8) as u8)
    }

    fn set_pc_raw(&mut self, context_base: isize, id: ContinuationId) {
        let config = self.config;
        self.set_raw(
            context_base + config.logical_offset_from_head(AbiField::PcLow.index()) as isize,
            id.get() as u8,
        );
        self.set_raw(
            context_base + config.logical_offset_from_head(AbiField::PcHigh.index()) as isize,
            (id.get() >> 8) as u8,
        );
    }

    fn set_pc_at(
        &mut self,
        context_base: isize,
        low: AbiField,
        high: AbiField,
        id: ContinuationId,
    ) {
        let low = context_base + self.config.logical_offset_from_head(low.index()) as isize;
        let high = context_base + self.config.logical_offset_from_head(high.index()) as isize;
        self.set(low, id.get() as u8);
        self.set(high, (id.get() >> 8) as u8);
    }

    fn set_abi_field(&mut self, field: AbiField, value: u8) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.set(offset, value);
        Ok(())
    }

    fn add_abi_field(&mut self, field: AbiField, value: u8) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.move_to(offset);
        self.adjust(value);
        self.move_to(0);
        Ok(())
    }

    fn clear_abi_field(&mut self, field: AbiField) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.clear(offset);
        Ok(())
    }

    fn copy_abi_field(&mut self, src: AbiField, dst: AbiField) -> Result<(), AbiCodegenError> {
        let src = self.current_abi_offset(src)?;
        let dst = self.current_abi_offset(dst)?;
        let restore = self.current_abi_offset(AbiField::Restore)?;
        self.copy(src, dst, restore);
        Ok(())
    }

    fn move_abi_field(&mut self, src: AbiField, dst: AbiField) {
        let src = self.config.logical_offset_from_head(src.index()) as isize;
        let dst = self.config.logical_offset_from_head(dst.index()) as isize;
        self.move_value(src, dst);
    }

    fn copy(&mut self, src: isize, dst: isize, restore: isize) {
        self.clear(dst);
        self.clear(restore);
        self.move_to(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(dst);
            emitter.adjust(1);
            emitter.move_to(restore);
            emitter.adjust(1);
            emitter.move_to(src);
        });
        self.output.push(BfInstruction::Loop(body));
        self.move_value(restore, src);
        self.move_to(0);
    }

    fn move_value(&mut self, src: isize, dst: isize) {
        self.clear(dst);
        self.move_to(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(dst);
            emitter.adjust(1);
            emitter.move_to(src);
        });
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
    }

    fn set(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.clear_current();
        self.adjust(value);
        self.move_to(0);
    }

    fn set_raw(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.clear_current();
        self.adjust(value);
    }

    fn clear(&mut self, offset: isize) {
        self.move_to(offset);
        self.clear_current();
        self.move_to(0);
    }

    fn clear_current(&mut self) {
        self.output
            .push(BfInstruction::Loop(vec![BfInstruction::Add(255)]));
    }

    fn adjust(&mut self, value: u8) {
        self.output.push(BfInstruction::Add(value));
    }

    fn move_to(&mut self, destination: isize) {
        self.output
            .push(BfInstruction::Move(destination - self.position));
        self.position = destination;
    }

    /// Move to another frame's context base and make it the new offset origin.
    fn migrate_context(&mut self, delta: isize) {
        self.move_to(delta);
        self.position = 0;
    }

    fn capture<T>(
        &mut self,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<Vec<BfInstruction>, AbiCodegenError> {
        let outer = std::mem::take(&mut self.output);
        emit(self)?;
        Ok(std::mem::replace(&mut self.output, outer))
    }

    fn capture_infallible(&mut self, emit: impl FnOnce(&mut Self)) -> Vec<BfInstruction> {
        let outer = std::mem::take(&mut self.output);
        emit(self);
        std::mem::replace(&mut self.output, outer)
    }
}

#[cfg(test)]
mod tests {
    use bf_interpreter::run;

    use super::*;
    use crate::continuation_adapter::adapt_flat_program;
    use crate::{CellId, Instruction, Program};

    fn execute_flat(instructions: Vec<Instruction>, cells: usize, config: AbiConfig) -> Vec<u8> {
        let flat = Program::new(cells, instructions).unwrap();
        let continuations = adapt_flat_program(&flat).unwrap();
        let bf = lower_continuations_with_config(&continuations, config)
            .unwrap()
            .to_source();
        run(bf.as_bytes(), b"").unwrap()
    }

    fn id(value: u16) -> ContinuationId {
        ContinuationId::new(value).unwrap()
    }

    fn execute_continuations(program: &ContinuationProgram, chunk_cells: usize) -> Vec<u8> {
        let source = lower_continuations_with_config(program, AbiConfig::new(chunk_cells).unwrap())
            .unwrap()
            .to_source();
        run(source.as_bytes(), b"").unwrap()
    }

    fn recursive_countdown_program() -> ContinuationProgram {
        let main = FunctionId::new(0);
        let countdown = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let countdown_entry = id(3);
        let countdown_recurse = id(4);
        let countdown_base = id(5);
        let countdown_resume = id(6);

        let functions = vec![
            FunctionDescriptor::new(main, vec![], 2, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                countdown,
                vec![FrameSlot::new(0)],
                3,
                crate::ValueType::Cell,
                countdown_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: 4,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(1)),
                        value: b'L',
                    },
                ],
                Terminator::Call {
                    callee: countdown,
                    arguments: vec![Address::Frame(FrameSlot::new(0))],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![
                    FrameInstruction::Output {
                        src: Address::AbiValue,
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(1)),
                    },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                countdown_entry,
                countdown,
                vec![FrameInstruction::Transfer {
                    src: Address::Frame(FrameSlot::new(0)),
                    targets: vec![
                        FrameTransferTarget {
                            dst: Address::Frame(FrameSlot::new(1)),
                            factor: 1,
                        },
                        FrameTransferTarget {
                            dst: Address::Frame(FrameSlot::new(2)),
                            factor: 1,
                        },
                    ],
                }],
                Terminator::Branch {
                    condition: Address::Frame(FrameSlot::new(1)),
                    then_target: countdown_recurse,
                    else_target: countdown_base,
                },
            ),
            Continuation::new(
                countdown_recurse,
                countdown,
                vec![FrameInstruction::AddConst {
                    dst: Address::Frame(FrameSlot::new(2)),
                    value: 255,
                }],
                Terminator::Call {
                    callee: countdown,
                    arguments: vec![Address::Frame(FrameSlot::new(2))],
                    return_to: countdown_resume,
                },
            ),
            Continuation::new(
                countdown_base,
                countdown,
                vec![],
                Terminator::Return {
                    value: Some(Address::Frame(FrameSlot::new(2))),
                },
            ),
            Continuation::new(
                countdown_resume,
                countdown,
                vec![FrameInstruction::AddConst {
                    dst: Address::AbiValue,
                    value: 1,
                }],
                Terminator::Return {
                    value: Some(Address::AbiValue),
                },
            ),
        ];
        ContinuationProgram::new(main, functions, continuations).unwrap()
    }

    #[test]
    fn main_only_adapter_runs_with_both_chunk_sizes() {
        let cell = CellId::new(0);
        let instructions = vec![
            Instruction::Set {
                dst: cell,
                value: b'A',
            },
            Instruction::Output { src: cell },
        ];
        for chunk_cells in [8, 16] {
            assert_eq!(
                execute_flat(
                    instructions.clone(),
                    1,
                    AbiConfig::new(chunk_cells).unwrap()
                ),
                b"A"
            );
        }
    }

    #[test]
    fn dispatcher_matches_nonzero_high_byte_ids() {
        let main = FunctionId::new(0);
        let entry = ContinuationId::new(0x0101).unwrap();
        let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, entry);
        let continuation = Continuation::new(
            entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: b'H',
                },
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(0)),
                },
            ],
            Terminator::Halt,
        );
        let program = ContinuationProgram::new(main, vec![function], vec![continuation]).unwrap();
        let source = compile_continuations(&program).unwrap();
        assert_eq!(run(source.as_bytes(), b"").unwrap(), b"H");
    }

    #[test]
    fn structured_branches_use_frame_temporaries() {
        let condition = CellId::new(0);
        let nested_condition = CellId::new(1);
        let value = CellId::new(2);
        let instructions = vec![
            Instruction::Set {
                dst: condition,
                value: 1,
            },
            Instruction::Branch {
                condition,
                then_body: vec![Instruction::Branch {
                    condition: nested_condition,
                    then_body: vec![],
                    else_body: vec![Instruction::Set {
                        dst: value,
                        value: b'B',
                    }],
                }],
                else_body: vec![],
            },
            Instruction::Output { src: value },
        ];
        assert_eq!(execute_flat(instructions, 3, AbiConfig::default()), b"B");
    }

    #[test]
    fn direct_recursion_and_caller_locals_survive_with_both_chunk_sizes() {
        let program = recursive_countdown_program();
        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), &[4, b'L']);
        }
    }

    #[test]
    fn mutual_recursion_supports_different_frame_sizes() {
        let main = FunctionId::new(0);
        let small = FunctionId::new(1);
        let large = FunctionId::new(2);
        let main_entry = id(1);
        let main_resume = id(2);
        let small_entry = id(3);
        let small_call = id(4);
        let small_base = id(5);
        let small_resume = id(6);
        let large_entry = id(7);
        let large_call = id(8);
        let large_base = id(9);
        let large_resume = id(10);

        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                small,
                vec![FrameSlot::new(0)],
                3,
                crate::ValueType::Cell,
                small_entry,
            ),
            FunctionDescriptor::new(
                large,
                vec![FrameSlot::new(0)],
                12,
                crate::ValueType::Cell,
                large_entry,
            ),
        ];

        let split_and_branch = |function, entry, then_target, else_target, condition, argument| {
            Continuation::new(
                entry,
                function,
                vec![FrameInstruction::Transfer {
                    src: Address::Frame(FrameSlot::new(0)),
                    targets: vec![
                        FrameTransferTarget {
                            dst: Address::Frame(condition),
                            factor: 1,
                        },
                        FrameTransferTarget {
                            dst: Address::Frame(argument),
                            factor: 1,
                        },
                    ],
                }],
                Terminator::Branch {
                    condition: Address::Frame(condition),
                    then_target,
                    else_target,
                },
            )
        };
        let decrement_and_call = |function, continuation, argument, callee, return_to| {
            Continuation::new(
                continuation,
                function,
                vec![FrameInstruction::AddConst {
                    dst: Address::Frame(argument),
                    value: 255,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![Address::Frame(argument)],
                    return_to,
                },
            )
        };
        let increment_and_return = |function, continuation| {
            Continuation::new(
                continuation,
                function,
                vec![FrameInstruction::AddConst {
                    dst: Address::AbiValue,
                    value: 1,
                }],
                Terminator::Return {
                    value: Some(Address::AbiValue),
                },
            )
        };

        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 5,
                }],
                Terminator::Call {
                    callee: small,
                    arguments: vec![Address::Frame(FrameSlot::new(0))],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            split_and_branch(
                small,
                small_entry,
                small_call,
                small_base,
                FrameSlot::new(1),
                FrameSlot::new(2),
            ),
            decrement_and_call(small, small_call, FrameSlot::new(2), large, small_resume),
            Continuation::new(
                small_base,
                small,
                vec![],
                Terminator::Return {
                    value: Some(Address::Frame(FrameSlot::new(2))),
                },
            ),
            increment_and_return(small, small_resume),
            split_and_branch(
                large,
                large_entry,
                large_call,
                large_base,
                FrameSlot::new(10),
                FrameSlot::new(11),
            ),
            decrement_and_call(large, large_call, FrameSlot::new(11), small, large_resume),
            Continuation::new(
                large_base,
                large,
                vec![],
                Terminator::Return {
                    value: Some(Address::Frame(FrameSlot::new(11))),
                },
            ),
            increment_and_return(large, large_resume),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), &[5]);
        }
    }

    #[test]
    fn zero_slot_void_call_clears_the_callers_stale_abi_value() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 0, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(callee, vec![], 0, crate::ValueType::Void, callee_entry),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::AbiValue,
                    value: b'X',
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Return { value: None },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), &[0]);
        }
    }

    #[test]
    fn scalar_parameter_and_return_cross_the_d16_value_chunk_boundary() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let boundary_slot = FrameSlot::new(17);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                callee,
                vec![boundary_slot],
                18,
                crate::ValueType::Cell,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: b'Q',
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![Address::Frame(FrameSlot::new(0))],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Return {
                    value: Some(Address::Frame(boundary_slot)),
                },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        assert_eq!(execute_continuations(&program, 16), b"Q");
    }

    #[test]
    fn returned_frame_data_is_zero_when_the_same_frame_is_reused() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_after_first_call = id(2);
        let main_after_second_call = id(3);
        let callee_entry = id(4);
        let callee_dirty = id(5);
        let callee_return = id(6);
        let parameter = FrameSlot::new(0);
        let high_local = FrameSlot::new(16);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                callee,
                vec![parameter],
                17,
                crate::ValueType::Cell,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 1,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![Address::Frame(FrameSlot::new(0))],
                    return_to: main_after_first_call,
                },
            ),
            Continuation::new(
                main_after_first_call,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 0,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![Address::Frame(FrameSlot::new(0))],
                    return_to: main_after_second_call,
                },
            ),
            Continuation::new(
                main_after_second_call,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Branch {
                    condition: Address::Frame(parameter),
                    then_target: callee_dirty,
                    else_target: callee_return,
                },
            ),
            Continuation::new(
                callee_dirty,
                callee,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(high_local),
                    value: b'X',
                }],
                Terminator::Goto {
                    target: callee_return,
                },
            ),
            Continuation::new(
                callee_return,
                callee,
                vec![],
                Terminator::Return {
                    value: Some(Address::Frame(high_local)),
                },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), &[0]);
        }
    }

    #[test]
    fn one_caller_address_can_be_copied_to_multiple_parameters() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let lhs = FrameSlot::new(0);
        let rhs = FrameSlot::new(1);
        let sum = FrameSlot::new(2);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                callee,
                vec![lhs, rhs],
                3,
                crate::ValueType::Cell,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 21,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![
                        Address::Frame(FrameSlot::new(0)),
                        Address::Frame(FrameSlot::new(0)),
                    ],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![
                    FrameInstruction::Transfer {
                        src: Address::Frame(lhs),
                        targets: vec![FrameTransferTarget {
                            dst: Address::Frame(sum),
                            factor: 1,
                        }],
                    },
                    FrameInstruction::Transfer {
                        src: Address::Frame(rhs),
                        targets: vec![FrameTransferTarget {
                            dst: Address::Frame(sum),
                            factor: 1,
                        }],
                    },
                ],
                Terminator::Return {
                    value: Some(Address::Frame(sum)),
                },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), &[42]);
        }
    }

    #[test]
    fn return_dispatches_to_a_continuation_with_a_nonzero_high_byte() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(0x0101);
        let callee_entry = id(2);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 0, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(callee, vec![], 0, crate::ValueType::Void, callee_entry),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![],
                Terminator::Call {
                    callee,
                    arguments: vec![],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::AbiValue,
                        value: b'H',
                    },
                    FrameInstruction::Output {
                        src: Address::AbiValue,
                    },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Return { value: None },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), b"H");
        }
    }
}
