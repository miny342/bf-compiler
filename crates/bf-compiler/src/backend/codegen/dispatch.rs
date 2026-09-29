//! Continuation dispatch encoding, selection, and entry initialization.

use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum DispatchEntry<'a> {
    Continuation(&'a Continuation),
    PortalAccessor(PortalAccessor),
    PortalResume(PortalSite),
    GlobalPortalRouter(GlobalPortalRouter),
    StaticResume(StaticResume),
}

impl DispatchEntry<'_> {
    pub(super) fn id(self) -> ContinuationId {
        match self {
            Self::Continuation(continuation) => continuation.id(),
            Self::PortalAccessor(accessor) => accessor.id,
            Self::PortalResume(site) => site.resume,
            Self::GlobalPortalRouter(router) => router.id,
            Self::StaticResume(resume) => resume.id,
        }
    }
}

/// Maps stable logical continuation IDs to the bytes stored in ABI PC fields.
/// Dense programs balance the two countdown levels instead of filling 256-entry
/// low-byte pages first. Sparse public IR retains its original page geometry.
/// Portal entries receive the first dense ranks to reduce PC transport costs.
/// Reverse pages where it further lowers their aggregate countdown distance.
#[derive(Debug)]
pub(super) struct DispatchEncoding {
    pub(super) reversed_low_pages: HashMap<u8, (u8, u8)>,
    pub(super) page_width: u16,
    pub(super) dense_ranks: Vec<u16>,
}

impl DispatchEncoding {
    pub(super) fn new(program: &ContinuationProgram, portal: &PortalPlan) -> Self {
        Self::with_regions(program, portal, None)
    }

    pub(super) fn with_regions(
        program: &ContinuationProgram,
        portal: &PortalPlan,
        regions: Option<&RegionPlan>,
    ) -> Self {
        Self::with_fixed_frames(program, portal, regions, None)
    }

    pub(super) fn with_fixed_frames(
        program: &ContinuationProgram,
        portal: &PortalPlan,
        regions: Option<&RegionPlan>,
        fixed: Option<&StaticFramePlan>,
    ) -> Self {
        let mut entries = program
            .continuations()
            .iter()
            .filter(|c| regions.is_none_or(|plan| plan.entries.contains(&c.id())))
            .map(|c| (c.id(), false))
            .chain(portal.accessors.iter().map(|a| (a.id, true)))
            .chain(portal.ordered_sites.iter().map(|s| (s.resume, true)))
            .chain(portal.routers.iter().map(|r| (r.id, true)))
            .chain(
                fixed
                    .into_iter()
                    .flat_map(|p| p.extra_resumes.iter())
                    .map(|r| (r.id, false)),
            )
            .collect::<Vec<_>>();
        let count = entries.len();
        let maximum = entries.iter().map(|(id, _)| id.get()).max().unwrap_or(1);
        // IDs are nonzero and unique, including hidden portal IDs. Equality
        // therefore proves that 1..=maximum is dense. Balancing p + n/p
        // bounds both linear countdown levels by approximately sqrt(n).
        // Include the unused zero code when choosing the width so that both
        // encoded bytes fit, even for a program with all 65,535 IDs occupied.
        // Region emission removes soft-only entries without rewriting semantic
        // IDs. Compact those remaining entries in the private PC encoding too.
        let dense = regions.is_some() || (count == usize::from(maximum) && count > 256);
        let page_width = if dense && count > 256 {
            (1_u16..=256)
                .find(|&width| usize::from(width).pow(2) > count)
                .unwrap_or(256)
        } else {
            256
        };
        let mut dense_ranks = Vec::new();
        if dense {
            // Portal PCs cross global/frame boundaries by value. Keep their
            // byte values small to reduce navigation while transferring them,
            // as well as the countdown needed to dispatch them.
            entries.sort_unstable_by_key(|&(id, hidden)| (!hidden, id.get()));
            dense_ranks.resize(usize::from(maximum) + 1, 0);
            for (index, &(id, _)) in entries.iter().enumerate() {
                dense_ranks[usize::from(id.get())] = (index + 1) as u16;
            }
        }
        let mut pages = BTreeMap::<u8, (Vec<u8>, Vec<u8>)>::new();
        for (id, hidden) in entries {
            let rank = dense_ranks
                .get(usize::from(id.get()))
                .copied()
                .unwrap_or(id.get());
            let value = (rank / page_width) * 256 + rank % page_width;
            let page = pages.entry((value >> 8) as u8).or_default();
            page.0.push(value as u8);
            if hidden {
                page.1.push(value as u8);
            }
        }

        let reversed_low_pages = pages
            .into_iter()
            .filter_map(|(high, (lows, hidden))| {
                if hidden.is_empty() {
                    return None;
                }
                let minimum = *lows.iter().min().expect("dispatch page is nonempty");
                let maximum = *lows.iter().max().expect("dispatch page is nonempty");
                let ascending_cost = hidden
                    .iter()
                    .map(|&low| usize::from(low - minimum))
                    .sum::<usize>();
                let descending_cost = hidden
                    .iter()
                    .map(|&low| usize::from(maximum - low))
                    .sum::<usize>();
                (descending_cost < ascending_cost).then_some((high, (minimum, maximum)))
            })
            .collect();
        Self {
            reversed_low_pages,
            page_width,
            dense_ranks,
        }
    }

    pub(super) fn encode(&self, id: ContinuationId) -> u16 {
        let rank = self
            .dense_ranks
            .get(usize::from(id.get()))
            .copied()
            .unwrap_or(id.get());
        let value = (rank / self.page_width) * 256 + rank % self.page_width;
        let high = (value >> 8) as u8;
        let low = value as u8;
        let encoded_low = self
            .reversed_low_pages
            .get(&high)
            .map_or(low, |&(minimum, maximum)| minimum + (maximum - low));
        (u16::from(high) << 8) | u16::from(encoded_low)
    }
}

impl<'a> AbiEmitter<'a> {
    pub(super) fn initialize_main(&mut self, check_capacity: bool) -> Result<(), AbiCodegenError> {
        if self.fixed.is_some() {
            return self.initialize_fixed_main(check_capacity);
        }
        let function = self.function(self.program.main())?;
        let function_id = function.id();
        let entry = function.entry();
        let frame = self.layout(function_id)?.frame.clone();
        if check_capacity {
            frame.validate_main_capacity(self.static_layout.anchor_head())?;
        }

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

    pub(super) fn emit_dispatcher(&mut self) -> Result<(), AbiCodegenError> {
        let body = self.capture(|emitter| {
            emitter.move_to(0);
            let mut pages = BTreeMap::<u8, Vec<DispatchEntry<'_>>>::new();
            for continuation in emitter.program.continuations() {
                if emitter
                    .regions
                    .is_some_and(|plan| !plan.entries.contains(&continuation.id()))
                {
                    continue;
                }
                let encoded = emitter.dispatch_encoding.encode(continuation.id());
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::Continuation(continuation));
            }
            for &accessor in &emitter.portal.accessors {
                let encoded = emitter.dispatch_encoding.encode(accessor.id);
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::PortalAccessor(accessor));
            }
            for &site in &emitter.portal.ordered_sites {
                let encoded = emitter.dispatch_encoding.encode(site.resume);
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::PortalResume(site));
            }
            for &router in &emitter.portal.routers {
                let encoded = emitter.dispatch_encoding.encode(router.id);
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::GlobalPortalRouter(router));
            }
            if let Some(fixed) = emitter.fixed {
                for &resume in &fixed.extra_resumes {
                    let encoded = emitter.dispatch_encoding.encode(resume.id);
                    pages
                        .entry((encoded >> 8) as u8)
                        .or_default()
                        .push(DispatchEntry::StaticResume(resume));
                }
            }
            let pages = pages
                .into_iter()
                .map(|(high, mut entries)| {
                    // A dispatched body can migrate to a fresh context whose PcLow
                    // is zero until the end of this dispatcher cycle. Keeping the
                    // zero case first prevents that fresh context from being
                    // mistaken for continuation xx00 later in the same page.
                    entries.sort_by_key(|entry| emitter.dispatch_encoding.encode(entry.id()) as u8);
                    (high, entries)
                })
                .collect::<Vec<_>>();
            let high_span = usize::from(
                pages.last().expect("nonempty dispatcher").0
                    - pages.first().expect("nonempty dispatcher").0,
            ) + 1;
            if high_span == pages.len() {
                emitter.with_profile_site(
                    "abi",
                    "abi.dispatch.pages.countdown",
                    "dispatch page countdown",
                    |emitter| emitter.emit_page_countdown(&pages),
                )?;
            } else {
                emitter.with_profile_site(
                    "abi",
                    "abi.dispatch.pages.compare",
                    "dispatch page equality scan",
                    |emitter| {
                        for (high, entries) in &pages {
                            emitter.emit_dispatch_page(*high, entries)?;
                        }
                        Ok(())
                    },
                )?;
            }
            emitter.move_abi_field(AbiField::NextPcLow, AbiField::PcLow);
            emitter.move_abi_field(AbiField::NextPcHigh, AbiField::PcHigh);
            let active = emitter.current_abi_offset(AbiField::Active)?;
            emitter.move_to(active);
            Ok(())
        })?;
        self.emit_loop(body);
        Ok(())
    }

    pub(super) fn emit_dispatch_page(
        &mut self,
        high: u8,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            format!("abi.dispatch.page.{high}"),
            format!("dispatch page {high}"),
            |emitter| emitter.emit_dispatch_page_inner(high, entries),
        )
    }

    pub(super) fn emit_dispatch_page_inner(
        &mut self,
        high: u8,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            "abi.dispatch.page.select",
            "dispatch page selector",
            |emitter| {
                emitter.copy_abi_field(AbiField::PcHigh, AbiField::Condition)?;
                emitter.add_abi_field(AbiField::Condition, 0_u8.wrapping_sub(high))?;
                emitter.set_abi_field(AbiField::Branch, 1)?;
                emitter.clear_branch_on_nonzero(AbiField::Condition)
            },
        )?;

        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            // Once a page matches, make every later equality page fail.
            emitter.clear_abi_field(AbiField::PcHigh)?;
            emitter.emit_dispatch_page_body(entries)?;
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    pub(super) fn emit_page_countdown(
        &mut self,
        pages: &[(u8, Vec<DispatchEntry<'a>>)],
    ) -> Result<(), AbiCodegenError> {
        let minimum = pages.first().expect("nonempty dispatcher").0;
        let maximum = pages.last().expect("nonempty dispatcher").0;
        let mut cases = [None; 256];
        for (index, (high, _)) in pages.iter().enumerate() {
            cases[usize::from(high.wrapping_sub(minimum))] = Some(index);
        }
        self.add_abi_field(AbiField::PcHigh, 0_u8.wrapping_sub(minimum))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.emit_page_countdown_level(0, maximum - minimum, &cases, pages)
    }

    pub(super) fn emit_page_countdown_level(
        &mut self,
        level: u8,
        maximum: u8,
        cases: &[Option<usize>; 256],
        pages: &[(u8, Vec<DispatchEntry<'a>>)],
    ) -> Result<(), AbiCodegenError> {
        let pc = self.current_abi_offset(AbiField::PcHigh)?;
        self.move_to(pc);
        let nonzero = self.capture(|emitter| {
            if level == maximum {
                emitter.clear_current();
                emitter.clear_abi_field(AbiField::Branch)?;
            } else {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_page_countdown_level(level + 1, maximum, cases, pages)?;
            }
            emitter.move_to(pc);
            Ok(())
        })?;
        self.emit_loop(nonzero);
        self.move_to(0);

        if let Some(index) = cases[usize::from(level)] {
            let (high, entries) = &pages[index];
            self.with_profile_site(
                "abi",
                format!("abi.dispatch.page.{high}"),
                format!("dispatch page {high}"),
                |emitter| emitter.emit_page_countdown_case(Some(entries)),
            )
        } else {
            self.emit_page_countdown_case(None)
        }
    }

    pub(super) fn emit_page_countdown_case(
        &mut self,
        entries: Option<&[DispatchEntry<'a>]>,
    ) -> Result<(), AbiCodegenError> {
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            if let Some(entries) = entries {
                emitter.emit_dispatch_page_body(entries)?;
            }
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    pub(super) fn emit_dispatch_page_body(
        &mut self,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        let low_span = usize::from(
            (self
                .dispatch_encoding
                .encode(entries.last().expect("nonempty dispatch page").id()) as u8)
                - (self
                    .dispatch_encoding
                    .encode(entries.first().expect("nonempty dispatch page").id())
                    as u8),
        ) + 1;
        if low_span == entries.len() {
            self.with_profile_site(
                "abi",
                "abi.dispatch.page.countdown",
                "dispatch page countdown",
                |emitter| emitter.emit_countdown_dispatch(entries),
            )
        } else {
            self.with_profile_site(
                "abi",
                "abi.dispatch.page.compare",
                "dispatch page equality scan",
                |emitter| emitter.emit_equality_dispatch(entries),
            )
        }
    }

    pub(super) fn emit_countdown_dispatch(
        &mut self,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        let minimum = entries
            .first()
            .map(|entry| self.dispatch_encoding.encode(entry.id()) as u8)
            .expect("every dispatch page contains at least one entry");
        let maximum = entries
            .last()
            .map(|entry| self.dispatch_encoding.encode(entry.id()) as u8)
            .expect("every dispatch page contains at least one entry");
        let mut cases = [None; 256];
        for &entry in entries {
            let relative = (self.dispatch_encoding.encode(entry.id()) as u8).wrapping_sub(minimum);
            cases[usize::from(relative)] = Some(entry);
        }
        // Normalize the page's occupied low-byte range to zero. Values below
        // `minimum` wrap above the range, so sparse or invalid PCs still reach
        // the no-match path rather than aliasing a case.
        self.add_abi_field(AbiField::PcLow, 0_u8.wrapping_sub(minimum))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.emit_countdown_level(0, maximum - minimum, &cases)
    }

    pub(super) fn emit_equality_dispatch(
        &mut self,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        for &entry in entries {
            self.with_profile_site(
                "abi",
                format!("abi.dispatch.case.{}", entry.id().get()),
                format!("dispatch case {}", entry.id().get()),
                |emitter| emitter.emit_equality_case(entry),
            )?;
        }
        Ok(())
    }

    pub(super) fn emit_equality_case(
        &mut self,
        entry: DispatchEntry<'a>,
    ) -> Result<(), AbiCodegenError> {
        let id = self.dispatch_encoding.encode(entry.id());
        self.copy_abi_field(AbiField::PcLow, AbiField::Condition)?;
        self.add_abi_field(AbiField::Condition, 0_u8.wrapping_sub(id as u8))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;

        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.clear_abi_field(AbiField::PcLow)?;
            emitter.emit_dispatch_entry_body(entry)?;
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_dispatch_entry_gate(entry.id(), body)?;
        self.move_to(0);
        Ok(())
    }

    pub(super) fn emit_countdown_level(
        &mut self,
        level: u8,
        maximum: u8,
        cases: &[Option<DispatchEntry<'a>>; 256],
    ) -> Result<(), AbiCodegenError> {
        let pc = self.current_abi_offset(AbiField::PcLow)?;
        self.move_to(pc);
        let nonzero = self.capture(|emitter| {
            if level == maximum {
                // No case can match above the last low byte in this page.
                emitter.clear_current();
                emitter.clear_abi_field(AbiField::Branch)?;
            } else {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_countdown_level(level + 1, maximum, cases)?;
            }
            emitter.move_to(pc);
            Ok(())
        })?;
        self.emit_loop(nonzero);
        self.move_to(0);

        let entry = cases[usize::from(level)];
        if let Some(entry) = entry {
            self.with_profile_site(
                "abi",
                format!("abi.dispatch.case.{}", entry.id().get()),
                format!("dispatch case {}", entry.id().get()),
                |emitter| emitter.emit_countdown_case(Some(entry)),
            )
        } else {
            self.emit_countdown_case(None)
        }
    }

    pub(super) fn emit_countdown_case(
        &mut self,
        entry: Option<DispatchEntry<'a>>,
    ) -> Result<(), AbiCodegenError> {
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            if let Some(entry) = entry {
                emitter.emit_dispatch_entry_body(entry)?;
            }
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        if let Some(entry) = entry {
            self.emit_dispatch_entry_gate(entry.id(), body)?;
        } else {
            self.emit_loop(body);
        }
        self.move_to(0);
        Ok(())
    }

    /// Attribute only the one-shot gate brackets, so loop iterations measure
    /// actual BF dispatcher visits independently of the emitted body shape.
    pub(super) fn emit_dispatch_entry_gate(
        &mut self,
        id: ContinuationId,
        body: Vec<AnnotatedBfInstruction>,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            format!("abi.dispatch.enter.{}", id.get()),
            "dispatcher entry",
            |emitter| {
                emitter.emit_loop(body);
                Ok(())
            },
        )
    }

    pub(super) fn emit_dispatch_entry_body(
        &mut self,
        entry: DispatchEntry<'a>,
    ) -> Result<(), AbiCodegenError> {
        let previous = self.fixed_context;
        let result = self.emit_dispatch_entry_body_inner(entry);
        self.fixed_context = previous;
        result
    }

    pub(super) fn emit_dispatch_entry_body_inner(
        &mut self,
        entry: DispatchEntry<'a>,
    ) -> Result<(), AbiCodegenError> {
        match entry {
            DispatchEntry::Continuation(continuation) => {
                if let Some(resume) = self
                    .fixed
                    .and_then(|p| p.resumes.get(&continuation.id()))
                    .copied()
                {
                    return self.emit_static_resume(resume);
                }
                self.fixed_context = self
                    .fixed
                    .and_then(|p| p.contexts.get(&continuation.function()))
                    .copied();
                self.emit_function_entry(continuation)
            }
            DispatchEntry::StaticResume(resume) => self.emit_static_resume(resume),
            DispatchEntry::PortalAccessor(accessor) => {
                self.fixed_context = None;
                self.emit_aggregate_accessor(accessor)?;
                self.move_abi_field(AbiField::ReturnPcLow, AbiField::NextPcLow);
                self.move_abi_field(AbiField::ReturnPcHigh, AbiField::NextPcHigh);
                Ok(())
            }
            DispatchEntry::PortalResume(site) => {
                self.fixed_context = self
                    .fixed
                    .and_then(|p| p.contexts.get(&site.function))
                    .copied();
                self.emit_portal_resume(site)
            }
            DispatchEntry::GlobalPortalRouter(router) => {
                self.fixed_context = None;
                self.emit_global_portal_router(router)
            }
        }
    }

    pub(super) fn emit_function_entry(
        &mut self,
        continuation: &'a Continuation,
    ) -> Result<(), AbiCodegenError> {
        if let Some(region) = self
            .regions
            .and_then(|plan| plan.regions.get(&continuation.id()))
        {
            self.emit_region(region, continuation.function())
        } else {
            self.emit_continuation_body(continuation)
        }
    }

    pub(super) fn emit_continuation_body(
        &mut self,
        continuation: &Continuation,
    ) -> Result<(), AbiCodegenError> {
        self.with_continuation_site(continuation, |emitter| {
            emitter.emit_continuation_body_inner(continuation)
        })
    }

    pub(super) fn emit_continuation_body_inner(
        &mut self,
        continuation: &Continuation,
    ) -> Result<(), AbiCodegenError> {
        self.branch_temporary_depth = 0;
        self.emit_all(
            continuation.body(),
            continuation.function(),
            Some(continuation.body_sources()),
        )?;
        self.with_source_span(continuation.terminator_source(), |emitter| {
            emitter.emit_terminator(continuation)
        })
    }

    pub(super) fn clear_branch_on_nonzero(
        &mut self,
        condition: AbiField,
    ) -> Result<(), AbiCodegenError> {
        let condition = self.current_abi_offset(condition)?;
        self.move_to(condition);
        let body = self.capture(|emitter| {
            emitter.clear_current();
            emitter.clear_abi_field(AbiField::Branch)?;
            emitter.move_to(condition);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }
}
