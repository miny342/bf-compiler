//! Address resolution, frame/global transport, and primitive BF operations.

use super::*;

impl<'a> AbiEmitter<'a> {
    pub(super) fn array_element_location(
        &self,
        array: AggregateRegion,
        index: usize,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            AggregateRegion::Frame(aggregate) => Location::Relative(
                self.layout(function)?
                    .frame
                    .aggregate_element_offset(aggregate, index)?,
            ),
            AggregateRegion::Global(global) => Location::Global(
                self.static_layout
                    .aggregate_element_position(global, index)?,
            ),
            AggregateRegion::Outbox => {
                Location::Relative(self.layout(function)?.frame.outbox_offset(index)?)
            }
        })
    }

    pub(super) fn value_operand_element_location(
        &self,
        operand: ValueOperand,
        index: usize,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        match operand {
            ValueOperand::Cell(address) => {
                debug_assert_eq!(index, 0);
                self.address_location(address, function)
            }
            ValueOperand::Array(region) => self.array_element_location(region, index, function),
            ValueOperand::Aggregate {
                region,
                offset,
                cells: _,
            } => self.array_element_location(region, offset + index, function),
        }
    }

    pub(super) fn common_outbox_offset(&self, index: usize) -> isize {
        let chunk = index / self.config.chunk_cells();
        let within = index % self.config.chunk_cells();
        let route_chunks = self
            .layout(self.program.main())
            .expect("main layout")
            .frame
            .route_chunks();
        -((route_chunks + chunk + 1) as isize * self.config.stride() as isize) + 1 + within as isize
    }

    pub(super) fn acquire_branch_temporary(
        &mut self,
        function: FunctionId,
    ) -> Result<isize, AbiCodegenError> {
        let layout = self.layout(function)?;
        let start = layout.branch_temporary_start;
        let frame = layout.frame.clone();
        let slot = FrameSlot::new(start + self.branch_temporary_depth);
        self.branch_temporary_depth += 1;
        Ok(frame.frame_offset(slot))
    }

    pub(super) fn address_location(
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

    pub(super) fn current_abi_offset(&self, field: AbiField) -> Result<isize, AbiCodegenError> {
        // Dispatch helpers use the common context geometry, so any layout is
        // sufficient. Using main also covers an empty function collection.
        Ok(self.layout(self.program.main())?.frame.abi_offset(field))
    }

    pub(super) fn function(&self, id: FunctionId) -> Result<&FunctionDescriptor, AbiCodegenError> {
        self.program
            .function(id)
            .ok_or(AbiCodegenError::MissingFunctionLayout { function: id })
    }

    pub(super) fn layout(&self, id: FunctionId) -> Result<&FunctionLayout, AbiCodegenError> {
        self.layouts
            .get(&id)
            .ok_or(AbiCodegenError::MissingFunctionLayout { function: id })
    }

    pub(super) fn set_abi_field(
        &mut self,
        field: AbiField,
        value: u8,
    ) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.set(offset, value);
        Ok(())
    }

    pub(super) fn add_abi_field(
        &mut self,
        field: AbiField,
        value: u8,
    ) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.move_to(offset);
        self.adjust(value);
        self.move_to(0);
        Ok(())
    }

    pub(super) fn clear_abi_field(&mut self, field: AbiField) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.clear(offset);
        Ok(())
    }

    pub(super) fn copy_abi_field(
        &mut self,
        src: AbiField,
        dst: AbiField,
    ) -> Result<(), AbiCodegenError> {
        let src = self.current_abi_offset(src)?;
        let dst = self.current_abi_offset(dst)?;
        let restore = self.current_abi_offset(AbiField::Restore)?;
        self.copy(src, dst, restore);
        Ok(())
    }

    pub(super) fn move_abi_field(&mut self, src: AbiField, dst: AbiField) {
        let src = self.config.logical_offset_from_head(src.index()) as isize;
        let dst = self.config.logical_offset_from_head(dst.index()) as isize;
        self.move_value(src, dst);
    }

    pub(super) fn set_location(&mut self, location: Location, value: u8) {
        self.move_context_to_location(location);
        self.clear_current();
        self.adjust(value);
        self.move_location_to_context(location);
    }

    pub(super) fn clear_location(&mut self, location: Location) {
        self.move_context_to_location(location);
        self.clear_current();
        self.move_location_to_context(location);
    }

    pub(super) fn copy_locations(&mut self, src: Location, dst: Location, restore: Location) {
        if src == dst {
            return;
        }
        self.clear_location(restore);
        self.copy_locations_with_zeroed_restore(src, dst, restore);
    }

    pub(super) fn copy_locations_with_zeroed_restore(
        &mut self,
        src: Location,
        dst: Location,
        restore: Location,
    ) {
        if src == dst {
            return;
        }
        self.clear_location(dst);
        self.copy_locations_to_zeroed_destination(src, dst, restore);
    }

    /// Copy a value when both the destination and restore cells are already
    /// zero. The copy restores `src` and leaves both scratch cells zero.
    /// Callers must establish the destination invariant, typically because a
    /// fresh frame or a freshly cleared aggregate slot is being populated.
    pub(super) fn copy_locations_to_zeroed_destination(
        &mut self,
        src: Location,
        dst: Location,
        restore: Location,
    ) {
        if src == dst {
            return;
        }
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
        self.emit_loop(body);
        self.move_location_to_context(src);
        self.move_location(restore, src);
    }

    /// Copy one static byte into the current frame without crossing the live
    /// stack once per source unit. Nine shared static cells hold a zero guard
    /// and eight binary digits. The bits are folded into two nibbles,
    /// bounding stack crossings by the sum of those digits (at most 30)
    /// instead of expanding eight separate navigation templates.
    pub(super) fn copy_global_to_relative_bits(&mut self, source: usize, destination: isize) {
        self.clear(destination);
        self.emit_context_to_global(source, self.config.portal_chunks());
        self.global_to_relative_nibbles(source, 0, destination, true);
    }

    /// Destructive counterpart for a fixed callee's return payload. Navigation
    /// uses the suspended dynamic caller; the fixed source is left zero.
    pub(super) fn move_global_to_relative_nibbles(
        &mut self,
        source: usize,
        destination: isize,
        scratch: Option<usize>,
    ) {
        self.clear(destination);
        self.emit_context_to_global(source, self.config.portal_chunks());
        let scratch =
            scratch.unwrap_or_else(|| self.static_layout.remote_copy_scratch_position(0).unwrap());
        self.global_to_relative_nibbles_at(source, 0, destination, false, scratch);
    }

    /// The pointer starts at `base` in static storage and finishes at the
    /// current frame. `source` is relative to that base; the frame destination
    /// must be zero. Destructive portal returns leave the source zero, while
    /// ordinary copies restore it during transport. Shared scratch is zero on
    /// exit in both cases.
    pub(super) fn global_to_relative_nibbles(
        &mut self,
        base: usize,
        source: isize,
        destination: isize,
        restore_source: bool,
    ) {
        let scratch = self
            .static_layout
            .remote_copy_scratch_position(0)
            .expect("D=16 static layout must reserve remote-copy scratch");
        self.global_to_relative_nibbles_at(base, source, destination, restore_source, scratch);
    }

    /// Fixed returns reuse the cleared callee's route scratch next to the
    /// payload. Other global copies retain the shared static scratch.
    fn global_to_relative_nibbles_at(
        &mut self,
        base: usize,
        source: isize,
        destination: isize,
        restore_source: bool,
        scratch: usize,
    ) {
        debug_assert!(self.config.chunk_cells() >= 9);
        let context_chunks = self.config.portal_chunks();
        let zero = scratch as isize - base as isize;
        let bits = std::array::from_fn::<_, 8, _>(|index| {
            scratch as isize + 1 + index as isize - base as isize
        });

        // Both shared static scratch and the callee's reused route scratch
        // must establish and restore their zero contract locally, including
        // copies supplied by public Continuation IR.
        self.clear(zero);
        for bit in bits {
            self.clear(bit);
        }

        // Consume the source into an eight-bit counter.  All movement here is
        // within static storage; bit slides never scan the live stack.
        self.move_to(source);
        let decompose = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.increment_contiguous_byte_bits(&bits, zero, source);
        });
        self.emit_loop(decompose);

        // Fold the binary digits into low/high nibble counters while staying
        // in static storage.  Reuse bit 0 and bit 4 as the digit cells.
        for index in 1..4 {
            self.move_static_value(bits[index], bits[0], 1_u8 << index);
        }
        for index in 5..8 {
            self.move_static_value(bits[index], bits[4], 1_u8 << (index - 4));
        }

        // Only these two loop bodies contain stack-navigation code.
        for (digit, contribution) in [(bits[0], 1_u8), (bits[4], 16_u8)] {
            self.move_to(digit);
            let transport = self.capture_infallible(|emitter| {
                emitter.adjust(255);
                if restore_source {
                    emitter.move_to(source);
                    emitter.adjust(contribution);
                }
                emitter.move_to(0);
                emitter.emit_global_to_context(base, context_chunks);
                emitter.move_to(destination);
                emitter.adjust(contribution);
                emitter.move_to(0);
                emitter.emit_context_to_global(base, context_chunks);
                emitter.move_to(digit);
            });
            self.emit_loop(transport);
            self.move_to(0);
        }
        self.emit_global_to_context(base, context_chunks);
    }

    /// Destructively move one frame-relative byte into a zero static cell.
    /// The otherwise unused lanes in the D=16 route chunk hold a zero guard and
    /// eight bits, bounding live-stack crossings by two nibble values (at
    /// most 30) instead of the source byte (at most 255).
    pub(super) fn move_relative_to_global_nibbles(
        &mut self,
        source: isize,
        destination: usize,
    ) -> Result<(), AbiCodegenError> {
        self.relative_to_global_nibbles(source, destination, false, "abi.portal.route")
    }

    /// A call argument may remain live in its caller. Rebuild it from the two
    /// digits while transporting, rather than copying it through ABI Restore.
    /// The static destination must be zero and must not alias the route scratch.
    pub(super) fn relative_to_global_nibbles(
        &mut self,
        source: isize,
        destination: usize,
        restore_source: bool,
        profile_prefix: &str,
    ) -> Result<(), AbiCodegenError> {
        debug_assert!(self.config.chunk_cells() >= GLOBAL_ROUTE_NIBBLE_CELLS);
        let Location::Relative(zero) = self.route_location(ROUTE_SCRATCH_START)? else {
            unreachable!("route scratch is frame-relative")
        };
        let bits = std::array::from_fn::<_, 8, _>(|index| zero + 1 + index as isize);

        self.with_profile_site_infallible(
            "abi",
            format!("{profile_prefix}.decompose"),
            "byte decomposition",
            |emitter| {
                emitter.clear(zero);
                for bit in bits {
                    emitter.clear(bit);
                }

                emitter.move_to(source);
                let decompose = emitter.capture_infallible(|emitter| {
                    emitter.adjust(255);
                    emitter.increment_contiguous_byte_bits(&bits, zero, source);
                });
                emitter.emit_loop(decompose);
                emitter.move_to(0);
            },
        );
        self.with_profile_site_infallible(
            "abi",
            format!("{profile_prefix}.pack"),
            "nibble packing",
            |emitter| {
                for index in 1..4 {
                    emitter.move_static_value(bits[index], bits[0], 1_u8 << index);
                }
                for index in 5..8 {
                    emitter.move_static_value(bits[index], bits[4], 1_u8 << (index - 4));
                }
            },
        );
        let context_chunks = self.config.portal_chunks();
        for (digit, contribution) in [(bits[0], 1_u8), (bits[4], 16_u8)] {
            self.with_profile_site_infallible(
                "abi",
                format!("{profile_prefix}.transport.nibble.{contribution}"),
                "nibble transport",
                |emitter| {
                    emitter.move_to(digit);
                    let transport = emitter.capture_infallible(|emitter| {
                        emitter.adjust(255);
                        if restore_source {
                            emitter.move_to(source);
                            emitter.adjust(contribution);
                        }
                        emitter.move_to(0);
                        emitter.emit_context_to_global(destination, context_chunks);
                        emitter.adjust(contribution);
                        emitter.emit_global_to_context(destination, context_chunks);
                        emitter.move_to(digit);
                    });
                    emitter.emit_loop(transport);
                    emitter.move_to(0);
                },
            );
        }
        Ok(())
    }

    pub(super) fn move_static_value(&mut self, source: isize, destination: isize, factor: u8) {
        self.move_to(source);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(destination);
            emitter.adjust(factor);
            emitter.move_to(source);
        });
        self.emit_loop(body);
        self.move_to(0);
    }

    /// Increment contiguous Boolean bits without moving their old values into
    /// a carry cell. `[>]` finds the first zero, `++[-<]` sets it to one and
    /// clears the trailing ones, exiting at the zero guard before bit zero.
    /// The counter must be below 255: byte decomposition starts at zero and
    /// increments at most 255 times, so the scan never leaves the eight bits.
    pub(super) fn increment_contiguous_byte_bits(
        &mut self,
        bits: &[isize; 8],
        zero: isize,
        return_to: isize,
    ) {
        debug_assert_eq!(bits[0], zero + 1);
        debug_assert!(bits.windows(2).all(|pair| pair[1] == pair[0] + 1));
        self.move_to(bits[0]);
        self.navigation_scan(1);
        self.adjust(2);
        let site = self.current_profile_site();
        self.emit_loop(vec![
            AnnotatedBfInstruction::new(site, AnnotatedBfOperation::Add(255)),
            AnnotatedBfInstruction::new(site, AnnotatedBfOperation::Move(-1)),
        ]);
        self.position = zero;
        self.move_to(return_to);
    }

    /// Increment a little-endian Boolean bit vector. The temporary is zero on
    /// entry and exit; `return_to` is the pointer offset required by the
    /// surrounding BF loop.
    #[cfg(test)]
    pub(super) fn increment_bits(
        &mut self,
        bits: &[isize; 8],
        temporary: isize,
        index: usize,
        return_to: isize,
    ) {
        let bit = bits[index];
        self.move_to(bit);
        let was_set = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(temporary);
            emitter.adjust(1);
            emitter.move_to(bit);
        });
        self.emit_loop(was_set);
        self.adjust(1);

        self.move_to(temporary);
        let carry = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(bit);
            emitter.adjust(255);
            if index + 1 < bits.len() {
                emitter.increment_bits(bits, temporary, index + 1, temporary);
            } else {
                emitter.move_to(temporary);
            }
        });
        self.emit_loop(carry);
        self.move_to(return_to);
    }

    pub(super) fn move_location(&mut self, src: Location, dst: Location) {
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
        self.emit_loop(body);
        self.move_location_to_context(src);
    }

    pub(super) fn move_location_to_zero(&mut self, src: Location, dst: Location) {
        if src == dst {
            return;
        }
        self.move_context_to_location(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            emitter.move_context_to_location(dst);
            emitter.adjust(1);
            emitter.move_location_to_context(dst);
            emitter.move_context_to_location(src);
        });
        self.emit_loop(body);
        self.move_location_to_context(src);
    }

    pub(super) fn move_context_to_location(&mut self, location: Location) {
        match location {
            Location::Relative(offset) => self.move_to(offset),
            Location::Global(position) => {
                if let Some(base) = self.fixed_context {
                    self.move_to(position as isize - base as isize);
                    return;
                }
                debug_assert_eq!(self.position, 0);
                self.emit_context_to_global(position, self.config.portal_chunks());
            }
        }
    }

    pub(super) fn move_location_to_context(&mut self, location: Location) {
        match location {
            Location::Relative(_) => self.move_to(0),
            Location::Global(position) => {
                if self.fixed_context.is_some() {
                    self.move_to(0);
                    return;
                }
                debug_assert_eq!(self.position, 0);
                self.emit_global_to_context(position, self.config.portal_chunks());
            }
        }
    }

    /// Move from a function context base to one absolute static cell and
    /// rebase the compile-time origin at that cell. Stack flags are preserved.
    pub(super) fn emit_context_to_global(&mut self, position: usize, context_chunks: usize) {
        if let Some(base) = self.fixed_context {
            self.migrate_context(position as isize - base as isize);
            return;
        }
        self.with_profile_site_infallible(
            "abi",
            "abi.navigation.global",
            "context to global",
            |emitter| emitter.emit_context_to_global_inner(position, context_chunks),
        );
    }

    pub(super) fn emit_context_to_global_inner(&mut self, position: usize, context_chunks: usize) {
        debug_assert_eq!(self.position, 0);
        let stride = self.config.stride() as isize;
        let (canonical, group) = self.static_layout.anchor_bank();
        if group == 1 {
            // Keep the legacy sequence, including profile transitions on
            // moves that cancel, when the anchor bank is disabled.
            self.push_move(-stride);
            self.push_move(context_chunks as isize * stride);
            self.navigation_scan(-stride);
        } else {
            self.push_move((context_chunks as isize - 1) * stride);
            // Allocated stack heads are one. A coarse scan reaches the zero
            // bank head on this phase lane without adding headers to frames.
            self.navigation_scan(-(group as isize * stride));
            self.push_move(ANCHOR_BREADCRUMB);
            self.adjust(255);
            self.push_move(ANCHOR_GUIDE - ANCHOR_BREADCRUMB);
            self.navigation_scan(-stride);
            self.push_move(-ANCHOR_GUIDE);
        }
        self.push_move(position as isize - canonical as isize);
        self.position = 0;
    }

    fn navigation_scan(&mut self, distance: isize) {
        self.emit_loop(vec![AnnotatedBfInstruction::new(
            self.current_profile_site(),
            AnnotatedBfOperation::Move(distance),
        )]);
    }

    /// Move from one absolute static cell through the anchor to the current
    /// frame context and rebase the compile-time origin there.
    pub(super) fn emit_global_to_context(&mut self, position: usize, context_chunks: usize) {
        if let Some(base) = self.fixed_context {
            self.migrate_context(base as isize - position as isize);
            return;
        }
        self.with_profile_site_infallible(
            "abi",
            "abi.navigation.global",
            "global to context",
            |emitter| emitter.emit_global_to_context_inner(position, context_chunks),
        );
    }

    pub(super) fn emit_global_to_context_inner(&mut self, position: usize, context_chunks: usize) {
        debug_assert_eq!(self.position, 0);
        let stride = self.config.stride() as isize;
        let (canonical, group) = self.static_layout.anchor_bank();
        self.push_move(canonical as isize - position as isize);
        if group == 1 {
            self.push_move(stride);
            self.navigation_scan(stride);
            self.push_move(-(context_chunks as isize * stride));
        } else {
            // Find and consume the phase breadcrumb, then scan to the first
            // free stack head on that lane and step back to this context.
            self.push_move(ANCHOR_BREADCRUMB);
            self.navigation_scan(stride);
            self.adjust(1);
            self.push_move(-ANCHOR_BREADCRUMB);
            let coarse = group as isize * stride;
            self.push_move(coarse);
            self.navigation_scan(coarse);
            self.push_move(-coarse - (context_chunks as isize - 1) * stride);
        }
        self.position = 0;
    }

    pub(super) fn push_move(&mut self, distance: isize) {
        if distance != 0 {
            self.emit_operation(AnnotatedBfOperation::Move(distance));
        }
    }

    pub(super) fn copy(&mut self, src: isize, dst: isize, restore: isize) {
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
        self.emit_loop(body);
        self.move_value(restore, src);
        self.move_to(0);
    }

    pub(super) fn move_value(&mut self, src: isize, dst: isize) {
        self.clear(dst);
        self.move_to(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(dst);
            emitter.adjust(1);
            emitter.move_to(src);
        });
        self.emit_loop(body);
        self.move_to(0);
    }

    pub(super) fn set(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.clear_current();
        self.adjust(value);
        self.move_to(0);
    }

    pub(super) fn set_raw(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.clear_current();
        self.adjust(value);
    }

    pub(super) fn clear(&mut self, offset: isize) {
        self.move_to(offset);
        self.clear_current();
        self.move_to(0);
    }

    pub(super) fn clear_current(&mut self) {
        let site = self.current_profile_site();
        self.emit_loop(vec![AnnotatedBfInstruction::new(
            site,
            AnnotatedBfOperation::Add(255),
        )]);
    }

    pub(super) fn adjust(&mut self, value: u8) {
        self.emit_operation(AnnotatedBfOperation::Add(value));
    }

    pub(super) fn move_to(&mut self, destination: isize) {
        self.emit_operation(AnnotatedBfOperation::Move(destination - self.position));
        self.position = destination;
    }

    /// Move to another frame's context base and make it the new offset origin.
    pub(super) fn migrate_context(&mut self, delta: isize) {
        self.move_to(delta);
        self.position = 0;
    }

    pub(super) fn capture<T>(
        &mut self,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<Vec<AnnotatedBfInstruction>, AbiCodegenError> {
        let outer = std::mem::take(&mut self.output);
        emit(self)?;
        Ok(std::mem::replace(&mut self.output, outer))
    }

    pub(super) fn capture_infallible(
        &mut self,
        emit: impl FnOnce(&mut Self),
    ) -> Vec<AnnotatedBfInstruction> {
        let outer = std::mem::take(&mut self.output);
        emit(self);
        std::mem::replace(&mut self.output, outer)
    }
}
