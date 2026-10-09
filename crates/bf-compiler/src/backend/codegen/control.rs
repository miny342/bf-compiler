//! Continuation terminators and dynamic call/return emission.

use super::provenance::terminator_kind;
use super::*;

/// A copied local aggregate that is returned immediately needs no intermediate
/// storage. Keep the original body slice so lifetime-plan instruction identities
/// and source paths remain valid. No calls, writes or control edges are crossed.
fn aggregate_return_source(c: &Continuation) -> Option<(usize, ValueOperand)> {
    let Terminator::Return {
        value:
            Some(ValueOperand::Aggregate {
                region,
                offset,
                cells,
            }),
    } = *c.terminator()
    else {
        return None;
    };
    if !matches!(region, AggregateRegion::Frame(_)) {
        return None;
    }
    let mut end = c.body().len();
    loop {
        let instruction = c.body().get(end.checked_sub(1)?)?;
        match instruction {
            FrameInstruction::AggregateCopy { src, dst, .. } if src == dst => end -= 1,
            FrameInstruction::Copy { src, dst } if src == dst => end -= 1,
            FrameInstruction::AggregateCopy {
                src,
                dst,
                cells: copied,
            } if *dst == region
                && offset + cells <= *copied
                && matches!(src, AggregateRegion::Frame(_) | AggregateRegion::Outbox) =>
            {
                return Some((
                    end - 1,
                    ValueOperand::Aggregate {
                        region: *src,
                        offset,
                        cells,
                    },
                ));
            }
            _ => break,
        }
    }
    // Imported flat CIR represents the same copy as individual leaves. Only
    // distinct regions with matching contiguous offsets can form this snapshot.
    if cells == 0 || cells > 16 || end < cells {
        return None;
    }
    let mut seen = [false; 16];
    let mut origin = None;
    for instruction in &c.body()[end - cells..end] {
        let FrameInstruction::Copy {
            src:
                Address::ArrayElement {
                    array: src,
                    index: source,
                },
            dst:
                Address::ArrayElement {
                    array: dst,
                    index: destination,
                },
        } = *instruction
        else {
            return None;
        };
        let index = destination.checked_sub(offset)?;
        if dst != region
            || src == region
            || index >= cells
            || seen[index]
            || !matches!(src, AggregateRegion::Frame(_) | AggregateRegion::Outbox)
        {
            return None;
        }
        let source_offset = source.checked_sub(index)?;
        if origin.is_some_and(|origin| origin != (src, source_offset)) {
            return None;
        }
        origin = Some((src, source_offset));
        seen[index] = true;
    }
    let (region, offset) = origin?;
    Some((
        end - cells,
        ValueOperand::Aggregate {
            region,
            offset,
            cells,
        },
    ))
}

impl<'a> AbiEmitter<'a> {
    pub(super) fn return_body_length(&self, c: &Continuation) -> usize {
        aggregate_return_source(c).map_or(c.body().len(), |(end, _)| end)
    }
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
            } => self.emit_call(
                continuation.id(),
                continuation.function(),
                *callee,
                arguments,
                *return_to,
            )?,
            Terminator::Return { value } => self.emit_return(
                continuation.function(),
                aggregate_return_source(continuation)
                    .map(|(_, value)| value)
                    .or(*value),
            )?,
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
        call: ContinuationId,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.call", "ABI call", |emitter| {
            let direct =
                emitter.plan_direct_call_entry(call, caller, callee, arguments, return_to)?;
            let setup = direct.as_ref().map(|p| &p.setup);
            if emitter
                .fixed
                .is_some_and(|plan| plan.contexts.contains_key(&callee))
            {
                emitter.emit_fixed_call(call, caller, callee, arguments, return_to, setup)?;
            } else {
                emitter.emit_call_inner(Some(call), caller, callee, arguments, return_to, setup)?;
            }
            if let Some(direct) = direct {
                emitter.output.extend(direct.body);
                emitter.position = direct.position;
            }
            Ok(())
        })
    }

    pub(super) fn emit_call_inner(
        &mut self,
        call: Option<ContinuationId>,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
        direct: Option<&DirectCallSetup>,
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

        // Repeated/overlapping sources are consumed only at their final use,
        // and only if they are dead after resume. This retains the snapshot
        // semantics of the already-evaluated, left-to-right argument list.
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
        let mut copies = Vec::new();
        let mut remaining = HashMap::<Location, usize>::new();
        for (&argument, &(parameter, parameter_cells)) in arguments.iter().zip(&parameters) {
            for index in 0..parameter_cells.unwrap_or(1) {
                let address = match argument {
                    ValueOperand::Cell(address) => address,
                    ValueOperand::Array(array) => Address::ArrayElement { array, index },
                    ValueOperand::Aggregate {
                        region: array,
                        offset,
                        ..
                    } => Address::ArrayElement {
                        array,
                        index: offset + index,
                    },
                };
                let src = self.address_location(address, caller)?;
                let offset = match parameter {
                    ParameterLocation::Cell(slot) => callee_frame.frame_offset(slot),
                    ParameterLocation::AggregateElement { aggregate, index } => {
                        callee_frame.aggregate_element_offset(aggregate, index)?
                    }
                    ParameterLocation::Array(aggregate)
                    | ParameterLocation::Aggregate(aggregate) => {
                        callee_frame.aggregate_element_offset(aggregate, index)?
                    }
                };
                let dst = Location::Relative(callee_context_delta + offset);
                *remaining.entry(src).or_default() += 1;
                copies.push((address, src, dst));
            }
        }
        let mut written_parameters = HashSet::new();
        for (address, src, dst) in copies {
            if !written_parameters.insert(dst) {
                self.clear_location(dst);
            }
            let left = remaining.get_mut(&src).unwrap();
            *left -= 1;
            if *left == 0 && call.is_some_and(|call| self.lifetime.terminal_dead(call, address)) {
                self.move_location_to_zero(src, dst);
            } else {
                self.copy_locations_to_zeroed_destination(src, dst, restore);
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
        if direct.is_none() {
            self.set_pc_at(
                callee_context_delta,
                AbiField::NextPcLow,
                AbiField::NextPcHigh,
                entry,
            );
        }
        if direct.is_none_or(|p| p.preserve_return_pc) {
            self.set_pc_at(
                callee_context_delta,
                AbiField::ReturnPcLow,
                AbiField::ReturnPcHigh,
                return_to,
            );
        }

        self.migrate_context(callee_context_delta);
        Ok(())
    }

    pub(super) fn emit_return(
        &mut self,
        callee: FunctionId,
        value: Option<ValueOperand>,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.return", "ABI return", |emitter| {
            let direct = emitter.plan_direct_return_resume(callee)?;
            if emitter.fixed.is_some() {
                emitter.emit_fixed_return(
                    callee,
                    value,
                    direct.as_ref().is_some_and(|r| r.delivered),
                )?;
            } else {
                emitter.emit_return_inner(callee, value)?;
            }
            if let Some(direct) = direct {
                emitter.move_to(0);
                emitter.output.extend(direct.body);
                emitter.position = direct.position;
            } else {
                emitter.set_known_return_pc(callee)?;
            }
            Ok(())
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
                // The callee frame dies here; local results need no restore
                // or staging through its ABI Value. Globals remain preserved.
                if matches!(value, Location::Relative(_)) {
                    self.move_location(value, caller_value);
                } else {
                    self.copy_locations(value, callee_value, restore);
                    self.move_location(callee_value, caller_value);
                }
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
                    if matches!(src, Location::Relative(_)) {
                        self.move_location(src, dst);
                    } else {
                        self.copy_locations_with_zeroed_restore(src, dst, restore);
                    }
                }
                self.clear_location(caller_value);
            }
            None => self.clear_location(caller_value),
        }

        if !self.has_direct_return(callee) {
            self.move_value(
                callee_frame.abi_offset(AbiField::ReturnPcLow),
                caller_delta + callee_frame.abi_offset(AbiField::NextPcLow),
            );
            self.move_value(
                callee_frame.abi_offset(AbiField::ReturnPcHigh),
                caller_delta + callee_frame.abi_offset(AbiField::NextPcHigh),
            );
        }

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
