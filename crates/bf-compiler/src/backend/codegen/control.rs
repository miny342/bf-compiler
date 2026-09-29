//! Continuation terminators and dynamic call/return emission.

use super::provenance::terminator_kind;
use super::*;

impl<'a> AbiEmitter<'a> {
    pub(super) fn emit_terminator(
        &mut self,
        continuation: &Continuation,
    ) -> Result<(), AbiCodegenError> {
        if matches!(
            self.granularity,
            ProfileGranularity::Abi | ProfileGranularity::Continuation
        ) {
            self.emit_terminator_inner(continuation)
        } else {
            self.with_profile_site(
                "terminator",
                format!(
                    "function.{}.terminator.{}",
                    continuation.function().index(),
                    terminator_kind(continuation.terminator())
                ),
                terminator_kind(continuation.terminator()),
                |emitter| emitter.emit_terminator_inner(continuation),
            )
        }
    }

    pub(super) fn emit_terminator_inner(
        &mut self,
        continuation: &Continuation,
    ) -> Result<(), AbiCodegenError> {
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
                self.emit_loop(body);
                self.move_location_to_context(condition);
            }
            Terminator::Call {
                callee,
                arguments,
                return_to,
            } => self.emit_call(continuation.function(), *callee, arguments, *return_to)?,
            Terminator::Return { value } => self.emit_return(continuation.function(), *value)?,
            Terminator::ArrayLoad {
                array: _,
                index: _,
                destination: _,
                return_to: _,
            }
            | Terminator::ArrayStore { .. }
            | Terminator::AggregateLoad { .. }
            | Terminator::AggregateStore { .. } => self.emit_portal_start(
                *self
                    .portal
                    .sites
                    .get(&continuation.id())
                    .expect("every validated portal terminator has a portal site"),
            )?,
            Terminator::Abort | Terminator::Halt => self.clear_abi_field(AbiField::Active)?,
        }
        Ok(())
    }

    pub(super) fn emit_call(
        &mut self,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.call", "ABI call", |emitter| {
            if emitter.fixed.is_some() {
                return emitter.emit_fixed_call(caller, callee, arguments, return_to);
            }
            emitter.emit_call_inner(caller, callee, arguments, return_to)
        })
    }

    pub(super) fn emit_call_inner(
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
                    ParameterLocation::Cell(_) | ParameterLocation::AggregateElement { .. } => None,
                    ParameterLocation::Array(array) | ParameterLocation::Aggregate(array) => Some(
                        callee_function
                            .frame_aggregate(*array)
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
        if !arguments.is_empty() {
            self.clear_location(restore);
        }
        // An aggregate parameter can overlap a later scalar parameter (or
        // vice versa). Only the first write is guaranteed to see a zero cell.
        let mut written_parameters = HashSet::new();
        for (&argument, &(parameter, parameter_cells)) in arguments.iter().zip(&parameters) {
            match (argument, parameter) {
                (ValueOperand::Cell(argument), ParameterLocation::Cell(parameter)) => {
                    let src = self.address_location(argument, caller)?;
                    let dst = Location::Relative(
                        callee_context_delta + callee_frame.frame_offset(parameter),
                    );
                    if !written_parameters.insert(dst) {
                        self.clear_location(dst);
                    }
                    self.copy_locations_to_zeroed_destination(src, dst, restore);
                }
                (
                    ValueOperand::Cell(argument),
                    ParameterLocation::AggregateElement { aggregate, index },
                ) => {
                    let src = self.address_location(argument, caller)?;
                    let dst = Location::Relative(
                        callee_context_delta
                            + callee_frame.aggregate_element_offset(aggregate, index)?,
                    );
                    if !written_parameters.insert(dst) {
                        self.clear_location(dst);
                    }
                    self.copy_locations_to_zeroed_destination(src, dst, restore);
                }
                (
                    ValueOperand::Array(_) | ValueOperand::Aggregate { .. },
                    ParameterLocation::Array(parameter) | ParameterLocation::Aggregate(parameter),
                ) => {
                    let cells = parameter_cells.expect("aggregate parameter size");
                    for index in 0..cells {
                        let src = self.value_operand_element_location(argument, index, caller)?;
                        let dst = Location::Relative(
                            callee_context_delta
                                + callee_frame.aggregate_element_offset(parameter, index)?,
                        );
                        if !written_parameters.insert(dst) {
                            self.clear_location(dst);
                        }
                        self.copy_locations_to_zeroed_destination(src, dst, restore);
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

    pub(super) fn emit_return(
        &mut self,
        callee: FunctionId,
        value: Option<ValueOperand>,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.return", "ABI return", |emitter| {
            if emitter.fixed.is_some() {
                return emitter.emit_fixed_return(callee, value);
            }
            emitter.emit_return_inner(callee, value)
        })
    }

    pub(super) fn emit_return_inner(
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
            Some(operand @ (ValueOperand::Array(_) | ValueOperand::Aggregate { .. })) => {
                let cells = match self.function(callee)?.return_type() {
                    ValueType::Array(cells) | ValueType::Aggregate { cells } => cells,
                    _ => unreachable!("validated aggregate return type"),
                };
                self.clear_location(restore);
                for index in 0..cells {
                    let src = self.value_operand_element_location(operand, index, callee)?;
                    let dst = Location::Relative(caller_delta + self.common_outbox_offset(index));
                    self.copy_locations_with_zeroed_restore(src, dst, restore);
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

    pub(super) fn set_next_pc(&mut self, id: ContinuationId) -> Result<(), AbiCodegenError> {
        let id = self.dispatch_encoding.encode(id);
        self.set_abi_field(AbiField::NextPcLow, id as u8)?;
        self.set_abi_field(AbiField::NextPcHigh, (id >> 8) as u8)
    }

    pub(super) fn set_pc_raw(&mut self, context_base: isize, id: ContinuationId) {
        let config = self.config;
        let id = self.dispatch_encoding.encode(id);
        self.set_raw(
            context_base + config.logical_offset_from_head(AbiField::PcLow.index()) as isize,
            id as u8,
        );
        self.set_raw(
            context_base + config.logical_offset_from_head(AbiField::PcHigh.index()) as isize,
            (id >> 8) as u8,
        );
    }

    pub(super) fn set_pc_at(
        &mut self,
        context_base: isize,
        low: AbiField,
        high: AbiField,
        id: ContinuationId,
    ) {
        let id = self.dispatch_encoding.encode(id);
        let low = context_base + self.config.logical_offset_from_head(low.index()) as isize;
        let high = context_base + self.config.logical_offset_from_head(high.index()) as isize;
        self.set(low, id as u8);
        self.set(high, (id >> 8) as u8);
    }
}
