//! Page-local request transport using bounded constant-distance jumps.

use super::*;

impl<'a> AbiEmitter<'a> {
    pub(super) fn emit_page_portal_accessor(
        &mut self,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            "abi.portal.page",
            "page portal resolver",
            |emitter| {
                let page_span = aggregate_page_portal_offset(1, emitter.config)? as isize;
                let sixteen_page_span = aggregate_page_portal_offset(16, emitter.config)? as isize;

                // Keep the two page digits for the return trip. Each outward
                // selector is consumed before moving the request directly to
                // its chosen portal, rather than carrying Index/Value once per
                // digit unit. Restore and Scratch3 hold the return selectors.
                emitter.split_abi_nibbles(
                    AbiField::Scratch0,
                    AbiField::Scratch1,
                    AbiField::Scratch2,
                )?;
                emitter.copy(
                    emitter.current_abi_offset(AbiField::Scratch1)?,
                    emitter.current_abi_offset(AbiField::Restore)?,
                    emitter.current_abi_offset(AbiField::Scratch0)?,
                );
                emitter.copy(
                    emitter.current_abi_offset(AbiField::Scratch2)?,
                    emitter.current_abi_offset(AbiField::Scratch3)?,
                    emitter.current_abi_offset(AbiField::Scratch0)?,
                );

                let high_fields: &[AbiField] = match kind {
                    PortalAccessKind::Load => &[
                        AbiField::Index,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch2,
                        AbiField::Scratch3,
                    ],
                    PortalAccessKind::Store => &[
                        AbiField::Index,
                        AbiField::Value,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch2,
                        AbiField::Scratch3,
                    ],
                };
                emitter.emit_page_portal_digit(
                    AbiField::Scratch2,
                    sixteen_page_span,
                    high_fields,
                )?;
                let low_fields: &[AbiField] = match kind {
                    PortalAccessKind::Load => &[
                        AbiField::Index,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch3,
                    ],
                    PortalAccessKind::Store => &[
                        AbiField::Index,
                        AbiField::Value,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch3,
                    ],
                };
                emitter.emit_page_portal_digit(AbiField::Scratch1, page_span, low_fields)?;

                // One shared payload resolver suffices for all selected pages.
                emitter.emit_direct_payload_countdown(kind)?;

                let low_return_fields: &[AbiField] = match kind {
                    PortalAccessKind::Load => &[AbiField::Scratch3, AbiField::Value],
                    PortalAccessKind::Store => &[AbiField::Scratch3],
                };
                emitter.emit_page_portal_digit(AbiField::Restore, -page_span, low_return_fields)?;
                let high_return_fields: &[AbiField] = match kind {
                    PortalAccessKind::Load => &[AbiField::Value],
                    PortalAccessKind::Store => &[],
                };
                emitter.emit_page_portal_digit(
                    AbiField::Scratch3,
                    -sixteen_page_span,
                    high_return_fields,
                )?;
                Ok(())
            },
        )
    }

    /// Limit the shared jump table using declared payload sizes. The walk
    /// fallback still accepts a digit beyond that limit without truncation.
    fn page_digit_limit(&self, delta: isize) -> Result<u8, AbiCodegenError> {
        let max_cells = self
            .program
            .globals()
            .iter()
            .map(|g| match g.value_type() {
                ValueType::Array(n) | ValueType::Aggregate { cells: n } => n,
                _ => 1,
            })
            .chain(
                self.program
                    .functions()
                    .iter()
                    .flat_map(|f| f.frame_aggregates().iter().map(|a| a.cells())),
            )
            .max()
            .unwrap_or(1);
        let max_page = max_cells.saturating_sub(1) / 256;
        let page_span = aggregate_page_portal_offset(1, self.config)? as isize;
        let limit = if delta.abs() == page_span {
            max_page
        } else {
            max_page / 16
        };
        Ok(limit.min(15) as u8)
    }

    pub(super) fn emit_page_portal_digit(
        &mut self,
        counter: AbiField,
        delta: isize,
        carried: &[AbiField],
    ) -> Result<(), AbiCodegenError> {
        let maximum = self.page_digit_limit(delta)?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.emit_page_portal_jump_level(counter, delta, carried, maximum, 0)
    }

    /// Digits are 0..=15. Nested countdowns select one constant-distance jump.
    /// Branch and the consumed selector are zero at the destination portal,
    /// so all outer countdown levels exit there despite the changed origin.
    pub(super) fn emit_page_portal_jump_level(
        &mut self,
        counter: AbiField,
        delta: isize,
        carried: &[AbiField],
        maximum: u8,
        level: u8,
    ) -> Result<(), AbiCodegenError> {
        let counter_offset = self.current_abi_offset(counter)?;
        if level < maximum {
            self.move_to(counter_offset);
            let body = self.capture(|emitter| {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_page_portal_jump_level(counter, delta, carried, maximum, level + 1)?;
                emitter.move_to(counter_offset);
                Ok(())
            })?;
            self.emit_loop(body);
        }
        if level == maximum && maximum < 15 {
            // Walk a larger digit's remainder, carrying Branch=1 as well as
            // the request. The selected constant jump then runs once at the
            // new portal. No protocol fields remain dirty in skipped pages.
            let mut fields = carried.to_vec();
            for field in [counter, AbiField::Branch] {
                if !fields.contains(&field) {
                    fields.push(field);
                }
            }
            self.move_to(counter_offset);
            let body = self.capture(|emitter| {
                emitter.adjust(255);
                emitter.move_to(0);
                for &field in &fields {
                    emitter.move_page_portal_field(field, delta)?;
                }
                emitter.migrate_context(delta);
                emitter.move_to(counter_offset);
                Ok(())
            })?;
            self.emit_loop(body);
        }
        self.move_to(0);
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            if level != 0 {
                let jump = delta * isize::from(level);
                for &field in carried {
                    if field != counter {
                        emitter.move_page_portal_field(field, jump)?;
                    }
                }
                emitter.migrate_context(jump);
            }
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn move_page_portal_field(
        &mut self,
        field: AbiField,
        delta: isize,
    ) -> Result<(), AbiCodegenError> {
        let field = self.current_abi_offset(field)?;
        self.move_value(field, delta + field);
        Ok(())
    }
}
