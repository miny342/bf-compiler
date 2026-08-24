use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use crate::bf_optimizer::optimize_bf;
use crate::continuation_ir::{
    Address, ArrayRegion, Continuation, ContinuationId, ContinuationProgram, FrameInstruction,
    FrameSlot, FrameTransferTarget, FunctionDescriptor, FunctionId, ParameterLocation, Terminator,
    ValueOperand, ValueType,
};
use crate::frame_layout::{AbiConfig, AbiField, FrameLayout, FrameLayoutError, PROTOCOL_CELLS};
use crate::static_layout::{StaticLayout, StaticLayoutError};
use crate::{BfInstruction, BfProgram};

/// Compile continuation IR with the default ABI configuration.
pub fn compile_continuations(program: &ContinuationProgram) -> Result<String, AbiCodegenError> {
    Ok(optimize_bf(&lower_continuations(program)?).to_source())
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
    let static_layout = StaticLayout::new(config, program.globals())?;
    let portal = PortalPlan::new(program)?;
    let mut emitter = AbiEmitter::new(program, &layouts, &static_layout, &portal, config);
    emitter.initialize_main()?;
    emitter.emit_dispatcher()?;
    Ok(BfProgram::new(emitter.output))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiCodegenError {
    Layout(FrameLayoutError),
    StaticLayout(StaticLayoutError),
    MissingFunctionLayout { function: FunctionId },
    ContinuationIdsExhausted,
}

impl fmt::Display for AbiCodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => error.fmt(f),
            Self::StaticLayout(error) => error.fmt(f),
            Self::MissingFunctionLayout { function } => {
                write!(f, "function {} has no ABI frame layout", function.index())
            }
            Self::ContinuationIdsExhausted => {
                write!(f, "array portals exhaust the 16-bit continuation ID space")
            }
        }
    }
}

impl Error for AbiCodegenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layout(error) => Some(error),
            Self::StaticLayout(error) => Some(error),
            Self::MissingFunctionLayout { .. } | Self::ContinuationIdsExhausted => None,
        }
    }
}

impl From<FrameLayoutError> for AbiCodegenError {
    fn from(error: FrameLayoutError) -> Self {
        Self::Layout(error)
    }
}

impl From<StaticLayoutError> for AbiCodegenError {
    fn from(error: StaticLayoutError) -> Self {
        Self::StaticLayout(error)
    }
}

#[derive(Debug)]
struct FunctionLayout {
    frame: FrameLayout,
    branch_temporary_start: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    /// Offset from the current function's dispatch-context base.
    Relative(isize),
    /// Absolute position in the static prefix.
    Global(usize),
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
        let frame = FrameLayout::with_arrays(
            config,
            value_cells,
            function.frame_arrays(),
            function.outbox_cells(),
        )?;
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

#[derive(Debug, Clone, Copy)]
enum PortalOperation {
    Load {
        array: ArrayRegion,
        destination: Address,
        return_to: ContinuationId,
    },
    Store {
        array: ArrayRegion,
        return_to: ContinuationId,
    },
}

#[derive(Debug, Clone, Copy)]
struct PortalSite {
    resume: ContinuationId,
    function: FunctionId,
    operation: PortalOperation,
}

#[derive(Debug)]
struct PortalPlan {
    load: Option<ContinuationId>,
    store: Option<ContinuationId>,
    sites: HashMap<ContinuationId, PortalSite>,
    ordered_sites: Vec<PortalSite>,
    maximum_length: usize,
}

impl PortalPlan {
    fn new(program: &ContinuationProgram) -> Result<Self, AbiCodegenError> {
        let mut used = program
            .continuations()
            .iter()
            .map(|continuation| continuation.id().get())
            .collect::<HashSet<_>>();
        let has_load = program
            .continuations()
            .iter()
            .any(|continuation| matches!(continuation.terminator(), Terminator::ArrayLoad { .. }));
        let has_store = program
            .continuations()
            .iter()
            .any(|continuation| matches!(continuation.terminator(), Terminator::ArrayStore { .. }));
        let load = has_load
            .then(|| allocate_hidden_id(&mut used))
            .transpose()?;
        let store = has_store
            .then(|| allocate_hidden_id(&mut used))
            .transpose()?;

        let mut sites = HashMap::new();
        let mut ordered_sites = Vec::new();
        let mut maximum_length = 0;
        for continuation in program.continuations() {
            let operation = match *continuation.terminator() {
                Terminator::ArrayLoad {
                    array,
                    destination,
                    return_to,
                    ..
                } => PortalOperation::Load {
                    array,
                    destination,
                    return_to,
                },
                Terminator::ArrayStore {
                    array, return_to, ..
                } => PortalOperation::Store { array, return_to },
                _ => continue,
            };
            let resume = allocate_hidden_id(&mut used)?;
            let site = PortalSite {
                resume,
                function: continuation.function(),
                operation,
            };
            sites.insert(continuation.id(), site);
            ordered_sites.push(site);
            let array = match operation {
                PortalOperation::Load { array, .. } | PortalOperation::Store { array, .. } => array,
            };
            maximum_length =
                maximum_length.max(array_cells(program, continuation.function(), array));
        }

        Ok(Self {
            load,
            store,
            sites,
            ordered_sites,
            maximum_length,
        })
    }
}

fn allocate_hidden_id(used: &mut HashSet<u16>) -> Result<ContinuationId, AbiCodegenError> {
    for value in 1..=u16::MAX {
        if used.insert(value) {
            return Ok(ContinuationId::new(value).expect("nonzero continuation ID"));
        }
    }
    Err(AbiCodegenError::ContinuationIdsExhausted)
}

fn array_cells(program: &ContinuationProgram, function: FunctionId, array: ArrayRegion) -> usize {
    match array {
        ArrayRegion::Frame(array) => program
            .function(function)
            .and_then(|function| function.frame_array(array))
            .map_or(0, |array| array.cells()),
        ArrayRegion::Global(global) => {
            match program.global(global).map(|global| global.value_type()) {
                Some(ValueType::Array(cells)) => cells,
                _ => 0,
            }
        }
        ArrayRegion::Outbox => program
            .function(function)
            .map_or(0, FunctionDescriptor::outbox_cells),
    }
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
    static_layout: &'a StaticLayout,
    portal: &'a PortalPlan,
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
        static_layout: &'a StaticLayout,
        portal: &'a PortalPlan,
        config: AbiConfig,
    ) -> Self {
        Self {
            program,
            layouts,
            static_layout,
            portal,
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
        frame.validate_main_capacity(self.static_layout.anchor_head())?;

        let stride = self.config.stride();
        let frame_bottom = self.static_layout.anchor_head() + stride;
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
            if let Some(load) = emitter.portal.load {
                emitter.emit_hidden_dispatch_case(load, |emitter| {
                    emitter.emit_array_load_accessor()?;
                    emitter.move_abi_field(AbiField::ReturnPcLow, AbiField::NextPcLow);
                    emitter.move_abi_field(AbiField::ReturnPcHigh, AbiField::NextPcHigh);
                    Ok(())
                })?;
            }
            if let Some(store) = emitter.portal.store {
                emitter.emit_hidden_dispatch_case(store, |emitter| {
                    emitter.emit_array_store_accessor()?;
                    emitter.move_abi_field(AbiField::ReturnPcLow, AbiField::NextPcLow);
                    emitter.move_abi_field(AbiField::ReturnPcHigh, AbiField::NextPcHigh);
                    Ok(())
                })?;
            }
            let sites = emitter.portal.ordered_sites.clone();
            for site in sites {
                emitter.emit_hidden_dispatch_case(site.resume, |emitter| {
                    emitter.emit_portal_resume(site)
                })?;
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
        self.emit_hidden_dispatch_case(continuation.id(), |emitter| {
            emitter.branch_temporary_depth = 0;
            emitter.emit_all(continuation.body(), continuation.function())?;
            emitter.emit_terminator(continuation)
        })
    }

    fn emit_hidden_dispatch_case(
        &mut self,
        continuation: ContinuationId,
        body_emitter: impl FnOnce(&mut Self) -> Result<(), AbiCodegenError>,
    ) -> Result<(), AbiCodegenError> {
        let id = continuation.get();
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
            body_emitter(emitter)?;
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
                let dst = self.address_location(*dst, function)?;
                self.set_location(dst, *value);
            }
            FrameInstruction::AddConst { dst, value } => {
                let dst = self.address_location(*dst, function)?;
                self.move_context_to_location(dst);
                self.adjust(*value);
                self.move_location_to_context(dst);
            }
            FrameInstruction::Transfer { src, targets } => {
                self.transfer(*src, targets, function)?;
            }
            FrameInstruction::AggregateCopy { src, dst, cells } => {
                self.aggregate_copy(*src, *dst, *cells, function)?;
            }
            FrameInstruction::Input { dst } => {
                let dst = self.address_location(*dst, function)?;
                self.move_context_to_location(dst);
                self.output.push(BfInstruction::Input);
                self.move_location_to_context(dst);
            }
            FrameInstruction::Output { src } => {
                let src = self.address_location(*src, function)?;
                self.move_context_to_location(src);
                self.output.push(BfInstruction::Output);
                self.move_location_to_context(src);
            }
            FrameInstruction::Loop { condition, body } => {
                let condition = self.address_location(*condition, function)?;
                self.move_context_to_location(condition);
                let body = self.capture(|emitter| {
                    emitter.move_location_to_context(condition);
                    emitter.emit_all(body, function)?;
                    emitter.move_context_to_location(condition);
                    Ok(())
                })?;
                self.output.push(BfInstruction::Loop(body));
                self.move_location_to_context(condition);
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
        let condition = self.address_location(condition, function)?;
        let flag = Location::Relative(self.acquire_branch_temporary(function)?);
        self.set_location(flag, 1);

        self.move_context_to_location(condition);
        let then_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(condition);
            emitter.emit_all(then_body, function)?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.move_context_to_location(condition);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(then_loop));
        self.move_location_to_context(condition);

        self.move_context_to_location(flag);
        let else_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(flag);
            emitter.emit_all(else_body, function)?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.move_context_to_location(flag);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(else_loop));
        self.move_location_to_context(flag);
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
                let condition = self.address_location(*condition, continuation.function())?;
                self.move_context_to_location(condition);
                let body = self.capture(|emitter| {
                    emitter.clear_current();
                    emitter.move_location_to_context(condition);
                    emitter.set_next_pc(*then_target)?;
                    emitter.move_context_to_location(condition);
                    Ok(())
                })?;
                self.output.push(BfInstruction::Loop(body));
                self.move_location_to_context(condition);
            }
            Terminator::Call {
                callee,
                arguments,
                return_to,
            } => self.emit_call(continuation.function(), *callee, arguments, *return_to)?,
            Terminator::Return { value } => self.emit_return(continuation.function(), *value)?,
            Terminator::ArrayLoad {
                array,
                index,
                destination: _,
                return_to: _,
            } => self.emit_array_call(continuation, *array, *index, None)?,
            Terminator::ArrayStore {
                array,
                index,
                value,
                return_to: _,
            } => self.emit_array_call(continuation, *array, *index, Some(*value))?,
            Terminator::Halt => self.clear_abi_field(AbiField::Active)?,
        }
        Ok(())
    }

    fn emit_call(
        &mut self,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let callee_function = self.function(callee)?;
        let parameters = callee_function
            .parameter_locations()
            .iter()
            .map(|parameter| {
                let cells = match parameter {
                    ParameterLocation::Cell(_) => None,
                    ParameterLocation::Array(array) => Some(
                        callee_function
                            .frame_array(*array)
                            .expect("validated aggregate parameter")
                            .cells(),
                    ),
                };
                (*parameter, cells)
            })
            .collect::<Vec<_>>();
        let entry = callee_function.entry();
        let callee_frame = self.layout(callee)?.frame.clone();
        let caller_context_chunks = self.layout(caller)?.frame.context_chunks();
        let stride = self.config.stride();
        let callee_context_delta = (callee_frame.frame_chunks() * stride) as isize;
        let caller_frontier = (caller_context_chunks * stride) as isize;

        // Each copy restores its caller source. Repeating an Address for
        // multiple parameters therefore has the same value semantics as the
        // source language's left-to-right, already-evaluated argument list.
        // Copy before marking callee heads: while a global source is visited,
        // anchor-to-frontier normalization must still return to the caller.
        // Returned frame data is zero, so writing the future parameter region
        // before allocation is safe.
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        for (&argument, &(parameter, parameter_cells)) in arguments.iter().zip(&parameters) {
            match (argument, parameter) {
                (ValueOperand::Cell(argument), ParameterLocation::Cell(parameter)) => {
                    let src = self.address_location(argument, caller)?;
                    let dst = Location::Relative(
                        callee_context_delta + callee_frame.frame_offset(parameter),
                    );
                    self.copy_locations(src, dst, restore);
                }
                (ValueOperand::Array(argument), ParameterLocation::Array(parameter)) => {
                    let cells = parameter_cells.expect("aggregate parameter size");
                    for index in 0..cells {
                        let src = self.array_element_location(argument, index, caller)?;
                        let dst = Location::Relative(
                            callee_context_delta
                                + callee_frame.array_element_offset(parameter, index)?,
                        );
                        self.copy_locations(src, dst, restore);
                    }
                }
                _ => unreachable!("validated call operand and parameter types must match"),
            }
        }

        // Returned frames are all-zero, so allocation only marks their heads.
        // The first callee head is the caller's current frontier.
        for chunk in 0..callee_frame.frame_chunks() {
            self.set(caller_frontier + (chunk * stride) as isize, 1);
        }

        // Context initialization deliberately uses only the common ABI fields;
        // parameters have ordinary scalar/array storage below this context.
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

        self.migrate_context(callee_context_delta);
        Ok(())
    }

    fn emit_return(
        &mut self,
        callee: FunctionId,
        value: Option<ValueOperand>,
    ) -> Result<(), AbiCodegenError> {
        let callee_frame = self.layout(callee)?.frame.clone();
        let stride = self.config.stride();
        let caller_delta = -((callee_frame.frame_chunks() * stride) as isize);
        let caller_value =
            Location::Relative(caller_delta + callee_frame.abi_offset(AbiField::Value));
        let restore = Location::Relative(callee_frame.abi_offset(AbiField::Restore));

        match value {
            Some(ValueOperand::Cell(value)) => {
                let value = self.address_location(value, callee)?;
                let callee_value = Location::Relative(callee_frame.abi_offset(AbiField::Value));
                self.copy_locations(value, callee_value, restore);
                self.move_location(callee_value, caller_value);
            }
            Some(ValueOperand::Array(array)) => {
                let ValueType::Array(cells) = self.function(callee)?.return_type() else {
                    unreachable!("validated aggregate return type")
                };
                for index in 0..cells {
                    let src = self.array_element_location(array, index, callee)?;
                    let dst = Location::Relative(caller_delta + self.common_outbox_offset(index));
                    self.copy_locations(src, dst, restore);
                }
                self.clear_location(caller_value);
            }
            None => self.clear_location(caller_value),
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
        let src = self.address_location(src, function)?;
        if targets.is_empty() {
            self.clear_location(src);
            return Ok(());
        }
        let targets = targets
            .iter()
            .map(|target| Ok((self.address_location(target.dst, function)?, target.factor)))
            .collect::<Result<Vec<_>, AbiCodegenError>>()?;
        self.move_context_to_location(src);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            for &(dst, factor) in &targets {
                emitter.move_context_to_location(dst);
                emitter.adjust(factor);
                emitter.move_location_to_context(dst);
            }
            emitter.move_context_to_location(src);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_location_to_context(src);
        Ok(())
    }

    fn aggregate_copy(
        &mut self,
        src: ArrayRegion,
        dst: ArrayRegion,
        cells: usize,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        if src == dst {
            return Ok(());
        }
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        for index in 0..cells {
            let src = self.array_element_location(src, index, function)?;
            let dst = self.array_element_location(dst, index, function)?;
            self.copy_locations(src, dst, restore);
        }
        Ok(())
    }

    fn emit_array_call(
        &mut self,
        continuation: &Continuation,
        array: ArrayRegion,
        index: Address,
        value: Option<Address>,
    ) -> Result<(), AbiCodegenError> {
        let site = *self
            .portal
            .sites
            .get(&continuation.id())
            .expect("every validated portal terminator has a portal site");
        let accessor = if value.is_some() {
            self.portal.store.expect("store accessor ID")
        } else {
            self.portal.load.expect("load accessor ID")
        };
        let function = continuation.function();
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);

        for field in AbiField::ALL {
            self.clear_location(self.portal_field_location(array, field, function)?);
        }
        let index_source = self.address_location(index, function)?;
        let index_port = self.portal_field_location(array, AbiField::Index, function)?;
        self.copy_locations(index_source, index_port, restore);
        if let Some(value) = value {
            let value_source = self.address_location(value, function)?;
            let value_port = self.portal_field_location(array, AbiField::Value, function)?;
            self.copy_locations(value_source, value_port, restore);
        }
        self.set_location(
            self.portal_field_location(array, AbiField::Active, function)?,
            1,
        );
        self.set_pc_locations(
            array,
            function,
            AbiField::NextPcLow,
            AbiField::NextPcHigh,
            accessor,
        )?;
        self.set_pc_locations(
            array,
            function,
            AbiField::ReturnPcLow,
            AbiField::ReturnPcHigh,
            site.resume,
        )?;
        self.enter_portal(array, function)
    }

    fn emit_array_load_accessor(&mut self) -> Result<(), AbiCodegenError> {
        self.clear_abi_field(AbiField::Value)?;
        self.compute_array_index_divmod()?;
        let chunks = self
            .portal
            .maximum_length
            .div_ceil(self.config.chunk_cells());
        for chunk in 0..chunks {
            self.if_abi_field_equals(AbiField::Scratch0, chunk as u8, |emitter| {
                let start = chunk * emitter.config.chunk_cells();
                let length =
                    (emitter.portal.maximum_length - start).min(emitter.config.chunk_cells());
                for within in 0..length {
                    emitter.if_abi_field_equals(AbiField::Scratch1, within as u8, |emitter| {
                        let source = emitter
                            .config
                            .logical_offset_from_head(PROTOCOL_CELLS + start + within)
                            as isize;
                        let destination = emitter.current_abi_offset(AbiField::Value)?;
                        let restore = emitter.current_abi_offset(AbiField::Restore)?;
                        emitter.copy(source, destination, restore);
                        Ok(())
                    })?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    fn emit_array_store_accessor(&mut self) -> Result<(), AbiCodegenError> {
        self.compute_array_index_divmod()?;
        let chunks = self
            .portal
            .maximum_length
            .div_ceil(self.config.chunk_cells());
        for chunk in 0..chunks {
            self.if_abi_field_equals(AbiField::Scratch0, chunk as u8, |emitter| {
                let start = chunk * emitter.config.chunk_cells();
                let length =
                    (emitter.portal.maximum_length - start).min(emitter.config.chunk_cells());
                for within in 0..length {
                    emitter.if_abi_field_equals(AbiField::Scratch1, within as u8, |emitter| {
                        let destination = emitter
                            .config
                            .logical_offset_from_head(PROTOCOL_CELLS + start + within)
                            as isize;
                        let source = emitter.current_abi_offset(AbiField::Value)?;
                        emitter.move_value(source, destination);
                        Ok(())
                    })?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    fn compute_array_index_divmod(&mut self) -> Result<(), AbiCodegenError> {
        for field in [
            AbiField::Scratch0,
            AbiField::Scratch1,
            AbiField::Scratch2,
            AbiField::Scratch3,
        ] {
            self.clear_abi_field(field)?;
        }
        self.set_abi_field(AbiField::Scratch2, self.config.chunk_cells() as u8)?;
        let index = self.current_abi_offset(AbiField::Index)?;
        self.move_to(index);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.add_abi_field(AbiField::Scratch1, 1)?;
            emitter.add_abi_field(AbiField::Scratch2, u8::MAX)?;
            emitter.copy_abi_field(AbiField::Scratch2, AbiField::Condition)?;
            emitter.set_abi_field(AbiField::Scratch3, 1)?;
            emitter.clear_field_on_nonzero(AbiField::Condition, AbiField::Scratch3)?;

            let zero = emitter.current_abi_offset(AbiField::Scratch3)?;
            emitter.move_to(zero);
            let zero_body = emitter.capture(|emitter| {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.add_abi_field(AbiField::Scratch0, 1)?;
                emitter.clear_abi_field(AbiField::Scratch1)?;
                emitter.set_abi_field(AbiField::Scratch2, emitter.config.chunk_cells() as u8)?;
                emitter.move_to(zero);
                Ok(())
            })?;
            emitter.output.push(BfInstruction::Loop(zero_body));
            emitter.move_to(index);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
        Ok(())
    }

    fn if_abi_field_equals(
        &mut self,
        field: AbiField,
        value: u8,
        body_emitter: impl FnOnce(&mut Self) -> Result<(), AbiCodegenError>,
    ) -> Result<(), AbiCodegenError> {
        self.copy_abi_field(field, AbiField::Condition)?;
        self.add_abi_field(AbiField::Condition, 0_u8.wrapping_sub(value))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            body_emitter(emitter)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
        Ok(())
    }

    fn clear_field_on_nonzero(
        &mut self,
        condition: AbiField,
        field: AbiField,
    ) -> Result<(), AbiCodegenError> {
        let condition = self.current_abi_offset(condition)?;
        self.move_to(condition);
        let body = self.capture(|emitter| {
            emitter.clear_current();
            emitter.clear_abi_field(field)?;
            emitter.move_to(condition);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
        Ok(())
    }

    fn emit_portal_resume(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        let array = match site.operation {
            PortalOperation::Load { array, .. } | PortalOperation::Store { array, .. } => array,
        };
        let is_load = matches!(site.operation, PortalOperation::Load { .. });
        for field in AbiField::ALL {
            if is_load && field == AbiField::Value {
                continue;
            }
            self.clear_abi_field(field)?;
        }

        let frame = self.layout(site.function)?.frame.clone();
        match array {
            ArrayRegion::Frame(array) => {
                let context_delta = -frame.array_base_offset(array)?;
                if is_load {
                    let source = self.current_abi_offset(AbiField::Value)?;
                    let destination = context_delta + frame.abi_offset(AbiField::Value);
                    self.move_value(source, destination);
                }
                self.clear_abi_field(AbiField::Value)?;
                self.migrate_context(context_delta);
            }
            ArrayRegion::Global(global) => {
                let base = self.static_layout.array_base_head(global)?;
                if is_load {
                    self.move_global_portal_value_to_context(
                        base,
                        frame.abi_offset(AbiField::Value),
                    )?;
                } else {
                    self.clear_abi_field(AbiField::Value)?;
                    self.emit_global_to_context(base, frame.context_chunks());
                }
            }
            ArrayRegion::Outbox => unreachable!("outbox cannot use the array portal"),
        }

        if let PortalOperation::Load { destination, .. } = site.operation {
            let source = Location::Relative(frame.abi_offset(AbiField::Value));
            let destination = self.address_location(destination, site.function)?;
            self.move_location(source, destination);
        }
        let return_to = match site.operation {
            PortalOperation::Load { return_to, .. } | PortalOperation::Store { return_to, .. } => {
                return_to
            }
        };
        self.set_next_pc(return_to)
    }

    fn move_global_portal_value_to_context(
        &mut self,
        base: usize,
        destination: isize,
    ) -> Result<(), AbiCodegenError> {
        let context_chunks = self.config.portal_chunks();
        // The pointer currently uses the global portal base as its origin.
        self.emit_global_to_context(base, context_chunks);
        self.clear(destination);
        self.emit_context_to_global(base, context_chunks);

        let value = self.current_abi_offset(AbiField::Value)?;
        self.move_to(value);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.emit_global_to_context(base, context_chunks);
            emitter.move_to(destination);
            emitter.adjust(1);
            emitter.move_to(0);
            emitter.emit_context_to_global(base, context_chunks);
            emitter.move_to(value);
            Ok(())
        })?;
        self.output.push(BfInstruction::Loop(body));
        self.move_to(0);
        self.emit_global_to_context(base, context_chunks);
        Ok(())
    }

    fn set_pc_locations(
        &mut self,
        array: ArrayRegion,
        function: FunctionId,
        low: AbiField,
        high: AbiField,
        value: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        self.set_location(
            self.portal_field_location(array, low, function)?,
            value.get() as u8,
        );
        self.set_location(
            self.portal_field_location(array, high, function)?,
            (value.get() >> 8) as u8,
        );
        Ok(())
    }

    fn enter_portal(
        &mut self,
        array: ArrayRegion,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        match self.portal_base_location(array, function)? {
            Location::Relative(offset) => self.migrate_context(offset),
            Location::Global(position) => {
                let context_chunks = self.layout(function)?.frame.context_chunks();
                self.emit_context_to_global(position, context_chunks);
            }
        }
        Ok(())
    }

    fn portal_base_location(
        &self,
        array: ArrayRegion,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            ArrayRegion::Frame(array) => {
                Location::Relative(self.layout(function)?.frame.array_base_offset(array)?)
            }
            ArrayRegion::Global(global) => {
                Location::Global(self.static_layout.array_base_head(global)?)
            }
            ArrayRegion::Outbox => unreachable!("validated portal array cannot be outbox"),
        })
    }

    fn portal_field_location(
        &self,
        array: ArrayRegion,
        field: AbiField,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            ArrayRegion::Frame(array) => Location::Relative(
                self.layout(function)?
                    .frame
                    .array_portal_offset(array, field)?,
            ),
            ArrayRegion::Global(global) => Location::Global(
                self.static_layout
                    .array_portal_field_position(global, field)?,
            ),
            ArrayRegion::Outbox => unreachable!("validated portal array cannot be outbox"),
        })
    }

    fn array_element_location(
        &self,
        array: ArrayRegion,
        index: usize,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            ArrayRegion::Frame(array) => Location::Relative(
                self.layout(function)?
                    .frame
                    .array_element_offset(array, index)?,
            ),
            ArrayRegion::Global(global) => {
                Location::Global(self.static_layout.array_element_position(global, index)?)
            }
            ArrayRegion::Outbox => {
                Location::Relative(self.layout(function)?.frame.outbox_offset(index)?)
            }
        })
    }

    fn common_outbox_offset(&self, index: usize) -> isize {
        let chunk = index / self.config.chunk_cells();
        let within = index % self.config.chunk_cells();
        -((chunk + 1) as isize * self.config.stride() as isize) + 1 + within as isize
    }

    fn acquire_branch_temporary(&mut self, function: FunctionId) -> Result<isize, AbiCodegenError> {
        let layout = self.layout(function)?;
        let start = layout.branch_temporary_start;
        let frame = layout.frame.clone();
        let slot = FrameSlot::new(start + self.branch_temporary_depth);
        self.branch_temporary_depth += 1;
        Ok(frame.frame_offset(slot))
    }

    fn address_location(
        &self,
        address: Address,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        let layout = self.layout(function)?;
        Ok(match address {
            Address::Frame(slot) => Location::Relative(layout.frame.frame_offset(slot)),
            Address::Global(global) => {
                Location::Global(self.static_layout.scalar_position(global)?)
            }
            Address::ArrayElement { array, index } => {
                self.array_element_location(array, index, function)?
            }
            Address::AbiValue => Location::Relative(layout.frame.abi_offset(AbiField::Value)),
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

    fn set_location(&mut self, location: Location, value: u8) {
        self.move_context_to_location(location);
        self.clear_current();
        self.adjust(value);
        self.move_location_to_context(location);
    }

    fn clear_location(&mut self, location: Location) {
        self.move_context_to_location(location);
        self.clear_current();
        self.move_location_to_context(location);
    }

    fn copy_locations(&mut self, src: Location, dst: Location, restore: Location) {
        if src == dst {
            return;
        }
        self.clear_location(dst);
        self.clear_location(restore);
        self.move_context_to_location(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            emitter.move_context_to_location(dst);
            emitter.adjust(1);
            emitter.move_location_to_context(dst);
            emitter.move_context_to_location(restore);
            emitter.adjust(1);
            emitter.move_location_to_context(restore);
            emitter.move_context_to_location(src);
        });
        self.output.push(BfInstruction::Loop(body));
        self.move_location_to_context(src);
        self.move_location(restore, src);
    }

    fn move_location(&mut self, src: Location, dst: Location) {
        if src == dst {
            return;
        }
        self.clear_location(dst);
        self.move_context_to_location(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            emitter.move_context_to_location(dst);
            emitter.adjust(1);
            emitter.move_location_to_context(dst);
            emitter.move_context_to_location(src);
        });
        self.output.push(BfInstruction::Loop(body));
        self.move_location_to_context(src);
    }

    fn move_context_to_location(&mut self, location: Location) {
        match location {
            Location::Relative(offset) => self.move_to(offset),
            Location::Global(position) => {
                debug_assert_eq!(self.position, 0);
                self.emit_context_to_global(position, self.config.portal_chunks());
            }
        }
    }

    fn move_location_to_context(&mut self, location: Location) {
        match location {
            Location::Relative(_) => self.move_to(0),
            Location::Global(position) => {
                debug_assert_eq!(self.position, 0);
                self.emit_global_to_context(position, self.config.portal_chunks());
            }
        }
    }

    /// Move from a function context base to one absolute static cell and
    /// rebase the compile-time origin at that cell. Stack flags are preserved.
    fn emit_context_to_global(&mut self, position: usize, context_chunks: usize) {
        debug_assert_eq!(self.position, 0);
        let stride = self.config.stride() as isize;
        self.push_move(context_chunks as isize * stride);
        self.push_move(-stride);
        self.output
            .push(BfInstruction::Loop(vec![BfInstruction::Move(-stride)]));
        self.push_move(-((self.static_layout.anchor_head() - position) as isize));
        self.position = 0;
    }

    /// Move from one absolute static cell through the anchor to the current
    /// frame context and rebase the compile-time origin there.
    fn emit_global_to_context(&mut self, position: usize, context_chunks: usize) {
        debug_assert_eq!(self.position, 0);
        let stride = self.config.stride() as isize;
        self.push_move((self.static_layout.anchor_head() - position) as isize);
        self.push_move(stride);
        self.output
            .push(BfInstruction::Loop(vec![BfInstruction::Move(stride)]));
        self.push_move(-(context_chunks as isize * stride));
        self.position = 0;
    }

    fn push_move(&mut self, distance: isize) {
        if distance != 0 {
            self.output.push(BfInstruction::Move(distance));
        }
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))],
                    return_to: countdown_resume,
                },
            ),
            Continuation::new(
                countdown_base,
                countdown,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))),
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
                    value: Some(ValueOperand::Cell(Address::AbiValue)),
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(argument))],
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
                    value: Some(ValueOperand::Cell(Address::AbiValue)),
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
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
                    value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))),
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
                    value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(11)))),
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
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
                    value: Some(ValueOperand::Cell(Address::Frame(boundary_slot))),
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
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
                    value: Some(ValueOperand::Cell(Address::Frame(high_local))),
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
                        ValueOperand::Cell(Address::Frame(FrameSlot::new(0))),
                        ValueOperand::Cell(Address::Frame(FrameSlot::new(0))),
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
                    value: Some(ValueOperand::Cell(Address::Frame(sum))),
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

    #[test]
    fn global_addresses_work_in_transfer_loop_and_branch_for_both_geometries() {
        let main = FunctionId::new(0);
        let global = crate::GlobalId::new(0);
        let entry = id(1);
        let slot = FrameSlot::new(0);
        let function = FunctionDescriptor::new(main, vec![], 1, ValueType::Void, entry);
        let continuation = Continuation::new(
            entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Global(global),
                    value: 2,
                },
                FrameInstruction::Loop {
                    condition: Address::Global(global),
                    body: vec![
                        FrameInstruction::Output {
                            src: Address::Global(global),
                        },
                        FrameInstruction::AddConst {
                            dst: Address::Global(global),
                            value: 255,
                        },
                    ],
                },
                FrameInstruction::Set {
                    dst: Address::Frame(slot),
                    value: 3,
                },
                FrameInstruction::Transfer {
                    src: Address::Frame(slot),
                    targets: vec![FrameTransferTarget {
                        dst: Address::Global(global),
                        factor: 1,
                    }],
                },
                FrameInstruction::Transfer {
                    src: Address::Global(global),
                    targets: vec![FrameTransferTarget {
                        dst: Address::Frame(slot),
                        factor: 1,
                    }],
                },
                FrameInstruction::Output {
                    src: Address::Frame(slot),
                },
                FrameInstruction::Set {
                    dst: Address::Global(global),
                    value: 1,
                },
                FrameInstruction::Branch {
                    condition: Address::Global(global),
                    then_body: vec![FrameInstruction::Set {
                        dst: Address::Frame(slot),
                        value: b'B',
                    }],
                    else_body: vec![FrameInstruction::Set {
                        dst: Address::Frame(slot),
                        value: b'X',
                    }],
                },
                FrameInstruction::Output {
                    src: Address::Frame(slot),
                },
            ],
            Terminator::Halt,
        );
        let program = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::cell(global)],
            vec![function],
            vec![continuation],
        )
        .unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(
                execute_continuations(&program, chunk_cells),
                &[2, 1, 3, b'B']
            );
        }
    }

    #[test]
    fn portal_ids_avoid_user_ids_and_report_exhaustion() {
        let source =
            "void main() { cell[2] values; cell i = 1; values[i] = 7; output(values[i]); }";
        let program = crate::lower_source(source).unwrap();
        let user_ids = program
            .continuations()
            .iter()
            .map(|continuation| continuation.id().get())
            .collect::<HashSet<_>>();
        let plan = PortalPlan::new(&program).unwrap();
        let mut hidden = Vec::new();
        hidden.extend(plan.load.map(ContinuationId::get));
        hidden.extend(plan.store.map(ContinuationId::get));
        hidden.extend(plan.ordered_sites.iter().map(|site| site.resume.get()));
        assert!(hidden.iter().all(|id| !user_ids.contains(id)));
        assert_eq!(
            hidden.iter().copied().collect::<HashSet<_>>().len(),
            hidden.len()
        );

        let mut exhausted = (1..=u16::MAX).collect::<HashSet<_>>();
        assert_eq!(
            allocate_hidden_id(&mut exhausted),
            Err(AbiCodegenError::ContinuationIdsExhausted)
        );
    }

    #[test]
    fn direct_global_scalar_and_array_arguments_normalize_to_the_caller() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let scalar = crate::GlobalId::new(0);
        let array = crate::GlobalId::new(1);
        let parameter_array = crate::FrameArrayId::new(0);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 0, ValueType::Void, main_entry),
            FunctionDescriptor::new_typed(
                callee,
                vec![
                    ParameterLocation::Cell(FrameSlot::new(0)),
                    ParameterLocation::Array(parameter_array),
                ],
                1,
                vec![crate::FrameArrayDescriptor::new(parameter_array, 2)],
                0,
                ValueType::Void,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Global(scalar),
                        value: b'Q',
                    },
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: ArrayRegion::Global(array),
                            index: 0,
                        },
                        value: b'A',
                    },
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: ArrayRegion::Global(array),
                            index: 1,
                        },
                        value: b'B',
                    },
                ],
                Terminator::Call {
                    callee,
                    arguments: vec![
                        ValueOperand::Cell(Address::Global(scalar)),
                        ValueOperand::Array(ArrayRegion::Global(array)),
                    ],
                    return_to: main_resume,
                },
            ),
            Continuation::new(main_resume, main, vec![], Terminator::Halt),
            Continuation::new(
                callee_entry,
                callee,
                vec![
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                    FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: ArrayRegion::Frame(parameter_array),
                            index: 0,
                        },
                    },
                    FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: ArrayRegion::Frame(parameter_array),
                            index: 1,
                        },
                    },
                ],
                Terminator::Return { value: None },
            ),
        ];
        let program = ContinuationProgram::new_with_globals(
            main,
            vec![
                crate::GlobalDescriptor::cell(scalar),
                crate::GlobalDescriptor::array(array, 2),
            ],
            functions,
            continuations,
        )
        .unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), b"QAB");
        }
    }

    #[test]
    fn aggregate_self_copy_is_a_no_op() {
        let main = FunctionId::new(0);
        let entry = id(1);
        let array = crate::FrameArrayId::new(0);
        let region = ArrayRegion::Frame(array);
        let program = ContinuationProgram::new(
            main,
            vec![FunctionDescriptor::new_typed(
                main,
                vec![],
                0,
                vec![crate::FrameArrayDescriptor::new(array, 2)],
                0,
                ValueType::Void,
                entry,
            )],
            vec![Continuation::new(
                entry,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: region,
                            index: 0,
                        },
                        value: b'A',
                    },
                    FrameInstruction::AggregateCopy {
                        src: region,
                        dst: region,
                        cells: 2,
                    },
                    FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: region,
                            index: 0,
                        },
                    },
                ],
                Terminator::Halt,
            )],
        )
        .unwrap();

        for chunk_cells in [8, 16] {
            assert_eq!(execute_continuations(&program, chunk_cells), b"A");
        }
    }
}
