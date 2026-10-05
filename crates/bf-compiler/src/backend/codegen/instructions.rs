//! Frame instruction and structured local control-flow emission.

use super::*;

impl<'a> AbiEmitter<'a> {
    pub(super) fn emit_all(
        &mut self,
        instructions: &[FrameInstruction],
        function: FunctionId,
        sources: Option<&[Option<SourceSpan>]>,
    ) -> Result<(), AbiCodegenError> {
        let mut index = 0;
        while index < instructions.len() {
            let FrameInstruction::Set {
                dst:
                    Address::ArrayElement {
                        array,
                        index: first,
                    },
                value: 0,
            } = instructions[index]
            else {
                let source = sources
                    .and_then(|sources| sources.get(index))
                    .copied()
                    .flatten()
                    .or(self.current_source);
                self.with_source_span(source, |emitter| {
                    emitter.with_instruction_path(index.to_string(), |emitter| {
                        emitter.emit_instruction(&instructions[index], function)
                    })
                })?;
                index += 1;
                continue;
            };
            let mut end = index + 1;
            while let Some(FrameInstruction::Set {
                dst:
                    Address::ArrayElement {
                        array: candidate,
                        index: element,
                    },
                value: 0,
            }) = instructions.get(end)
            {
                if *candidate != array || *element != first + (end - index) {
                    break;
                }
                end += 1;
            }
            if end - index >= 2 && array != AggregateRegion::Outbox {
                self.emit_aggregate_clear_range(
                    array,
                    first,
                    function,
                    &instructions[index..end],
                    index,
                    sources,
                )?;
            } else {
                for (offset, instruction) in instructions[index..end].iter().enumerate() {
                    let source = sources
                        .and_then(|sources| sources.get(index + offset))
                        .copied()
                        .flatten()
                        .or(self.current_source);
                    self.with_source_span(source, |emitter| {
                        emitter.with_instruction_path((index + offset).to_string(), |emitter| {
                            emitter.emit_instruction(instruction, function)
                        })
                    })?;
                }
            }
            index = end;
        }
        Ok(())
    }

    pub(super) fn emit_aggregate_clear_range(
        &mut self,
        region: AggregateRegion,
        first: usize,
        function: FunctionId,
        instructions: &[FrameInstruction],
        instruction_start: usize,
        sources: Option<&[Option<SourceSpan>]>,
    ) -> Result<(), AbiCodegenError> {
        let base = self.portal_base_location(region, function)?;
        self.move_context_to_location(base);
        for (instruction_offset, instruction) in instructions.iter().enumerate() {
            let index = first + instruction_offset;
            let mut offset = aggregate_element_physical_offset(index, self.config)? as isize;
            // Dynamic-to-global navigation rebases at the aggregate head.
            // Relative and fixed-to-global moves retain the function origin.
            if let Location::Relative(base) = base {
                offset += base;
            } else if let (Location::Global(base), Some(context)) = (base, self.fixed_context) {
                offset += base as isize - context as isize;
            }
            let source = sources
                .and_then(|sources| sources.get(instruction_start + instruction_offset))
                .copied()
                .flatten()
                .or(self.current_source);
            self.with_source_span(source, |emitter| {
                emitter.with_instruction_path(
                    (instruction_start + instruction_offset).to_string(),
                    |emitter| {
                        emitter.emit_instruction_site(instruction, function, |emitter| {
                            emitter.with_profile_site(
                                "abi",
                                "abi.frame.set",
                                "frame set",
                                |emitter| {
                                    emitter.clear(offset);
                                    Ok(())
                                },
                            )
                        })
                    },
                )
            })?;
        }
        self.move_location_to_context(base);
        Ok(())
    }

    pub(super) fn emit_instruction(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        self.emit_instruction_site(instruction, function, |emitter| {
            emitter.emit_instruction_with_abi_site(instruction, function)
        })
    }

    pub(super) fn emit_instruction_inner(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        match instruction {
            FrameInstruction::SubWithBorrow {
                left,
                right,
                difference,
                borrow,
                true_value,
                false_value,
            } => {
                self.emit_compare(
                    *left,
                    *right,
                    *borrow,
                    *true_value,
                    *false_value,
                    Some(*difference),
                    function,
                )?;
            }
            FrameInstruction::Compare {
                left,
                right,
                dst,
                true_value,
                false_value,
            } => {
                self.emit_compare(
                    *left,
                    *right,
                    *dst,
                    *true_value,
                    *false_value,
                    None,
                    function,
                )?;
            }
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
            FrameInstruction::Copy { src, dst } => {
                let source_address = *src;
                let src = self.address_location(source_address, function)?;
                let dst = self.address_location(*dst, function)?;
                let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
                if self.nibble_transfer
                    && self.fixed_context.is_none()
                    && matches!(source_address, Address::Global(_))
                    && let (Location::Global(source), Location::Relative(destination)) = (src, dst)
                {
                    self.copy_global_to_relative_bits(source, destination);
                } else {
                    self.copy_locations(src, dst, restore);
                }
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
                self.emit_operation(AnnotatedBfOperation::Input);
                self.move_location_to_context(dst);
            }
            FrameInstruction::Output { src } => {
                let src = self.address_location(*src, function)?;
                self.move_context_to_location(src);
                self.emit_operation(AnnotatedBfOperation::Output);
                self.move_location_to_context(src);
            }
            FrameInstruction::Loop { condition, body } => {
                let condition = self.address_location(*condition, function)?;
                self.move_context_to_location(condition);
                let body = self.capture(|emitter| {
                    emitter.move_location_to_context(condition);
                    emitter.with_instruction_path("loop", |emitter| {
                        emitter.emit_all(body, function, None)
                    })?;
                    emitter.move_context_to_location(condition);
                    Ok(())
                })?;
                self.emit_loop(body);
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

    /// Destructive unsigned comparison. Default emission stages operands in
    /// Scratch0..3; opt-in emission uses private guards beside frame operands.
    /// Both layouts satisfy flag-right = zero-flag = left-left_zero = 1.
    /// The countdown exits at right for right <= left, or at flag otherwise;
    /// result processing joins at zero and clears both operands and the flag.
    /// Restore retains the optional wrapping difference, separate from branch
    /// temporaries. Equal or non-frame operands use the default staging path.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_compare(
        &mut self,
        left: Address,
        right: Address,
        dst: Address,
        true_value: u8,
        false_value: u8,
        difference: Option<Address>,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let inplace = self.inplace_compare
            && self.fixed_context.is_none()
            && matches!((left, right), (Address::Frame(_), Address::Frame(_)))
            && left != right;
        let left = self.address_location(left, function)?;
        let right = self.address_location(right, function)?;
        let dst = self.address_location(dst, function)?;
        let difference = difference
            .map(|address| self.address_location(address, function))
            .transpose()?;
        // Restore is private ABI scratch, separate from structured-branch flags.
        // Retain the countdown remainder here when both outputs are requested.
        let remainder = self.current_abi_offset(AbiField::Restore)?;
        let (right_cell, flag, zero, left_cell, left_zero) = if inplace {
            let (Location::Relative(left), Location::Relative(right)) = (left, right) else {
                unreachable!("in-place comparison uses frame operands");
            };
            // Each operand slot owns [zero, value, flag, zero]. Guards stay
            // zero; the right-hand flag is consumed by result delivery below.
            (right, right + 1, right + 2, left, left - 1)
        } else {
            let right_cell = self.current_abi_offset(AbiField::Scratch0)?;
            let flag = self.current_abi_offset(AbiField::Scratch1)?;
            let zero = self.current_abi_offset(AbiField::Scratch2)?;
            let left_cell = self.current_abi_offset(AbiField::Scratch3)?;
            if left == right {
                self.copy_locations(
                    right,
                    Location::Relative(right_cell),
                    Location::Relative(flag),
                );
            } else {
                self.move_location_to_zero(right, Location::Relative(right_cell));
            }
            self.move_location_to_zero(left, Location::Relative(left_cell));
            (right_cell, flag, zero, left_cell, zero)
        };
        debug_assert_eq!(
            [flag - right_cell, zero - flag, left_cell - left_zero],
            [1, 1, 1]
        );
        if difference.is_some() {
            self.clear(remainder);
        }
        self.move_to(flag);
        self.adjust(1);
        self.move_to(right_cell);
        let countdown = self.capture_infallible(|emitter| {
            emitter.move_to(left_cell);
            let nonzero = emitter.capture_infallible(|emitter| {
                emitter.adjust(255);
                emitter.move_to(left_zero);
            });
            emitter.emit_loop(nonzero);
            // Nonzero left exits at its zero guard; zero left stays at its
            // operand. The same move maps these to right or its flag.
            emitter.push_move(right_cell - left_zero);
            emitter.position = right_cell;
            emitter.adjust(255);
        });
        self.emit_loop(countdown);
        // Exit at right=0 for right <= left, otherwise flag=0 with right-left remaining.
        self.push_move(1);
        self.position = flag;
        let not_greater = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            if false_value != 0 {
                emitter.adjust(false_value);
            }
            emitter.move_to(left_cell);
            if difference.is_some() {
                emitter.move_value(left_cell, remainder);
            } else {
                emitter.clear_current();
            }
            emitter.move_to(zero);
        });
        self.emit_loop(not_greater);
        self.position = zero;
        self.move_to(right_cell);
        let greater = self.capture_infallible(|emitter| {
            if difference.is_some() {
                let subtract = emitter.capture_infallible(|emitter| {
                    emitter.adjust(255);
                    emitter.move_to(remainder);
                    emitter.adjust(255);
                    emitter.move_to(right_cell);
                });
                emitter.emit_loop(subtract);
            } else {
                emitter.clear_current();
            }
            if true_value != 0 {
                emitter.move_to(flag);
                emitter.adjust(true_value);
            }
            emitter.move_to(right_cell);
        });
        self.emit_loop(greater);
        self.move_to(0);
        if let Some(difference) = difference {
            self.move_location(Location::Relative(remainder), difference);
        }
        self.move_location(Location::Relative(flag), dst);
        Ok(())
    }

    pub(super) fn emit_structured_branch(
        &mut self,
        condition: Address,
        then_body: &[FrameInstruction],
        else_body: &[FrameInstruction],
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let condition = self.address_location(condition, function)?;

        // A missing else arm needs no flag: the condition cell itself gates
        // the then arm.  Clearing it before and after the body preserves the
        // destructive Branch contract even when the body writes it again.
        if else_body.is_empty() {
            self.move_context_to_location(condition);
            let then_loop = self.capture(|emitter| {
                emitter.clear_current();
                emitter.move_location_to_context(condition);
                emitter.with_instruction_path("then", |emitter| {
                    emitter.emit_all(then_body, function, None)
                })?;
                emitter.clear_location(condition);
                emitter.move_context_to_location(condition);
                Ok(())
            })?;
            self.emit_loop(then_loop);
            self.move_location_to_context(condition);
            return Ok(());
        }

        let flag = Location::Relative(self.acquire_branch_temporary(function)?);
        self.set_location(flag, 1);

        self.move_context_to_location(condition);
        let then_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(condition);
            emitter.with_instruction_path("then", |emitter| {
                emitter.emit_all(then_body, function, None)
            })?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.move_context_to_location(condition);
            Ok(())
        })?;
        self.emit_loop(then_loop);
        self.move_location_to_context(condition);

        self.move_context_to_location(flag);
        let else_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(flag);
            emitter.with_instruction_path("else", |emitter| {
                emitter.emit_all(else_body, function, None)
            })?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.move_context_to_location(flag);
            Ok(())
        })?;
        self.emit_loop(else_loop);
        self.move_location_to_context(flag);
        self.branch_temporary_depth -= 1;
        Ok(())
    }

    pub(super) fn transfer(
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
        self.emit_loop(body);
        self.move_location_to_context(src);
        Ok(())
    }

    pub(super) fn aggregate_copy(
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
        self.clear_location(restore);
        for index in 0..cells {
            let src = self.array_element_location(src, index, function)?;
            let dst = self.array_element_location(dst, index, function)?;
            self.copy_locations_with_zeroed_restore(src, dst, restore);
        }
        Ok(())
    }
}
