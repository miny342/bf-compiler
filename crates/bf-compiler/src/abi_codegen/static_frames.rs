//! Experimental fixed activations for nonrecursive functions. Recursive SCCs
//! keep the dynamic stack. A static return inbox bridges the two representations.
//! No function bodies or storage are shared by this pass, and CIR is unchanged.
use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct StaticResume {
    pub id: ContinuationId,
    target: ContinuationId,
    cells: usize,
}

#[derive(Debug)]
pub(super) struct StaticFramePlan {
    pub contexts: HashMap<FunctionId, usize>,
    pub resumes: HashMap<ContinuationId, StaticResume>,
    pub extra_resumes: Vec<StaticResume>,
    return_ids: HashMap<(ContinuationId, usize), ContinuationId>,
    inbox: FrameLayout,
    inbox_context: usize,
    /// Recursive callees entered from a fixed caller need a return-route bit.
    hub_callees: HashSet<FunctionId>,
    static_cells: usize,
    dynamic_functions: usize,
}

fn result_cells(value_type: ValueType) -> usize {
    match value_type {
        ValueType::Array(cells) | ValueType::Aggregate { cells } => cells,
        ValueType::Cell | ValueType::Void => 0,
    }
}

impl StaticFramePlan {
    pub fn new(
        program: &ContinuationProgram,
        layouts: &HashMap<FunctionId, FunctionLayout>,
        portal: &PortalPlan,
        storage: &mut StaticLayout,
        check_capacity: bool,
    ) -> Result<Self, AbiCodegenError> {
        let recursive = crate::continuation_inline::recursive_functions(program);
        let static_start = storage.anchor_head();
        let config = storage.config();
        let mut contexts = HashMap::new();
        for function in program.functions() {
            if recursive.contains(&function.id()) {
                continue;
            }
            let frame = &layouts[&function.id()].frame;
            let cells = frame
                .frame_chunks()
                .checked_mul(config.stride())
                .ok_or(FrameLayoutError::SizeOverflow)?;
            let bottom = storage.reserve_internal_cells(cells)?;
            contexts.insert(
                function.id(),
                bottom + cells - frame.context_chunks() * config.stride(),
            );
        }
        let cells = program
            .functions()
            .iter()
            .map(|f| result_cells(f.return_type()))
            .max()
            .unwrap_or(0);
        let inbox = FrameLayout::new(config, 0, cells)?;
        let size = inbox
            .frame_chunks()
            .checked_mul(config.stride())
            .ok_or(FrameLayoutError::SizeOverflow)?;
        let bottom = storage.reserve_internal_cells(size)?;
        let inbox_context = bottom + size - inbox.context_chunks() * config.stride();
        if check_capacity {
            storage.validate_capacity()?;
            if !contexts.contains_key(&program.main()) {
                layouts[&program.main()]
                    .frame
                    .validate_main_capacity(storage.anchor_head())?;
            }
        }

        // Reuse the original dispatcher entry when it is reached exclusively
        // by Calls with the same result width. Shared soft/portal/function
        // entries need a separate gate, but never an extra dispatcher visit.
        let mut ordinary = program
            .functions()
            .iter()
            .map(|f| f.entry())
            .collect::<HashSet<_>>();
        let mut widths = HashMap::<ContinuationId, HashSet<usize>>::new();
        let mut hub_callees = HashSet::new();
        for c in program.continuations() {
            if let Terminator::Call {
                callee, return_to, ..
            } = c.terminator()
            {
                if !contexts.contains_key(&c.function()) && !contexts.contains_key(callee) {
                    ordinary.insert(*return_to);
                    continue;
                }
                hub_callees.insert(*callee);
                widths.entry(*return_to).or_default().insert(result_cells(
                    program.function(*callee).unwrap().return_type(),
                ));
            } else {
                ordinary.extend(c.terminator().edges().map(|(id, _)| id));
            }
        }
        let mut used = program
            .continuations()
            .iter()
            .map(|c| c.id().get())
            .chain(portal.accessors.iter().map(|a| a.id.get()))
            .chain(portal.ordered_sites.iter().map(|s| s.resume.get()))
            .chain(portal.routers.iter().map(|r| r.id.get()))
            .collect::<HashSet<_>>();
        let mut resumes = HashMap::new();
        let mut extra_resumes = Vec::new();
        let mut return_ids = HashMap::new();
        for c in program.continuations() {
            let Terminator::Call {
                callee, return_to, ..
            } = c.terminator()
            else {
                continue;
            };
            if !contexts.contains_key(&c.function()) && !contexts.contains_key(callee) {
                continue;
            }
            let cells = result_cells(program.function(*callee).unwrap().return_type());
            let key = (*return_to, cells);
            if return_ids.contains_key(&key) {
                continue;
            }
            let id = if !ordinary.contains(return_to) && widths[return_to].len() == 1 {
                *return_to
            } else {
                allocate_hidden_id(&mut used)?
            };
            let resume = StaticResume {
                id,
                target: *return_to,
                cells,
            };
            if id == *return_to {
                resumes.insert(id, resume);
            } else {
                extra_resumes.push(resume);
            }
            return_ids.insert(key, id);
        }
        Ok(Self {
            contexts,
            resumes,
            extra_resumes,
            return_ids,
            inbox,
            inbox_context,
            hub_callees,
            static_cells: storage.anchor_head() - static_start,
            dynamic_functions: recursive.len(),
        })
    }

    pub fn profile_attributes(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("static_functions".into(), self.contexts.len().to_string()),
            (
                "dynamic_functions".into(),
                self.dynamic_functions.to_string(),
            ),
            ("static_frame_cells".into(), self.static_cells.to_string()),
            (
                "reused_return_entries".into(),
                self.resumes.len().to_string(),
            ),
            (
                "extra_return_entries".into(),
                self.extra_resumes.len().to_string(),
            ),
        ])
    }

    fn inbox_field(&self, field: AbiField) -> Location {
        Location::Global(self.inbox_context + self.inbox.abi_offset(field) as usize)
    }

    fn inbox_element(&self, index: usize) -> Result<Location, AbiCodegenError> {
        Ok(Location::Global(
            self.inbox_context
                .checked_add_signed(self.inbox.outbox_offset(index)?)
                .ok_or(FrameLayoutError::SizeOverflow)?,
        ))
    }
}

impl<'a> AbiEmitter<'a> {
    pub(super) fn initialize_fixed_main(
        &mut self,
        check_capacity: bool,
    ) -> Result<(), AbiCodegenError> {
        let fixed = self.fixed.unwrap();
        self.set_raw(
            fixed.inbox_context as isize + fixed.inbox.abi_offset(AbiField::Active),
            1,
        );
        let Some(&base) = fixed.contexts.get(&self.program.main()) else {
            // Only initialization uses physical coordinates. Temporarily hide
            // the plan to use the existing dynamic-root initialization.
            let plan = self.fixed.take();
            let result = self.initialize_main(check_capacity);
            self.fixed = plan;
            return result;
        };
        let frame = &self.layouts[&self.program.main()].frame;
        let bottom = base - (frame.frame_chunks() - frame.context_chunks()) * self.config.stride();
        for chunk in 0..frame.frame_chunks() {
            self.set_raw((bottom + chunk * self.config.stride()) as isize, 1);
        }
        self.set_raw(base as isize + frame.abi_offset(AbiField::Active), 1);
        self.set_pc_raw(base as isize, self.function(self.program.main())?.entry());
        self.move_to(base as isize + frame.abi_offset(AbiField::Active));
        self.position = frame.abi_offset(AbiField::Active);
        Ok(())
    }

    pub(super) fn emit_fixed_call(
        &mut self,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let fixed = self.fixed.unwrap();
        let callee_base = fixed.contexts.get(&callee).copied();
        let original_context = self.fixed_context;
        if original_context.is_none() && callee_base.is_none() {
            return self.emit_call_inner(caller, callee, arguments, return_to);
        }
        let function = self.function(callee)?;
        let entry = function.entry();
        let return_id = fixed.return_ids[&(return_to, result_cells(function.return_type()))];
        let frame = self.layout(callee)?.frame.clone();
        let mut copies = Vec::new();
        for (&argument, &parameter) in arguments.iter().zip(function.parameter_locations()) {
            match parameter {
                ParameterLocation::Cell(slot) => copies.push((
                    self.value_operand_element_location(argument, 0, caller)?,
                    frame.frame_offset(slot),
                )),
                ParameterLocation::AggregateElement { aggregate, index } => copies.push((
                    self.value_operand_element_location(argument, 0, caller)?,
                    frame.aggregate_element_offset(aggregate, index)?,
                )),
                ParameterLocation::Array(aggregate) | ParameterLocation::Aggregate(aggregate) => {
                    for index in 0..function.frame_aggregate(aggregate).unwrap().cells() {
                        copies.push((
                            self.value_operand_element_location(argument, index, caller)?,
                            frame.aggregate_element_offset(aggregate, index)?,
                        ));
                    }
                }
            }
        }
        if callee_base.is_none()
            && let Some(base) = original_context
        {
            // A recursive callee is pushed above the live dynamic stack, not
            // above the fixed caller. Pin source locations before changing origin.
            for (source, _) in &mut copies {
                if let Location::Relative(offset) = *source {
                    *source = Location::Global(
                        base.checked_add_signed(offset)
                            .ok_or(FrameLayoutError::SizeOverflow)?,
                    );
                }
            }
            self.fixed_context = None;
            self.emit_global_to_context(base, self.config.portal_chunks());
        }
        let delta = (frame.frame_chunks() * self.config.stride()) as isize;
        let destination = |offset| match callee_base {
            Some(base) => Location::Global(base.checked_add_signed(offset).expect("frame offset")),
            None => Location::Relative(delta + offset),
        };
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        self.clear_location(restore);
        let mut written = HashSet::new();
        for (source, offset) in copies {
            let target = destination(offset);
            if !written.insert(target) {
                self.clear_location(target);
            }
            self.copy_locations_to_zeroed_destination(source, target, restore);
        }
        // Parameters are now complete. Initialize the header at the callee,
        // where every field has a constant relative offset. Doing these stores
        // from a dynamic caller would scan its stack twice for EACH field/head.
        if let Some(base) = callee_base {
            self.emit_context_to_global(base, frame.context_chunks());
        } else {
            self.migrate_context(delta);
        }
        let bottom =
            -(((frame.frame_chunks() - frame.context_chunks()) * self.config.stride()) as isize);
        for chunk in 0..frame.frame_chunks() {
            self.set(bottom + (chunk * self.config.stride()) as isize, 1);
        }
        for field in AbiField::ALL {
            self.clear(frame.abi_offset(field));
        }
        self.set(frame.abi_offset(AbiField::Active), 1);
        if callee_base.is_none() {
            // Index is unused by function-local templates. Portal Index lives
            // in its separate context; it never overwrites this persistent bit.
            self.set(frame.abi_offset(AbiField::Index), 1);
        }
        for (field, value) in [
            (
                AbiField::NextPcLow,
                self.dispatch_encoding.encode(entry) as u8,
            ),
            (
                AbiField::NextPcHigh,
                (self.dispatch_encoding.encode(entry) >> 8) as u8,
            ),
            (
                AbiField::ReturnPcLow,
                self.dispatch_encoding.encode(return_id) as u8,
            ),
            (
                AbiField::ReturnPcHigh,
                (self.dispatch_encoding.encode(return_id) >> 8) as u8,
            ),
        ] {
            self.set(frame.abi_offset(field), value);
        }
        self.fixed_context = original_context;
        Ok(())
    }

    pub(super) fn emit_fixed_return(
        &mut self,
        callee: FunctionId,
        value: Option<ValueOperand>,
    ) -> Result<(), AbiCodegenError> {
        if self.fixed_context.is_some() {
            return self.emit_inbox_return(callee, value);
        }
        if !self.fixed.unwrap().hub_callees.contains(&callee) {
            return self.emit_return_inner(callee, value);
        }
        // Consume the route before migration. Both the inbox Index and the
        // resumed caller's Branch are zero where the corresponding loops close.
        // The caller's own Index must survive: it describes that activation's
        // eventual Return, not this callee's.
        let route = self.current_abi_offset(AbiField::Index)?;
        let gate = self.current_abi_offset(AbiField::Branch)?;
        self.set(gate, 1);
        self.move_to(route);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.clear(gate);
            emitter.emit_inbox_return(callee, value)?;
            emitter.move_to(route);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(gate);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.emit_return_inner(callee, value)?;
            emitter.move_to(gate);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn emit_inbox_return(
        &mut self,
        callee: FunctionId,
        value: Option<ValueOperand>,
    ) -> Result<(), AbiCodegenError> {
        let fixed = self.fixed.unwrap();
        let frame = self.layout(callee)?.frame.clone();
        let restore = Location::Relative(frame.abi_offset(AbiField::Restore));
        let target_value = fixed.inbox_field(AbiField::Value);
        match value {
            Some(ValueOperand::Cell(address)) => self.copy_locations(
                self.address_location(address, callee)?,
                target_value,
                restore,
            ),
            Some(operand) => {
                self.clear_location(restore);
                for index in 0..result_cells(self.function(callee)?.return_type()) {
                    self.copy_locations_with_zeroed_restore(
                        self.value_operand_element_location(operand, index, callee)?,
                        fixed.inbox_element(index)?,
                        restore,
                    );
                }
                self.clear_location(target_value);
            }
            None => self.clear_location(target_value),
        }
        for (source, target) in [
            (AbiField::ReturnPcLow, AbiField::NextPcLow),
            (AbiField::ReturnPcHigh, AbiField::NextPcHigh),
        ] {
            self.move_location(
                Location::Relative(frame.abi_offset(source)),
                fixed.inbox_field(target),
            );
        }
        let bottom =
            -(((frame.frame_chunks() - frame.context_chunks()) * self.config.stride()) as isize);
        for chunk in 0..frame.frame_chunks() {
            let head = bottom + (chunk * self.config.stride()) as isize;
            self.clear(head);
            for data in 0..self.config.chunk_cells() {
                self.clear(head + 1 + data as isize);
            }
        }
        if self.fixed_context.is_none() {
            // The freed head is zero: start the anchor scan at the preceding
            // dynamic context (or at the anchor when no dynamic frame remains).
            self.migrate_context(-((frame.frame_chunks() * self.config.stride()) as isize));
        }
        self.emit_context_to_global(fixed.inbox_context, frame.context_chunks());
        Ok(())
    }

    pub(super) fn emit_static_resume(
        &mut self,
        resume: StaticResume,
    ) -> Result<(), AbiCodegenError> {
        let fixed = self.fixed.unwrap();
        let continuation = self.program.continuation(resume.target).unwrap();
        let caller = continuation.function();
        let frame = self.layout(caller)?.frame.clone();
        self.fixed_context = fixed.contexts.get(&caller).copied();
        // The return inbox is the current physical origin; normalize once to
        // the suspended caller, then destructively deliver its result.
        self.emit_global_to_context(fixed.inbox_context, frame.context_chunks());
        self.with_profile_site(
            "abi",
            "abi.return.deliver",
            "return inbox delivery",
            |emitter| {
                emitter.move_location(
                    fixed.inbox_field(AbiField::Value),
                    Location::Relative(frame.abi_offset(AbiField::Value)),
                );
                for index in 0..resume.cells {
                    emitter.move_location(
                        fixed.inbox_element(index)?,
                        Location::Relative(frame.outbox_offset(index)?),
                    );
                }
                Ok(())
            },
        )?;
        // Execute the semantic resume and its B1 region in this same visit.
        self.emit_function_entry(continuation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bf_interpreter::{ProfileMode, ProfileOptions, RunOptions};

    struct Measurement {
        output: Vec<u8>,
        stats: bf_interpreter::RunStats,
        visits: u64,
    }

    fn program(source: &str) -> ContinuationProgram {
        crate::lower_source_with_options(
            source,
            crate::ContinuationOptimizationOptions {
                inline_functions: false,
                ..Default::default()
            },
        )
        .unwrap()
        .0
    }

    fn check(
        program: &ContinuationProgram,
        input: &[u8],
        regions: bool,
        fixed: bool,
    ) -> Measurement {
        let mut expected = Vec::new();
        let semantic = crate::run_continuations_with_io(
            program,
            &mut &input[..],
            &mut expected,
            Default::default(),
            |_| {},
        )
        .unwrap();
        let annotated = lower_continuations_with_profile_and_codegen_options(
            program,
            ProfileGranularity::Source,
            AbiCodegenOptions {
                static_frames: fixed,
                region_emission: regions,
                ..Default::default()
            },
        )
        .unwrap();
        let artifact = optimize_annotated_bf(&annotated).profile_artifact(true);
        let mut plain = Vec::new();
        optimize_bf(&annotated.into_plain())
            .write_compressed_source(&mut plain)
            .unwrap();
        assert_eq!(
            plain,
            artifact.source.as_bytes(),
            "profiling must preserve BF"
        );
        let result = bf_interpreter::run_with_options(
            artifact.source.as_bytes(),
            input,
            RunOptions {
                collect_stats: true,
                profile: Some(ProfileOptions {
                    map: artifact.map.clone(),
                    mode: ProfileMode::Counters,
                }),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.output, expected, "fixed={fixed} regions={regions}");
        let profile = result.profile.as_ref().unwrap();
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|s| s.counters.input_operations)
                .sum::<u64>(),
            semantic.input_operations
        );
        let visits = profile
            .sites
            .iter()
            .filter(|s| {
                artifact
                    .map
                    .sites
                    .iter()
                    .any(|m| m.id == s.site && m.stable_key.starts_with("abi.dispatch.enter."))
            })
            .map(|s| s.counters.loop_iterations)
            .sum::<u64>();
        eprintln!(
            "static_frames={fixed} regions={regions} compressed={} visits={visits} raw={} RLE={} native={} scan_steps={} max_pointer={}",
            artifact.source.len(),
            result.stats.executed_instructions,
            result.stats.executed_rle_instructions,
            result.stats.optimization.executed_native_operations,
            result.stats.optimization.scan_steps,
            result.stats.max_pointer
        );
        Measurement {
            output: result.output,
            stats: result.stats,
            visits,
        }
    }

    #[test]
    fn static_frames_scalar_calls_and_repeated_initialization() {
        let p = program(
            "cell g; cell leaf(cell x) { cell zero; output(zero); g = g + 1; if (x) { return x + g; } return g; } cell middle(cell x) { cell keep = x + 10; cell a = leaf(x); cell b = leaf(x + 1); return keep + a + b; } void main() { cell n = input(); while (n) { output(middle(n)); n = n - 1; } output(g); }",
        );
        for regions in [false, true] {
            let old = check(&p, &[3], regions, false);
            let new = check(&p, &[3], regions, true);
            assert_eq!(
                old.visits, new.visits,
                "Call/Return add no dispatcher visits"
            );
            assert_eq!(
                new.stats.optimization.scan_steps, 0,
                "all contexts are fixed"
            );
            assert!(old.stats.optimization.scan_steps > 0);
        }
    }

    #[test]
    fn static_frames_bridge_recursive_scc_and_nonrecursive_leaf() {
        let p = program(
            "cell g; cell leaf(cell x) { g = g + 1; return x + g; } cell odd(cell x) { if (x == 0) { return leaf(3); } cell saved = leaf(x); return even(x - 1) + saved; } cell even(cell x) { if (x == 0) { return leaf(7); } cell saved = leaf(x); return odd(x - 1) + saved; } void main() { output(even(input())); output(even(0)); output(g); }",
        );
        for regions in [false, true] {
            let old = check(&p, &[4], regions, false);
            let new = check(&p, &[4], regions, true);
            assert_eq!(old.visits, new.visits, "recursive bridges add no visits");
        }
    }

    #[test]
    fn static_frames_portal_only() {
        for source in [
            "cell[8] a; void main() { cell i=input(); a[i]=65; output(a[i]); }",
            "void main() { cell[8] a; cell i=input(); a[i]=65; output(a[i]); }",
            "struct P { cell a; cell b; } P make(cell n) { P p; p.a=n; p.b=n+1; return p; } void main(){P p=make(20); output(p.a); output(p.b);}",
        ] {
            check(&program(source), &[3], false, true);
        }
    }

    #[test]
    fn static_frames_aggregate_returns_and_portals() {
        let p = program(
            "struct Pair { cell a; cell b; } Pair[256] data; Pair make(cell x) { Pair p; p.a = x; p.b = x + 1; return p; } Pair recur(cell n) { if (n == 0) { return make(40); } Pair keep = make(n); Pair q = recur(n - 1); q.a = q.a + keep.b; return q; } void main() { Pair[256] local; cell i = input(); data[i] = recur(3); local[i] = data[i]; output(local[i].a); output(local[i].b); data[200] = make(61); output(data[200].a); }",
        );
        for regions in [false, true] {
            check(&p, &[129], regions, false);
            check(&p, &[129], regions, true);
        }
    }

    #[test]
    fn static_frames_branches_abort_and_short_circuit() {
        let p = program(
            "cell g; cell effect(cell x) { g = g + 1; if (x == 9) { abort(); } return x; } void main() { cell n = input(); output(n && effect(3)); output(n || effect(0)); if (n) { output(effect(4)); } else { output(effect(5)); } output(g); output(effect(9)); output(99); }",
        );
        for input in [0, 1] {
            for regions in [false, true] {
                check(&p, &[input], regions, true);
            }
        }
    }

    #[test]
    fn static_frames_shared_soft_resume_uses_a_separate_gate() {
        let id = |n| ContinuationId::new(n).unwrap();
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let slot = Address::Frame(FrameSlot::new(0));
        let p = ContinuationProgram::new(
            main,
            vec![
                FunctionDescriptor::new(main, vec![], 1, ValueType::Void, id(1)),
                FunctionDescriptor::new(callee, vec![], 1, ValueType::Cell, id(256)),
            ],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![
                        FrameInstruction::Input { dst: slot },
                        FrameInstruction::Set {
                            dst: Address::AbiValue,
                            value: 77,
                        },
                    ],
                    Terminator::Branch {
                        condition: slot,
                        then_target: id(2),
                        else_target: id(3),
                    },
                ),
                Continuation::new(
                    id(2),
                    main,
                    vec![],
                    Terminator::Call {
                        callee,
                        arguments: vec![],
                        return_to: id(3),
                    },
                ),
                Continuation::new(
                    id(3),
                    main,
                    vec![FrameInstruction::Output {
                        src: Address::AbiValue,
                    }],
                    Terminator::Halt,
                ),
                Continuation::new(
                    id(256),
                    callee,
                    vec![FrameInstruction::Set {
                        dst: slot,
                        value: 42,
                    }],
                    Terminator::Return {
                        value: Some(ValueOperand::Cell(slot)),
                    },
                ),
            ],
        )
        .unwrap();
        for regions in [false, true] {
            for (input, expected) in [(0, 77), (1, 42)] {
                assert_eq!(check(&p, &[input], regions, true).output, [expected]);
            }
        }
    }

    #[test]
    fn static_frames_mixed_return_width_preserves_outbox_tail() {
        let id = |n| ContinuationId::new(n).unwrap();
        let main = FunctionId::new(0);
        let short = FunctionId::new(1);
        let long = FunctionId::new(2);
        let scalar = Address::Frame(FrameSlot::new(0));
        let element = |i| Address::ArrayElement {
            array: AggregateRegion::Outbox,
            index: i,
        };
        let p = ContinuationProgram::new(
            main,
            vec![
                FunctionDescriptor::new_aggregates(
                    main,
                    vec![],
                    1,
                    vec![],
                    2,
                    ValueType::Void,
                    id(1),
                ),
                FunctionDescriptor::new(short, vec![], 1, ValueType::Cell, id(5)),
                FunctionDescriptor::new_aggregates(
                    long,
                    vec![],
                    0,
                    vec![],
                    2,
                    ValueType::Aggregate { cells: 2 },
                    id(6),
                ),
            ],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![
                        FrameInstruction::Input { dst: scalar },
                        FrameInstruction::Set {
                            dst: element(0),
                            value: 80,
                        },
                        FrameInstruction::Set {
                            dst: element(1),
                            value: 81,
                        },
                    ],
                    Terminator::Branch {
                        condition: scalar,
                        then_target: id(2),
                        else_target: id(3),
                    },
                ),
                Continuation::new(
                    id(2),
                    main,
                    vec![],
                    Terminator::Call {
                        callee: short,
                        arguments: vec![],
                        return_to: id(4),
                    },
                ),
                Continuation::new(
                    id(3),
                    main,
                    vec![],
                    Terminator::Call {
                        callee: long,
                        arguments: vec![],
                        return_to: id(4),
                    },
                ),
                Continuation::new(
                    id(4),
                    main,
                    vec![
                        FrameInstruction::Output {
                            src: Address::AbiValue,
                        },
                        FrameInstruction::Output { src: element(0) },
                        FrameInstruction::Output { src: element(1) },
                    ],
                    Terminator::Halt,
                ),
                Continuation::new(
                    id(5),
                    short,
                    vec![FrameInstruction::Set {
                        dst: scalar,
                        value: 42,
                    }],
                    Terminator::Return {
                        value: Some(ValueOperand::Cell(scalar)),
                    },
                ),
                Continuation::new(
                    id(6),
                    long,
                    vec![
                        FrameInstruction::Set {
                            dst: element(0),
                            value: 65,
                        },
                        FrameInstruction::Set {
                            dst: element(1),
                            value: 66,
                        },
                    ],
                    Terminator::Return {
                        value: Some(ValueOperand::Aggregate {
                            region: AggregateRegion::Outbox,
                            offset: 0,
                            cells: 2,
                        }),
                    },
                ),
            ],
        )
        .unwrap();
        for regions in [false, true] {
            assert_eq!(check(&p, &[0], regions, true).output, [0, 65, 66]);
            assert_eq!(check(&p, &[1], regions, true).output, [42, 80, 81]);
        }
    }

    #[test]
    fn static_frames_aggregate_arguments_and_void_return() {
        let p = program(
            "struct P { cell a; cell b; } P modify(P p, cell n) { p.a = p.a + n; return p; } void visit(P p) { output(p.a); output(p.b); } P recurse(P p, cell n) { if (n == 0) { return modify(p, 2); } P saved = modify(p, n); P result = recurse(saved, n - 1); output(saved.a); return result; } void main() { P p; p.a = 10; p.b = 20; P q = recurse(p, 2); visit(q); visit(p); }",
        );
        for regions in [false, true] {
            check(&p, &[], regions, false);
            check(&p, &[], regions, true);
        }
    }

    #[test]
    fn static_frames_overlapping_parameters_keep_last_write() {
        let id = |n| ContinuationId::new(n).unwrap();
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let region = crate::FrameAggregateId::new(0);
        let source = GlobalId::new(0);
        let scalar = Address::Frame(FrameSlot::new(0));
        let element = |i| Address::ArrayElement {
            array: AggregateRegion::Frame(region),
            index: i,
        };
        let p = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::aggregate(source, 3)],
            vec![
                FunctionDescriptor::new(main, vec![], 1, ValueType::Void, id(1)),
                FunctionDescriptor::new_aggregates(
                    callee,
                    vec![
                        ParameterLocation::Aggregate(region),
                        ParameterLocation::AggregateElement {
                            aggregate: region,
                            index: 0,
                        },
                    ],
                    0,
                    vec![crate::FrameAggregateDescriptor::new(region, 2)],
                    0,
                    ValueType::Void,
                    id(3),
                ),
            ],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![
                        FrameInstruction::Set {
                            dst: Address::ArrayElement {
                                array: AggregateRegion::Global(source),
                                index: 1,
                            },
                            value: 6,
                        },
                        FrameInstruction::Set {
                            dst: Address::ArrayElement {
                                array: AggregateRegion::Global(source),
                                index: 2,
                            },
                            value: 7,
                        },
                        FrameInstruction::Set {
                            dst: scalar,
                            value: 99,
                        },
                        FrameInstruction::Set {
                            dst: Address::AbiValue,
                            value: 88,
                        },
                    ],
                    Terminator::Call {
                        callee,
                        arguments: vec![
                            ValueOperand::Aggregate {
                                region: AggregateRegion::Global(source),
                                offset: 1,
                                cells: 2,
                            },
                            ValueOperand::Cell(scalar),
                        ],
                        return_to: id(2),
                    },
                ),
                Continuation::new(
                    id(2),
                    main,
                    vec![FrameInstruction::Output {
                        src: Address::AbiValue,
                    }],
                    Terminator::Halt,
                ),
                Continuation::new(
                    id(3),
                    callee,
                    vec![
                        FrameInstruction::Output { src: element(0) },
                        FrameInstruction::Output { src: element(1) },
                    ],
                    Terminator::Return { value: None },
                ),
            ],
        )
        .unwrap();
        for regions in [false, true] {
            assert_eq!(check(&p, &[], regions, true).output, [99, 7, 0]);
        }
    }

    #[test]
    fn static_frames_recursive_portals_preserve_return_route() {
        let p = program(
            "cell[256] g; cell bias; cell recurse(cell n) { cell[8] local; cell i = n + 1; local[i] = n + bias; g[n] = n; if (n) { cell child = recurse(n - 1); return child + local[i] + g[n]; } return local[i]; } void main() { bias = 10; output(recurse(3)); output(recurse(0)); }",
        );
        for regions in [false, true] {
            let expected = check(&p, &[], regions, true).output;
            let annotated = lower_continuations_with_profile_and_codegen_options(
                &p,
                ProfileGranularity::Abi,
                AbiCodegenOptions {
                    static_frames: true,
                    region_emission: regions,
                    nibble_transfer: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let bf = optimize_annotated_bf(&annotated).profile_artifact(true);
            let result = bf_interpreter::run_with_options(
                bf.source.as_bytes(),
                &[],
                RunOptions {
                    disable_remote_transfer: true,
                    disable_compare: true,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                result.output, expected,
                "nibble routing without interpreter recognition"
            );
        }
    }

    #[test]
    fn static_frames_header_initialization_does_not_scan_per_head() {
        let p = program(
            "void leaf() { output(65); } void recurse(cell n) { if (n) { leaf(); recurse(n - 1); } } void main() { recurse(3); }",
        );
        let functions = p
            .functions()
            .iter()
            .cloned()
            .map(|f| {
                if f.name() == Some("leaf") {
                    let slots = f.frame_slots() + 1024;
                    f.with_frame_slots(slots)
                } else {
                    f
                }
            })
            .collect();
        let padded = ContinuationProgram::new_with_globals(
            p.main(),
            p.globals().to_vec(),
            functions,
            p.continuations().to_vec(),
        )
        .unwrap()
        .with_source_files(p.source_files().to_vec());
        let narrow = check(&p, &[], true, true);
        let wide = check(&padded, &[], true, true);
        assert_eq!(
            narrow.stats.optimization.scan_steps, wide.stats.optimization.scan_steps,
            "a larger fixed callee must not add dynamic stack scans for its header"
        );
        assert_eq!(narrow.visits, wide.visits);
    }
}
