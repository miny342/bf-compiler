//! Experimental global execution contexts for closed, nonrecursive call graphs.
//! Each selected function has a fixed frame; unrelated call paths share storage.
//! Caller-specific return gates deliver results directly from that frame;
//! there is no shared return inbox.
//! CIR and the shared portal accessor bodies retain their existing semantics.
use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct StaticResume {
    pub id: ContinuationId,
    target: ContinuationId,
    callee: FunctionId,
    cells: usize,
}

#[derive(Debug)]
pub(super) struct StaticFramePlan {
    pub contexts: HashMap<FunctionId, usize>,
    pub resumes: HashMap<ContinuationId, StaticResume>,
    pub extra_resumes: Vec<StaticResume>,
    return_ids: HashMap<(ContinuationId, FunctionId), ContinuationId>,
    /// Compact, private return payloads. A leaf's CIR outbox may have zero
    /// cells when aggregate returns were forwarded from its local storage.
    result_bases: HashMap<FunctionId, usize>,
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
        selected: &HashSet<FunctionId>,
    ) -> Result<Self, AbiCodegenError> {
        let config = storage.config();
        let mut contexts = HashMap::new();
        let mut result_bases = HashMap::new();
        let ranks = crate::cir::analysis::call_graph::callee_ranks(program);
        let mut functions = program
            .functions()
            .iter()
            .filter(|function| selected.contains(&function.id()))
            .collect::<Vec<_>>();
        functions.sort_by_key(|function| ranks[&function.id()]);
        let mut callees = HashMap::<FunctionId, Vec<FunctionId>>::new();
        for continuation in program.continuations() {
            if selected.contains(&continuation.function())
                && let Some(callee) = continuation.terminator().callee()
            {
                debug_assert!(selected.contains(&callee));
                callees
                    .entry(continuation.function())
                    .or_default()
                    .push(callee);
            }
        }
        // The selected graph is closed and acyclic. Put each caller above all
        // of its descendants; unrelated activation paths can reuse storage.
        // Include each function's private return payload in its interval.
        let mut ends = HashMap::<FunctionId, usize>::new();
        for function in functions {
            let frame = &layouts[&function.id()].frame;
            let cells = frame
                .frame_chunks()
                .checked_mul(config.stride())
                .ok_or(FrameLayoutError::SizeOverflow)?;
            let bottom = callees
                .get(&function.id())
                .into_iter()
                .flatten()
                .map(|callee| ends[callee])
                .max()
                .unwrap_or(0);
            let result = result_cells(function.return_type());
            let end = bottom
                .checked_add(cells)
                .and_then(|end| end.checked_add(result))
                .ok_or(FrameLayoutError::SizeOverflow)?;
            ends.insert(function.id(), end);
            contexts.insert(
                function.id(),
                bottom + cells - frame.context_chunks() * config.stride(),
            );
            if result > 0 {
                result_bases.insert(function.id(), bottom + cells);
            }
        }
        let static_cells = ends.values().copied().max().unwrap_or(0);
        let pool_base = storage.reserve_internal_cells(static_cells)?;
        let displacement = storage.place_internal_before_near_globals(static_cells);
        for base in contexts.values_mut().chain(result_bases.values_mut()) {
            *base += pool_base - displacement;
        }
        if check_capacity {
            storage.validate_capacity()?;
            if !contexts.contains_key(&program.main()) {
                layouts[&program.main()]
                    .frame
                    .validate_main_capacity(storage.anchor_head())?;
            }
        }

        // A resume may receive results from different fixed frames. Preserve
        // its semantic ID for ordinary edges; allocate origin-specific gates.
        let mut ordinary = program
            .functions()
            .iter()
            .map(|f| f.entry())
            .collect::<HashSet<_>>();
        let mut origins = HashMap::<ContinuationId, HashSet<FunctionId>>::new();
        for c in program.continuations() {
            if let Terminator::Call {
                callee, return_to, ..
            } = c.terminator()
            {
                if contexts.contains_key(callee) {
                    origins.entry(*return_to).or_default().insert(*callee);
                } else {
                    ordinary.insert(*return_to);
                }
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
            if !contexts.contains_key(callee) {
                continue;
            }
            let key = (*return_to, *callee);
            if return_ids.contains_key(&key) {
                continue;
            }
            let id = if !ordinary.contains(return_to) && origins[return_to].len() == 1 {
                *return_to
            } else {
                allocate_hidden_id(&mut used)?
            };
            let resume = StaticResume {
                id,
                target: *return_to,
                callee: *callee,
                cells: result_cells(program.function(*callee).unwrap().return_type()),
            };
            if id == *return_to {
                resumes.insert(id, resume);
            } else {
                extra_resumes.push(resume);
            }
            return_ids.insert(key, id);
        }
        Ok(Self {
            dynamic_functions: program.functions().len() - contexts.len(),
            contexts,
            resumes,
            extra_resumes,
            return_ids,
            result_bases,
            static_cells,
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
}

impl<'a> AbiEmitter<'a> {
    pub(super) fn initialize_fixed_main(
        &mut self,
        check_capacity: bool,
    ) -> Result<(), AbiCodegenError> {
        let fixed = self.fixed.unwrap();
        let Some(&base) = fixed.contexts.get(&self.program.main()) else {
            let plan = self.fixed.take();
            let result = self.initialize_main(check_capacity);
            self.fixed = plan;
            return result;
        };
        self.initialize_anchor_bank();
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
        call: ContinuationId,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let fixed = self.fixed.unwrap();
        let callee_base = fixed.contexts[&callee];
        let original_context = self.fixed_context;
        let function = self.function(callee)?;
        let entry = function.entry();
        let return_id = fixed.return_ids[&(return_to, callee)];
        let frame = self.layout(callee)?.frame.clone();
        let mut copies = Vec::new();
        let mut remaining = HashMap::<Location, usize>::new();
        for (&argument, &parameter) in arguments.iter().zip(function.parameter_locations()) {
            let cells = match parameter {
                ParameterLocation::Cell(_) | ParameterLocation::AggregateElement { .. } => 1,
                ParameterLocation::Array(aggregate) | ParameterLocation::Aggregate(aggregate) => {
                    function.frame_aggregate(aggregate).unwrap().cells()
                }
            };
            for index in 0..cells {
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
                let source = self.address_location(address, caller)?;
                let offset = match parameter {
                    ParameterLocation::Cell(slot) => frame.frame_offset(slot),
                    ParameterLocation::AggregateElement { aggregate, index } => {
                        frame.aggregate_element_offset(aggregate, index)?
                    }
                    ParameterLocation::Array(aggregate)
                    | ParameterLocation::Aggregate(aggregate) => {
                        frame.aggregate_element_offset(aggregate, index)?
                    }
                };
                *remaining.entry(source).or_default() += 1;
                copies.push((address, source, offset));
            }
        }
        let destination = |offset| {
            Location::Global(
                callee_base
                    .checked_add_signed(offset)
                    .expect("frame offset"),
            )
        };
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        self.clear_location(restore);
        let mut written = HashSet::new();
        for (address, source, offset) in copies {
            let target = destination(offset);
            if !written.insert(target) {
                self.clear_location(target);
            }
            let left = remaining.get_mut(&source).unwrap();
            *left -= 1;
            let consume = *left == 0 && self.lifetime.terminal_dead(call, address);
            if self.nibble_transfer
                && original_context.is_none()
                && self.config.chunk_cells() >= GLOBAL_ROUTE_NIBBLE_CELLS
                && let Location::Relative(source) = source
                && let Location::Global(target) = target
            {
                self.relative_to_global_nibbles(
                    source,
                    target,
                    !consume,
                    "abi.call.argument.nibble",
                )?;
            } else if original_context.is_none()
                && let Location::Global(source) = source
            {
                // Both endpoints are static. Copy there using shared static
                // scratch, avoiding a scan per unit through the caller's Restore.
                let scratch =
                    Location::Global(self.static_layout.remote_copy_scratch_position(0).unwrap());
                self.emit_context_to_global(source, self.config.portal_chunks());
                self.fixed_context = Some(source);
                self.clear_location(scratch);
                self.copy_locations_to_zeroed_destination(
                    Location::Global(source),
                    target,
                    scratch,
                );
                self.move_to(0);
                self.fixed_context = None;
                self.emit_global_to_context(source, self.config.portal_chunks());
            } else if consume {
                self.move_location_to_zero(source, target);
            } else {
                self.copy_locations_to_zeroed_destination(source, target, restore);
            }
        }
        // Parameters are now complete. Initialize the header at the callee,
        // where every field has a constant relative offset. Doing these stores
        // from a dynamic caller would scan its stack twice for EACH field/head.
        self.emit_context_to_global(callee_base, frame.context_chunks());
        let bottom =
            -(((frame.frame_chunks() - frame.context_chunks()) * self.config.stride()) as isize);
        for chunk in 0..frame.frame_chunks() {
            self.set(bottom + (chunk * self.config.stride()) as isize, 1);
        }
        for field in AbiField::ALL {
            self.clear(frame.abi_offset(field));
        }
        self.set(frame.abi_offset(AbiField::Active), 1);
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
        if self.fixed_context.is_none() {
            return self.emit_return_inner(callee, value);
        }
        let frame = self.layout(callee)?.frame.clone();
        let restore = Location::Relative(frame.abi_offset(AbiField::Restore));
        match value {
            Some(ValueOperand::Cell(address)) => {
                let src = self.address_location(address, callee)?;
                let dst = Location::Relative(frame.abi_offset(AbiField::Value));
                if matches!(src, Location::Relative(_)) {
                    self.move_location(src, dst);
                } else {
                    self.copy_locations(src, dst, restore);
                }
            }
            Some(operand) => {
                self.clear_location(restore);
                for index in 0..result_cells(self.function(callee)?.return_type()) {
                    let src = self.value_operand_element_location(operand, index, callee)?;
                    let dst = Location::Global(self.fixed.unwrap().result_bases[&callee] + index);
                    if matches!(src, Location::Relative(_)) {
                        self.move_location(src, dst);
                    } else {
                        self.copy_locations_with_zeroed_restore(src, dst, restore);
                    }
                }
                self.clear_location(Location::Relative(frame.abi_offset(AbiField::Value)));
            }
            None => self.clear_location(Location::Relative(frame.abi_offset(AbiField::Value))),
        }
        // Locals are dead after materializing the outgoing result. Clear them
        // once per return body, instead of replicating this frame-sized sweep
        // at every caller's resume gate. Keep the context alive for dispatch.
        let local_chunks = frame.frame_chunks() - frame.context_chunks();
        let bottom = -((local_chunks * self.config.stride()) as isize);
        for chunk in 0..local_chunks {
            let head = bottom + (chunk * self.config.stride()) as isize;
            self.clear(head);
            for data in 0..self.config.chunk_cells() {
                self.clear(head + 1 + data as isize);
            }
        }
        // Retain this fixed origin until the caller-specific gate is selected.
        self.move_abi_field(AbiField::ReturnPcLow, AbiField::NextPcLow);
        self.move_abi_field(AbiField::ReturnPcHigh, AbiField::NextPcHigh);
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
        let callee_frame = self.layout(resume.callee)?.frame.clone();
        let scalar_return = self.function(resume.callee)?.return_type() == ValueType::Cell;
        let base = fixed.contexts[&resume.callee];
        self.fixed_context = Some(base);
        // Return already cleared every chunk below the context. The context
        // stayed live through the dispatch to this gate; retain only Value.
        for chunk in 0..callee_frame.context_chunks() {
            let head = (chunk * self.config.stride()) as isize;
            self.clear(head);
            for data in 0..self.config.chunk_cells() {
                let offset = head + 1 + data as isize;
                if offset != callee_frame.abi_offset(AbiField::Value) {
                    self.clear(offset);
                }
            }
        }
        let caller_base = fixed.contexts.get(&caller).copied();
        if let Some(target) = caller_base {
            self.emit_context_to_global(target, frame.context_chunks());
        } else {
            self.fixed_context = None;
            self.emit_global_to_context(base, frame.context_chunks());
        }
        self.fixed_context = caller_base;
        let return_scratch = if callee_frame.route_chunks() > 0 {
            Some(
                base.checked_add_signed(callee_frame.route_offset(ROUTE_SCRATCH_START)?)
                    .unwrap(),
            )
        } else {
            None
        };
        self.with_profile_site(
            "abi",
            "abi.return.deliver",
            "direct fixed frame result delivery",
            |emitter| {
                // Aggregate/void returns establish Value=0 at the callee.
                // Set that known value locally instead of emitting transport
                // and decomposition templates for a byte that is always zero.
                if !scalar_return {
                    emitter.clear(frame.abi_offset(AbiField::Value));
                }
                let scalar_source = base
                    .checked_add_signed(callee_frame.abi_offset(AbiField::Value))
                    .unwrap();
                let mut deliver = |source, destination| {
                    if emitter.nibble_transfer
                        && caller_base.is_none()
                        && emitter.config.chunk_cells() >= GLOBAL_ROUTE_NIBBLE_CELLS
                    {
                        emitter.move_global_to_relative_nibbles(
                            source,
                            destination,
                            return_scratch,
                        );
                    } else {
                        emitter.move_location(
                            Location::Global(source),
                            Location::Relative(destination),
                        );
                    }
                };
                if scalar_return {
                    deliver(scalar_source, frame.abi_offset(AbiField::Value));
                }
                for index in 0..resume.cells {
                    deliver(
                        fixed.result_bases[&resume.callee] + index,
                        frame.outbox_offset(index)?,
                    );
                }
                Ok(())
            },
        )?;
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
        let p = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::cell(GlobalId::new(0))],
            vec![
                FunctionDescriptor::new(main, vec![], 1, ValueType::Void, id(1)),
                FunctionDescriptor::new(callee, vec![], 1, ValueType::Cell, id(256)),
            ],
            vec![
                Continuation::new(
                    id(1),
                    main,
                    vec![
                        FrameInstruction::Set {
                            dst: Address::Global(GlobalId::new(0)),
                            value: 1,
                        },
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
        let p = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::cell(GlobalId::new(0))],
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
                        FrameInstruction::Set {
                            dst: Address::Global(GlobalId::new(0)),
                            value: 1,
                        },
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
        // Equal result widths still require separate gates when the physical
        // callee origins differ. The semantic resume and outbox tail are shared.
        let scalar_long = FunctionDescriptor::new(long, vec![], 1, ValueType::Cell, id(6));
        let functions = p
            .functions()
            .iter()
            .map(|f| {
                if f.id() == long {
                    scalar_long.clone()
                } else {
                    f.clone()
                }
            })
            .collect();
        let nodes = p
            .continuations()
            .iter()
            .map(|c| {
                if c.id() == id(6) {
                    Continuation::new(
                        id(6),
                        long,
                        vec![FrameInstruction::Set {
                            dst: scalar,
                            value: 7,
                        }],
                        Terminator::Return {
                            value: Some(ValueOperand::Cell(scalar)),
                        },
                    )
                } else {
                    c.clone()
                }
            })
            .collect();
        let same_width =
            ContinuationProgram::new_with_globals(main, p.globals().to_vec(), functions, nodes)
                .unwrap();
        for regions in [false, true] {
            assert_eq!(check(&same_width, &[0], regions, true).output, [7, 80, 81]);
            assert_eq!(check(&same_width, &[1], regions, true).output, [42, 80, 81]);
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
            "cell g; void leaf() { g = g + 1; output(65); } void recurse(cell n) { if (n) { leaf(); recurse(n - 1); } } void main() { recurse(3); }",
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

    #[test]
    fn global_contexts_exclude_callers_of_recursion_and_include_local_descendants() {
        let p = program(
            "cell g; cell local(cell x) { return x + 1; } cell leaf(cell x) { g = g + 1; return local(x) + g; } cell recurse(cell n) { if (n) { cell saved = leaf(n); return recurse(n - 1) + saved; } return leaf(0); } cell bridge(cell n) { return recurse(n); } void main() { output(bridge(input())); output(g); }",
        );
        let selected = crate::cir::analysis::call_graph::global_context_functions(&p);
        for f in p.functions() {
            assert_eq!(
                selected.contains(&f.id()),
                matches!(f.name(), Some("leaf" | "local")),
                "{:?}",
                f.name(),
            );
        }
        for regions in [false, true] {
            let baseline = check(&p, &[4], regions, false);
            let candidate = check(&p, &[4], regions, true);
            assert_eq!(baseline.visits, candidate.visits);
        }
        assert!(
            crate::cir::analysis::call_graph::global_context_functions(&program(
                "cell pure(cell x) { return x + 1; } void main() { output(pure(3)); }"
            ))
            .is_empty()
        );
    }

    #[test]
    fn global_contexts_keep_parent_locals_across_nested_calls_and_portals() {
        let p = program(
            "cell[256] g; struct P { cell a; cell b; cell c; } P leaf(cell i, cell x) { P p; g[i] = x; p.a = g[i]; p.b = x + 1; p.c = x + 2; return p; } P parent(cell i) { P keep = leaf(i, 255); P next = leaf(i + 1, 1); next.b = next.b + keep.b; next.c = keep.a; return next; } void recurse(cell n) { P saved = parent(n); if (n) { recurse(n - 1); } output(saved.a); output(saved.b); output(saved.c); } void main() { recurse(3); recurse(0); }",
        );
        for regions in [false, true] {
            let baseline = check(&p, &[], regions, false);
            let candidate = check(&p, &[], regions, true);
            assert!(
                candidate.visits <= baseline.visits,
                "fixed portal routing needs no extra visit"
            );
            assert!(
                candidate.stats.optimization.scan_steps > 0,
                "dynamic caller remains"
            );
        }
    }

    #[test]
    fn global_contexts_nibble_arguments_preserve_aliases_and_aggregate_returns() {
        // No global portal: argument transport must reserve its own route.
        // Repeated live arguments, a global argument, and a live aggregate all
        // survive calls from recursive frames spanning several anchor phases.
        let p = program(
            "cell g; struct P { cell a; cell b; cell c; } P leaf(P p, cell x, cell y, cell z) { g = g + 1; p.a = x < z; p.b = p.b + y; p.c = z; return p; } void recurse(cell n) { cell x = input(); P keep; keep.a = x; keep.b = 255; keep.c = 127; P next = leaf(keep, x, x, g); if (n) { recurse(n - 1); } output(next.a); output(next.b); output(next.c); output(keep.a); output(keep.b); output(keep.c); output(x); } void main() { g = 255; recurse(19); recurse(0); }",
        );
        let input = (0..21).map(|i| [0, 1, 127, 255][i % 4]).collect::<Vec<_>>();
        let mut expected = Vec::new();
        crate::run_continuations_with_io(
            &p,
            &mut &input[..],
            &mut expected,
            Default::default(),
            |_| {},
        )
        .unwrap();
        for regions in [false, true] {
            for anchor_bank in [false, true] {
                let annotated = lower_continuations_with_profile_and_codegen_options(
                    &p,
                    ProfileGranularity::Abi,
                    AbiCodegenOptions {
                        static_frames: true,
                        nibble_transfer: true,
                        inplace_compare: true,
                        region_emission: regions,
                        anchor_bank,
                        ..Default::default()
                    },
                )
                .unwrap();
                let artifact = optimize_annotated_bf(&annotated).profile_artifact(true);
                let optimized = bf_interpreter::run_with_options(
                    artifact.source.as_bytes(),
                    &input,
                    RunOptions {
                        collect_stats: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                assert_eq!(optimized.output, expected);
                let rle = bf_interpreter::run_with_options(
                    artifact.source.as_bytes(),
                    &input,
                    RunOptions {
                        collect_stats: true,
                        disable_clear: true,
                        disable_scan: true,
                        disable_transfer: true,
                        disable_countdown: true,
                        disable_remote_transfer: true,
                        disable_compare: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                assert_eq!(rle.output, expected);
                assert_eq!(
                    rle.stats.executed_instructions,
                    optimized.stats.executed_instructions
                );
                assert_eq!(
                    rle.stats.executed_rle_instructions,
                    optimized.stats.executed_rle_instructions
                );
            }
        }
    }

    #[test]
    fn global_contexts_share_sibling_frames_without_overlapping_ancestors() {
        let p = program(
            "cell g; struct P { cell a; cell b; cell c; } cell shared(cell x) { g = g + 1; return x + g; } P left(cell x) { P p; p.a = shared(x); p.b = x; p.c = 255; return p; } cell right(cell x) { cell[40] buf; buf[x] = shared(x); return buf[x]; } P parent(cell x) { P keep = left(x); keep.b = right(x); return keep; } void recurse(cell n) { P keep = parent(n); if (n) { recurse(n - 1); } output(keep.a); output(keep.b); output(keep.c); } void main() { recurse(2); }",
        );
        let config = AbiConfig::default();
        let selected = crate::cir::analysis::call_graph::global_context_functions(&p);
        let layouts = build_layouts_with_regions(&p, config, None).unwrap();
        let portal = PortalPlan::new(&p).unwrap();
        let mut storage = StaticLayout::new(config, p.globals()).unwrap();
        let fixed =
            StaticFramePlan::new(&p, &layouts, &portal, &mut storage, true, &selected).unwrap();
        let interval = |id: FunctionId| {
            let frame = &layouts[&id].frame;
            let bottom = fixed.contexts[&id]
                - (frame.frame_chunks() - frame.context_chunks()) * config.stride();
            let end = bottom
                + frame.frame_chunks() * config.stride()
                + result_cells(p.function(id).unwrap().return_type());
            (bottom, end)
        };
        let named = |name| {
            p.functions()
                .iter()
                .find(|function| function.name() == Some(name))
                .unwrap()
                .id()
        };
        assert_eq!(interval(named("left")).0, interval(named("right")).0);
        for c in p.continuations() {
            if selected.contains(&c.function())
                && let Some(callee) = c.terminator().callee()
            {
                assert!(interval(c.function()).0 >= interval(callee).1);
            }
        }
        let separate_cells = selected
            .iter()
            .map(|id| interval(*id).1 - interval(*id).0)
            .sum::<usize>();
        assert!(fixed.static_cells < separate_cells);
        for regions in [false, true] {
            for nibble in [false, true] {
                for anchor_bank in [false, true] {
                    let bf = lower_continuations_with_profile_and_codegen_options(
                        &p,
                        ProfileGranularity::Abi,
                        AbiCodegenOptions {
                            static_frames: true,
                            region_emission: regions,
                            nibble_transfer: nibble,
                            anchor_bank,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    let artifact = optimize_annotated_bf(&bf).profile_artifact(true);
                    let result = bf_interpreter::run_with_options(
                        artifact.source.as_bytes(),
                        &[],
                        RunOptions {
                            collect_stats: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    assert_eq!(result.output, [5, 6, 255, 4, 5, 255, 3, 4, 255]);
                    let rle = bf_interpreter::run_with_options(
                        artifact.source.as_bytes(),
                        &[],
                        RunOptions {
                            collect_stats: true,
                            disable_clear: true,
                            disable_scan: true,
                            disable_transfer: true,
                            disable_countdown: true,
                            disable_remote_transfer: true,
                            disable_compare: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    assert_eq!(rle.output, result.output);
                    assert_eq!(
                        rle.stats.executed_instructions,
                        result.stats.executed_instructions
                    );
                    assert_eq!(
                        rle.stats.executed_rle_instructions,
                        result.stats.executed_rle_instructions
                    );
                }
            }
        }
    }

    #[test]
    fn global_contexts_non_scalar_returns_clear_value_and_preserve_outbox_tail() {
        let p = program(
            "cell g; struct P { cell a; cell b; } void leaf(cell n) { g = n; } P pair(cell n) { P p; p.a = n; p.b = g; return p; } void recurse(cell n) { leaf(n); P p = pair(n); if (n) { recurse(n - 1); } output(p.a); output(p.b); } void main() { recurse(1); }",
        );
        let fixed = crate::cir::analysis::call_graph::global_context_functions(&p);
        let resumes = p
            .continuations()
            .iter()
            .filter_map(|c| match c.terminator() {
                Terminator::Call {
                    callee, return_to, ..
                } if fixed.contains(callee) => Some(*return_to),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let tail = Address::ArrayElement {
            array: AggregateRegion::Outbox,
            index: 2,
        };
        let functions = p
            .functions()
            .iter()
            .map(|f| {
                FunctionDescriptor::new_aggregates(
                    f.id(),
                    f.parameter_locations().to_vec(),
                    f.frame_slots(),
                    f.frame_aggregates().to_vec(),
                    f.outbox_cells().max(3),
                    f.return_type(),
                    f.entry(),
                )
            })
            .collect();
        let continuations = p
            .continuations()
            .iter()
            .map(|c| {
                let mut body = Vec::new();
                if resumes.contains(&c.id()) {
                    body.extend([
                        FrameInstruction::Output {
                            src: Address::AbiValue,
                        },
                        FrameInstruction::Output { src: tail },
                    ]);
                }
                body.extend_from_slice(c.body());
                if matches!(c.terminator(), Terminator::Call { callee, .. } if fixed.contains(callee)) {
                    body.extend([
                        FrameInstruction::Set {
                            dst: Address::AbiValue,
                            value: 255,
                        },
                        FrameInstruction::Set { dst: tail, value: 81 },
                    ]);
                }
                Continuation::new(c.id(), c.function(), body, c.terminator().clone())
            })
            .collect();
        let p = ContinuationProgram::new_with_globals(
            p.main(),
            p.globals().to_vec(),
            functions,
            continuations,
        )
        .unwrap();
        for regions in [false, true] {
            for nibble in [false, true] {
                for anchor_bank in [false, true] {
                    let bf = lower_continuations_with_profile_and_codegen_options(
                        &p,
                        ProfileGranularity::Abi,
                        AbiCodegenOptions {
                            static_frames: true,
                            region_emission: regions,
                            nibble_transfer: nibble,
                            anchor_bank,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    let artifact = optimize_annotated_bf(&bf).profile_artifact(true);
                    let result = bf_interpreter::run_with_options(
                        artifact.source.as_bytes(),
                        &[],
                        RunOptions::default(),
                    )
                    .unwrap();
                    assert_eq!(result.output, [0, 81, 0, 81, 0, 81, 0, 81, 0, 0, 1, 1]);
                }
            }
        }
    }

    #[test]
    fn global_contexts_rle_only_matches_optimized_counters() {
        let p = program(
            "cell[256] g; struct P { cell a; cell b; } P read(cell i) { P p; p.a = g[i]; p.b = g[i + 1]; return p; } cell worker(cell n) { g[n] = 255; g[n + 1] = 0; P p = read(n); return p.a + p.b; } cell recursive(cell n) { g[n + 128] = n; cell saved = worker(n); if (n) { return recursive(n - 1) + saved + g[n + 128]; } return saved; } void main() { output(recursive(1)); output(worker(3)); }",
        );
        for regions in [false, true] {
            for (nibble, anchor_bank) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let annotated = lower_continuations_with_profile_and_codegen_options(
                    &p,
                    ProfileGranularity::Abi,
                    AbiCodegenOptions {
                        static_frames: true,
                        region_emission: regions,
                        nibble_transfer: nibble,
                        anchor_bank,
                        inplace_compare: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                let artifact = optimize_annotated_bf(&annotated).profile_artifact(true);
                let optimized = bf_interpreter::run_with_options(
                    artifact.source.as_bytes(),
                    &[],
                    RunOptions {
                        collect_stats: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                let rle = bf_interpreter::run_with_options(
                    artifact.source.as_bytes(),
                    &[],
                    RunOptions {
                        collect_stats: true,
                        disable_clear: true,
                        disable_scan: true,
                        disable_transfer: true,
                        disable_countdown: true,
                        disable_remote_transfer: true,
                        disable_compare: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                assert_eq!(optimized.output, [255, 255]);
                assert_eq!(rle.output, optimized.output);
                assert_eq!(
                    rle.stats.executed_instructions,
                    optimized.stats.executed_instructions
                );
                assert_eq!(
                    rle.stats.executed_rle_instructions,
                    optimized.stats.executed_rle_instructions
                );
            }
        }
    }
}
