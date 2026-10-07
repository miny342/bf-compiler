//! Dynamic aggregate access, request routing, and resume emission.

use super::*;

impl<'a> AbiEmitter<'a> {
    fn portal_source_dead(&self, site: PortalSite, address: Address) -> bool {
        // Only ordinary frame scalars: aggregate index aliasing can change a
        // value that the runtime-selected access has not read yet.
        self.fixed.is_none()
            && matches!(address, Address::Frame(_))
            && self.lifetime.terminal_dead(site.origin, address)
    }

    fn portal_offset_dead(&self, site: PortalSite, address: Address, low: bool) -> bool {
        // The offset is reread between leaves. Word halves and a scalar store
        // value may share a source; consume only the final internal read.
        // A dead static frame field can also be consumed for a single-cell
        // global access. Frame portals may alias their own index field, and
        // multi-leaf portals must retain it until the final request.
        let dead_field = self.fixed.is_none()
            && matches!(site.region, AggregateRegion::Global(_))
            && site.cells == 1
            && matches!(
                address,
                Address::ArrayElement {
                    array: AggregateRegion::Frame(_),
                    ..
                }
            )
            && self.lifetime.terminal_dead(site.origin, address);
        if site.next_resume.is_some() || !(self.portal_source_dead(site, address) || dead_field) {
            return false;
        }
        if low
            && let PortalOffset::Word(offset) = site.offset
            && offset.high == address
        {
            return false;
        }
        if site.cells == 1
            && let PortalOperation::Store { source } = site.operation
            && self
                .value_operand_element_location(source, 0, site.function)
                .ok()
                == self.address_location(address, site.function).ok()
        {
            return false;
        }
        true
    }

    fn consume_portal_payload(&self, site: PortalSite) -> bool {
        let PortalOperation::Store { source } = site.operation else {
            return false;
        };
        // Multi-leaf stores read a private snapshot buffer. Each leaf is sent
        // exactly once and is never observed by the source program.
        if site.cells > 1 {
            return true;
        }
        matches!(source, ValueOperand::Cell(address) if self.portal_source_dead(site, address))
    }

    pub(super) fn emit_portal_start(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.portal.start", "portal start", |emitter| {
            emitter.emit_portal_start_inner(site)
        })
    }

    pub(super) fn emit_portal_start_inner(
        &mut self,
        site: PortalSite,
    ) -> Result<(), AbiCodegenError> {
        if let PortalOperation::Store { source } = site.operation
            && site.cells > 1
        {
            let layout = self.layout(site.function)?;
            debug_assert!(layout.portal_temporary_cells >= site.cells);
            let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
            self.clear_location(restore);
            for leaf in 0..site.cells {
                let src = self.value_operand_element_location(source, leaf, site.function)?;
                let dst = self.portal_temporary_location(site.function, leaf)?;
                self.copy_locations_with_zeroed_restore(src, dst, restore);
            }
        }
        self.emit_portal_call(site)
    }

    pub(super) fn emit_portal_call(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        self.with_profile_attributes(
            "abi",
            "abi.portal.request",
            "portal request (static origin)",
            BTreeMap::from([
                ("region".into(), format!("{:?}", site.region)),
                ("function".into(), site.function.index().to_string()),
                ("cells".into(), site.cells.to_string()),
                ("leaf".into(), site.leaf.to_string()),
                (
                    "accessor_encoded".into(),
                    self.dispatch_encoding.encode(site.accessor).to_string(),
                ),
                (
                    "resume_encoded".into(),
                    self.dispatch_encoding.encode(site.resume).to_string(),
                ),
            ]),
            |emitter| emitter.emit_portal_call_inner(site),
        )
    }

    pub(super) fn emit_portal_call_inner(
        &mut self,
        site: PortalSite,
    ) -> Result<(), AbiCodegenError> {
        if let Some(router) = site.router {
            return self.stage_global_portal(site, router);
        }
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);

        for field in AbiField::ALL {
            self.clear_location(self.portal_field_location(site.region, field, site.function)?);
        }
        self.clear_location(restore);
        let offset_low = self.portal_field_location(site.region, AbiField::Index, site.function)?;
        let offset_high =
            self.portal_field_location(site.region, AbiField::Scratch0, site.function)?;
        match site.offset {
            PortalOffset::Byte(index) => {
                let source = self.address_location(index, site.function)?;
                if self.portal_offset_dead(site, index, true) {
                    self.move_location_to_zero(source, offset_low);
                } else {
                    self.copy_locations_to_zeroed_destination(source, offset_low, restore);
                }
            }
            PortalOffset::Word(offset) => {
                let low = self.address_location(offset.low, site.function)?;
                let high = self.address_location(offset.high, site.function)?;
                if self.portal_offset_dead(site, offset.low, true) {
                    self.move_location_to_zero(low, offset_low);
                } else {
                    self.copy_locations_to_zeroed_destination(low, offset_low, restore);
                }
                if self.portal_offset_dead(site, offset.high, false) {
                    self.move_location_to_zero(high, offset_high);
                } else {
                    self.copy_locations_to_zeroed_destination(high, offset_high, restore);
                }
            }
        }
        if let PortalOperation::Store { source } = site.operation {
            let value_source = if site.cells > 1 {
                self.portal_temporary_location(site.function, site.leaf)?
            } else {
                self.value_operand_element_location(source, site.leaf, site.function)?
            };
            let value_port =
                self.portal_field_location(site.region, AbiField::Value, site.function)?;
            if self.consume_portal_payload(site) {
                self.move_location_to_zero(value_source, value_port);
            } else {
                self.copy_locations_to_zeroed_destination(value_source, value_port, restore);
            }
        }
        self.set_location(
            self.portal_field_location(site.region, AbiField::Active, site.function)?,
            1,
        );
        self.set_pc_locations(
            site.region,
            site.function,
            AbiField::NextPcLow,
            AbiField::NextPcHigh,
            site.accessor,
        )?;
        self.set_pc_locations(
            site.region,
            site.function,
            AbiField::ReturnPcLow,
            AbiField::ReturnPcHigh,
            site.resume,
        )?;
        self.enter_portal(site.region, site.function)
    }

    pub(super) fn stage_global_portal(
        &mut self,
        site: PortalSite,
        router: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        let route_low = self.route_location(ROUTE_OFFSET_LOW)?;
        let route_high = self.route_location(ROUTE_OFFSET_HIGH)?;
        self.clear_location(restore);
        self.with_profile_site(
            "abi",
            "abi.portal.stage.offset",
            "abi.portal.stage.offset",
            |emitter| {
                match site.offset {
                    PortalOffset::Byte(index) => {
                        let source = emitter.address_location(index, site.function)?;
                        if emitter.portal_offset_dead(site, index, true) {
                            emitter.move_location(source, route_low);
                        } else {
                            emitter.copy_locations_with_zeroed_restore(source, route_low, restore);
                        }
                        emitter.clear_location(route_high);
                    }
                    PortalOffset::Word(offset) => {
                        let low = emitter.address_location(offset.low, site.function)?;
                        let high = emitter.address_location(offset.high, site.function)?;
                        if emitter.portal_offset_dead(site, offset.low, true) {
                            emitter.move_location(low, route_low);
                        } else {
                            emitter.copy_locations_with_zeroed_restore(low, route_low, restore);
                        }
                        if emitter.portal_offset_dead(site, offset.high, false) {
                            emitter.move_location(high, route_high);
                        } else {
                            emitter.copy_locations_with_zeroed_restore(high, route_high, restore);
                        }
                    }
                }

                Ok(())
            },
        )?;
        self.with_profile_site(
            "abi",
            "abi.portal.stage.payload",
            "abi.portal.stage.payload",
            |emitter| {
                if let PortalOperation::Store { source } = site.operation {
                    let value_source = if site.cells > 1 {
                        emitter.portal_temporary_location(site.function, site.leaf)?
                    } else {
                        emitter.value_operand_element_location(source, site.leaf, site.function)?
                    };
                    if emitter.consume_portal_payload(site) {
                        emitter.move_location(value_source, emitter.route_location(ROUTE_VALUE)?);
                    } else {
                        emitter.copy_locations_with_zeroed_restore(
                            value_source,
                            emitter.route_location(ROUTE_VALUE)?,
                            restore,
                        );
                    }
                } else {
                    emitter.clear_location(emitter.route_location(ROUTE_VALUE)?);
                }

                Ok(())
            },
        )?;
        self.with_profile_site(
            "abi",
            "abi.portal.stage.pc",
            "abi.portal.stage.pc",
            |emitter| {
                for (index, value) in [
                    (
                        ROUTE_ACCESSOR_LOW,
                        emitter.dispatch_encoding.encode(site.accessor) as u8,
                    ),
                    (
                        ROUTE_ACCESSOR_HIGH,
                        (emitter.dispatch_encoding.encode(site.accessor) >> 8) as u8,
                    ),
                    (
                        ROUTE_RESUME_LOW,
                        emitter.dispatch_encoding.encode(site.resume) as u8,
                    ),
                    (
                        ROUTE_RESUME_HIGH,
                        (emitter.dispatch_encoding.encode(site.resume) >> 8) as u8,
                    ),
                ] {
                    emitter.set_location(emitter.route_location(index)?, value);
                }

                Ok(())
            },
        )?;
        if self.fixed_context.is_some() {
            // The route is already at a known absolute position. Perform its
            // transfer directly instead of redispatching through a dynamic router.
            self.emit_global_portal_router_inner(GlobalPortalRouter {
                id: router,
                global: match site.region {
                    AggregateRegion::Global(global) => global,
                    _ => unreachable!("global route"),
                },
            })
        } else {
            self.set_next_pc(router)
        }
    }

    pub(super) fn emit_global_portal_router(
        &mut self,
        router: GlobalPortalRouter,
    ) -> Result<(), AbiCodegenError> {
        let key = format!("abi.portal.router.global.{}", router.global.index());
        self.with_profile_site("abi", &key, "global portal router", |emitter| {
            emitter.emit_global_portal_router_inner(router)
        })
    }

    pub(super) fn emit_global_portal_router_inner(
        &mut self,
        router: GlobalPortalRouter,
    ) -> Result<(), AbiCodegenError> {
        let region = AggregateRegion::Global(router.global);
        self.move_global_portal_request(region)?;
        self.set_location(
            self.portal_field_location(region, AbiField::Active, self.program.main())?,
            1,
        );
        self.enter_portal(region, self.program.main())
    }

    // Kept separate so the compact transport fixture exercises precisely the
    // production request path, with controlled bytes independent of PC layout.
    pub(super) fn move_global_portal_request(
        &mut self,
        region: AggregateRegion,
    ) -> Result<(), AbiCodegenError> {
        // A static portal starts zero and its resume path clears every protocol
        // field after each access. Route staging is single-use, so moving the
        // seven request bytes is both smaller and cheaper than seven restored
        // cross-stack copies plus sixteen redundant remote clears.
        for (route, field) in [
            (ROUTE_OFFSET_LOW, AbiField::Index),
            (ROUTE_OFFSET_HIGH, AbiField::Scratch0),
            (ROUTE_VALUE, AbiField::Value),
            (ROUTE_ACCESSOR_LOW, AbiField::NextPcLow),
            (ROUTE_ACCESSOR_HIGH, AbiField::NextPcHigh),
            (ROUTE_RESUME_LOW, AbiField::ReturnPcLow),
            (ROUTE_RESUME_HIGH, AbiField::ReturnPcHigh),
        ] {
            self.with_profile_site(
                "abi",
                format!(
                    "abi.portal.route.field.{}",
                    [
                        "offset_low",
                        "offset_high",
                        "payload",
                        "accessor_low",
                        "accessor_high",
                        "resume_low",
                        "resume_high"
                    ][route]
                ),
                "request byte",
                |emitter| {
                    let source = emitter.route_location(route)?;
                    let destination =
                        emitter.portal_field_location(region, field, emitter.program.main())?;
                    // Each nibble transport duplicates the long navigation template.
                    // Restrict it to dynamic low bytes, where the bounded crossings
                    // repay that source-size cost; high bytes and payload values use
                    // the compact unary move.
                    let use_nibbles = emitter.nibble_transfer
                        && emitter.fixed_context.is_none()
                        && matches!(
                            route,
                            ROUTE_OFFSET_LOW | ROUTE_ACCESSOR_LOW | ROUTE_RESUME_LOW
                        );
                    if use_nibbles {
                        let Location::Relative(source) = source else {
                            unreachable!("route staging is frame-relative")
                        };
                        let Location::Global(destination) = destination else {
                            unreachable!("global portal fields are static")
                        };
                        emitter.move_relative_to_global_nibbles(source, destination)?;
                    } else {
                        emitter.with_profile_site_infallible(
                            "abi",
                            "abi.portal.route.transport.unary",
                            "unary transport",
                            |emitter| {
                                emitter.move_location_to_zero(source, destination);
                            },
                        );
                    }
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    pub(super) fn emit_aggregate_accessor(
        &mut self,
        accessor: PortalAccessor,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.portal.accessor", "portal accessor", |emitter| {
            emitter.emit_aggregate_accessor_inner(accessor)
        })
    }

    pub(super) fn emit_aggregate_accessor_inner(
        &mut self,
        accessor: PortalAccessor,
    ) -> Result<(), AbiCodegenError> {
        // A byte-sized logical offset can be dispatched directly without
        // moving the portal context. Larger offsets use the page portal
        // resolver, which moves only the request fields between page-local
        // protocol prefixes.
        self.emit_direct_or_page_accessor(accessor.kind)?;
        for field in [
            AbiField::Index,
            AbiField::Condition,
            AbiField::Restore,
            AbiField::Branch,
            AbiField::Scratch0,
            AbiField::Scratch1,
            AbiField::Scratch2,
            AbiField::Scratch3,
        ] {
            self.clear_abi_field(field)?;
        }
        Ok(())
    }

    pub(super) fn emit_direct_or_page_accessor(
        &mut self,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        self.emit_portal_direct_test()?;

        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let direct = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.emit_direct_payload_countdown(kind)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(direct);
        self.move_to(0);

        // Condition is one for the direct path and zero for the page path.
        // Starting with Branch=1 and clearing it when Condition is nonzero
        // therefore selects the page resolver only for a nonzero high byte.
        self.set_abi_field(AbiField::Branch, 1)?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;
        self.move_to(branch);
        let page = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.emit_page_portal_accessor(kind)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(page);
        self.move_to(0);
        Ok(())
    }

    pub(super) fn emit_portal_direct_test(&mut self) -> Result<(), AbiCodegenError> {
        // Scratch0 holds the high byte, which the page resolver still needs.
        // Its adjacent Scratch1/2 are private until that resolver splits it.
        // Use the two possible loop exits instead of saving/restoring high.
        for field in [AbiField::Condition, AbiField::Restore, AbiField::Branch] {
            self.clear_abi_field(field)?;
        }
        let source = self.current_abi_offset(AbiField::Scratch0)?;
        let flag = self.current_abi_offset(AbiField::Scratch1)?;
        let zero = self.current_abi_offset(AbiField::Scratch2)?;
        self.emit_preserving_zero_test(source, flag, zero);
        self.move_to(flag);
        let direct = self.capture(|emitter| {
            emitter.adjust(255);
            // Keep the decision across the first guarded body, which consumes
            // Branch. The second guarded body observes its inverse.
            emitter.add_abi_field(AbiField::Branch, 1)?;
            emitter.add_abi_field(AbiField::Condition, 1)?;
            emitter.move_to(flag);
            Ok(())
        })?;
        self.emit_loop(direct);
        self.move_to(0);
        Ok(())
    }

    pub(super) fn emit_direct_payload_countdown(
        &mut self,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            "abi.portal.payload.direct",
            "direct payload countdown",
            |emitter| {
                emitter.set_abi_field(AbiField::Branch, 1)?;
                emitter.emit_direct_payload_countdown_level(0, u8::MAX, kind)
            },
        )
    }

    pub(super) fn emit_direct_payload_countdown_level(
        &mut self,
        level: u8,
        maximum: u8,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        let index = self.current_abi_offset(AbiField::Index)?;
        self.move_to(index);
        let body = self.capture(|emitter| {
            if level == maximum {
                emitter.clear_current();
                emitter.clear_abi_field(AbiField::Branch)?;
            } else {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_direct_payload_countdown_level(level + 1, maximum, kind)?;
            }
            emitter.move_to(index);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        self.emit_direct_payload_countdown_case(level, kind)
    }

    pub(super) fn emit_direct_payload_countdown_case(
        &mut self,
        element: u8,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            let payload = emitter
                .config
                .logical_offset_from_head(PROTOCOL_CELLS + usize::from(element))
                as isize;
            let value = emitter.current_abi_offset(AbiField::Value)?;
            match kind {
                PortalAccessKind::Load => {
                    let scratch = emitter.current_abi_offset(AbiField::Scratch2)?;
                    emitter.copy(payload, value, scratch);
                }
                PortalAccessKind::Store => emitter.move_value(value, payload),
            }
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn abi_field_offsets(
        &self,
        fields: [AbiField; 8],
    ) -> Result<[isize; 8], AbiCodegenError> {
        let mut offsets = [0; 8];
        for (offset, field) in offsets.iter_mut().zip(fields) {
            *offset = self.current_abi_offset(field)?;
        }
        Ok(offsets)
    }

    #[cfg(test)]
    pub(super) fn decompose_abi_byte(
        &mut self,
        source: AbiField,
        bits: &[isize; 8],
        temporary: isize,
    ) -> Result<(), AbiCodegenError> {
        self.clear(temporary);
        for &bit in bits {
            self.clear(bit);
        }
        let source = self.current_abi_offset(source)?;
        self.move_to(source);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.increment_bits(bits, temporary, 0, source);
        });
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    /// Consume a byte with a bounded countdown. Each entered level updates
    /// the result directly; after the source reaches zero, all enclosing
    /// loops exit without touching it. No binary carry scratch cells are needed.
    pub(super) fn split_abi_nibbles(
        &mut self,
        source: AbiField,
        low: AbiField,
        high: AbiField,
    ) -> Result<(), AbiCodegenError> {
        let source = self.current_abi_offset(source)?;
        let low = self.current_abi_offset(low)?;
        let high = self.current_abi_offset(high)?;
        self.clear(low);
        self.clear(high);
        self.split_nibble_level(source, low, high, 1);
        self.move_to(0);
        Ok(())
    }

    pub(super) fn split_nibble_level(&mut self, source: isize, low: isize, high: isize, level: u8) {
        self.move_to(source);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(low);
            if level.is_multiple_of(16) {
                emitter.adjust(0_u8.wrapping_sub(15));
                emitter.move_to(high);
                emitter.adjust(1);
            } else {
                emitter.adjust(1);
            }
            if level < u8::MAX {
                emitter.split_nibble_level(source, low, high, level + 1);
            }
            emitter.move_to(source);
        });
        self.emit_loop(body);
    }

    pub(super) fn emit_portal_resume(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.portal.resume", "portal resume", |emitter| {
            emitter.emit_portal_resume_inner(site)
        })
    }

    pub(super) fn emit_portal_resume_inner(
        &mut self,
        site: PortalSite,
    ) -> Result<(), AbiCodegenError> {
        let is_load = matches!(site.operation, PortalOperation::Load { .. });
        self.with_profile_site(
            "abi",
            "abi.portal.resume.clear",
            "abi.portal.resume.clear",
            |emitter| {
                for field in AbiField::ALL {
                    if is_load && field == AbiField::Value {
                        continue;
                    }
                    emitter.clear_abi_field(field)?;
                }

                Ok(())
            },
        )?;
        let frame = self.layout(site.function)?.frame.clone();
        self.with_profile_site(
            "abi",
            "abi.portal.resume.transport",
            "abi.portal.resume.transport",
            |emitter| {
                match site.region {
                    AggregateRegion::Frame(aggregate) => {
                        let context_delta = -frame.aggregate_base_offset(aggregate)?;
                        if is_load {
                            let source = emitter.current_abi_offset(AbiField::Value)?;
                            let destination = context_delta + frame.abi_offset(AbiField::Value);
                            emitter.move_value(source, destination);
                        }
                        emitter.clear_abi_field(AbiField::Value)?;
                        emitter.migrate_context(context_delta);
                    }
                    AggregateRegion::Global(global) => {
                        let base = emitter.static_layout.aggregate_base_head(global)?;
                        if is_load {
                            emitter.move_global_portal_value_to_context(
                                base,
                                frame.abi_offset(AbiField::Value),
                            )?;
                        } else {
                            emitter.clear_abi_field(AbiField::Value)?;
                            emitter.emit_global_to_context(base, frame.context_chunks());
                        }
                    }
                    AggregateRegion::Outbox => {
                        unreachable!("outbox cannot use the aggregate portal")
                    }
                }

                Ok(())
            },
        )?;
        self.with_profile_site(
            "abi",
            "abi.portal.resume.deliver",
            "abi.portal.resume.deliver",
            |emitter| {
                if let PortalOperation::Load { destination } = site.operation {
                    let source = Location::Relative(frame.abi_offset(AbiField::Value));
                    let destination = if site.cells > 1 {
                        emitter.portal_temporary_location(site.function, site.leaf)?
                    } else {
                        emitter.value_operand_element_location(
                            destination,
                            site.leaf,
                            site.function,
                        )?
                    };
                    emitter.move_location(source, destination);
                }

                Ok(())
            },
        )?;
        if let Some(next_resume) = site.next_resume {
            self.with_profile_site(
                "abi",
                "abi.portal.resume.advance",
                "abi.portal.resume.advance",
                |emitter| {
                    emitter.increment_portal_offset(site.offset, site.function)?;

                    Ok(())
                },
            )?;
            let next = *self
                .portal
                .ordered_sites
                .iter()
                .find(|candidate| candidate.resume == next_resume)
                .expect("portal leaf resume chain must be complete");
            self.emit_portal_call(next)
        } else {
            if let PortalOperation::Load { destination } = site.operation
                && site.cells > 1
            {
                for leaf in 0..site.cells {
                    let source = self.portal_temporary_location(site.function, leaf)?;
                    let destination =
                        self.value_operand_element_location(destination, leaf, site.function)?;
                    self.move_location(source, destination);
                }
            }
            // Store staging and final load delivery consume every private
            // buffer leaf, so the buffer is already zero for the next request.
            self.set_next_pc(site.return_to)
        }
    }

    pub(super) fn increment_portal_offset(
        &mut self,
        offset: PortalOffset,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let PortalOffset::Word(offset) = offset else {
            unreachable!("single-cell Version-0 access never increments its byte index")
        };
        let low = self.address_location(offset.low, function)?;
        let high = self.address_location(offset.high, function)?;
        let condition = Location::Relative(self.current_abi_offset(AbiField::Condition)?);
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        let branch = Location::Relative(self.current_abi_offset(AbiField::Branch)?);
        self.copy_locations(low, condition, restore);
        self.move_context_to_location(condition);
        self.adjust(1);
        self.move_location_to_context(condition);
        self.set_location(branch, 1);
        self.move_context_to_location(condition);
        let nonzero = self.capture_infallible(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(condition);
            emitter.clear_location(branch);
            emitter.move_context_to_location(condition);
        });
        self.emit_loop(nonzero);
        self.move_location_to_context(condition);

        self.move_context_to_location(low);
        self.adjust(1);
        self.move_location_to_context(low);
        self.move_context_to_location(branch);
        let carry = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(branch);
            emitter.move_context_to_location(high);
            emitter.adjust(1);
            emitter.move_location_to_context(high);
            emitter.move_context_to_location(branch);
        });
        self.emit_loop(carry);
        self.move_location_to_context(branch);
        Ok(())
    }

    pub(super) fn move_global_portal_value_to_context(
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
        if self.nibble_transfer && self.fixed_context.is_none() {
            self.global_to_relative_nibbles(base, value, destination, false);
            return Ok(());
        }
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
        self.emit_loop(body);
        self.move_to(0);
        self.emit_global_to_context(base, context_chunks);
        Ok(())
    }

    pub(super) fn set_pc_locations(
        &mut self,
        array: AggregateRegion,
        function: FunctionId,
        low: AbiField,
        high: AbiField,
        value: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let value = self.dispatch_encoding.encode(value);
        self.set_location(
            self.portal_field_location(array, low, function)?,
            value as u8,
        );
        self.set_location(
            self.portal_field_location(array, high, function)?,
            (value >> 8) as u8,
        );
        Ok(())
    }

    pub(super) fn enter_portal(
        &mut self,
        array: AggregateRegion,
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

    pub(super) fn portal_base_location(
        &self,
        array: AggregateRegion,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            AggregateRegion::Frame(aggregate) => Location::Relative(
                self.layout(function)?
                    .frame
                    .aggregate_base_offset(aggregate)?,
            ),
            AggregateRegion::Global(global) => {
                Location::Global(self.static_layout.aggregate_base_head(global)?)
            }
            AggregateRegion::Outbox => {
                unreachable!("validated portal aggregate cannot be outbox")
            }
        })
    }

    pub(super) fn portal_field_location(
        &self,
        array: AggregateRegion,
        field: AbiField,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            AggregateRegion::Frame(aggregate) => Location::Relative(
                self.layout(function)?
                    .frame
                    .aggregate_portal_offset(aggregate, field)?,
            ),
            AggregateRegion::Global(global) => Location::Global(
                self.static_layout
                    .aggregate_portal_field_position(global, field)?,
            ),
            AggregateRegion::Outbox => {
                unreachable!("validated portal aggregate cannot be outbox")
            }
        })
    }

    pub(super) fn portal_temporary_location(
        &self,
        function: FunctionId,
        index: usize,
    ) -> Result<Location, AbiCodegenError> {
        let layout = self.layout(function)?;
        debug_assert!(index < layout.portal_temporary_cells);
        Ok(Location::Relative(layout.frame.frame_offset(
            FrameSlot::new(layout.portal_temporary_start + index),
        )))
    }

    pub(super) fn route_location(&self, index: usize) -> Result<Location, AbiCodegenError> {
        Ok(Location::Relative(
            self.layout(self.program.main())?
                .frame
                .route_offset(index)?,
        ))
    }
}
