//! Execute existing entry regions directly; keep all shared resume identities.
use super::*;

#[derive(Clone, Copy)]
struct DirectReturn {
    callee: FunctionId,
    target: ContinuationId,
    fixed: Option<StaticResume>,
    shared_return: bool,
}

#[derive(Clone, Copy)]
struct DirectBinding {
    location: Location,
    consumable: bool,
}

#[derive(Default)]
pub(super) struct DirectRegionState {
    depth: usize,
    returns: Vec<DirectReturn>,
    bindings: HashMap<(FunctionId, Address), DirectBinding>,
    reference_trial: bool,
}

#[derive(Clone)]
pub(super) struct DirectCallSetup {
    pub preserve_return_pc: bool,
    pub frameless: bool,
    pub borrowed: HashSet<Address>,
    pub borrowed_sources: HashSet<Location>,
}

pub(super) struct DirectCall {
    pub setup: DirectCallSetup,
    pub body: Vec<AnnotatedBfInstruction>,
    pub position: isize,
}

pub(super) struct DirectResume {
    pub delivered: bool,
    pub body: Vec<AnnotatedBfInstruction>,
    pub position: isize,
}

fn limits() -> (usize, usize, usize) {
    let get = |name, default| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(default)
    };
    (
        get("BFC_EVAL_DIRECT_REGION", 0),
        get("BFC_EVAL_DIRECT_RAW_LIMIT", usize::MAX),
        get("BFC_EVAL_DIRECT_CODE_LIMIT", usize::MAX),
    )
}

fn emitted_size(body: &[AnnotatedBfInstruction]) -> (usize, usize) {
    let mut pending = vec![body];
    let (mut raw, mut code) = (0usize, 0usize);
    while let Some(body) = pending.pop() {
        for i in body {
            let (r, c) = match &i.operation {
                AnnotatedBfOperation::Move(n) => (n.unsigned_abs(), 1),
                AnnotatedBfOperation::Add(n) => (usize::from((*n).min(n.wrapping_neg())), 1),
                AnnotatedBfOperation::Input | AnnotatedBfOperation::Output => (1, 1),
                AnnotatedBfOperation::Loop(body) => {
                    pending.push(body);
                    (2, 2)
                }
            };
            raw = raw.saturating_add(r);
            code = code.saturating_add(c);
        }
    }
    (raw, code)
}

impl<'a> AbiEmitter<'a> {
    pub(super) fn plan_direct_call_entry(
        &mut self,
        call: ContinuationId,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<Option<DirectCall>, AbiCodegenError> {
        let (depth, raw_limit, code_limit) = limits();
        if self.regions.is_none() || self.direct_regions.depth >= depth {
            return Ok(None);
        }
        let base = self.fixed.and_then(|p| p.contexts.get(&callee)).copied();
        if base.is_none() && std::env::var("BFC_EVAL_DIRECT_DYNAMIC").as_deref() != Ok("1") {
            return Ok(None);
        }
        let known = DirectReturn {
            callee,
            target: return_to,
            fixed: base.map(|_| self.fixed.unwrap().resume_for_call(callee, return_to)),
            shared_return: false,
        };
        let entry = self.function(callee)?.entry();
        // An unexpanded Call, portal, or bounded soft-edge fallback needs the
        // stored return PC after shared dispatch. Only closed entry regions
        // can omit it, even when one other path returns immediately.
        let closed = |id| {
            matches!(
                self.program.continuation(id).unwrap().terminator(),
                Terminator::Return { .. } | Terminator::Abort | Terminator::Halt
            )
        };
        let preserve_return_pc = std::env::var("BFC_EVAL_DIRECT_RETURN").as_deref() != Ok("1")
            || !self
                .regions
                .and_then(|p| p.regions.get(&entry))
                .map_or_else(
                    || closed(entry),
                    |r| r.terminals.iter().copied().all(closed),
                );
        let binding_mode = std::env::var("BFC_EVAL_DIRECT_BINDINGS").unwrap_or_default();
        let specialized =
            !self.direct_regions.reference_trial && matches!(binding_mode.as_str(), "1" | "2");
        let bindings = if specialized && !preserve_return_pc && base.is_some() {
            let mut bindings = self.direct_parameter_bindings(call, caller, callee, arguments)?;
            if binding_mode == "1" {
                bindings.retain(|&(_, address), _| self.direct_address_read_only(callee, address));
            }
            bindings
        } else {
            HashMap::new()
        };
        // Isolate value optimization from the size selector: a smaller
        // specialized body must not admit new, otherwise rejected call sites.
        // If specialization grows beyond the budget, retain this canonical
        // body instead of dropping a previously accepted direct call.
        let reference = if specialized {
            let previous_returns = self.direct_regions.returns.clone();
            self.direct_regions.reference_trial = true;
            let reference = self.plan_direct_call_entry(call, caller, callee, arguments, return_to);
            self.direct_regions.reference_trial = false;
            self.direct_regions.returns = previous_returns;
            let Some(reference) = reference? else {
                return Ok(None);
            };
            Some(reference)
        } else {
            None
        };
        let borrowed = bindings.keys().map(|&(_, address)| address).collect();
        let borrowed_sources = bindings.values().map(|b| b.location).collect();
        let previous_bindings = std::mem::replace(&mut self.direct_regions.bindings, bindings);
        let previous = self.fixed_context;
        let previous_position = self.position;
        let previous_depth = self.branch_temporary_depth;
        let previous_loops = std::mem::take(&mut self.region_loops);
        self.fixed_context = base;
        // This detached body runs after the prologue migrates to the callee.
        self.position = 0;
        self.branch_temporary_depth = 0;
        self.direct_regions.depth += 1;
        self.direct_regions.returns.push(known);
        let body = self.capture(|e| {
            e.with_profile_site("abi", "abi.region.call", "direct callee entry", |e| {
                e.emit_function_entry(e.program.continuation(entry).unwrap())
            })
        });
        let position = self.position;
        let return_state = self.direct_regions.returns.pop().unwrap();
        self.direct_regions.depth -= 1;
        self.fixed_context = previous;
        self.position = previous_position;
        self.branch_temporary_depth = previous_depth;
        self.region_loops = previous_loops;
        self.direct_regions.bindings = previous_bindings;
        let body = body?;
        let (raw, code) = emitted_size(&body);
        if raw <= raw_limit && code <= code_limit {
            Ok(Some(DirectCall {
                setup: DirectCallSetup {
                    preserve_return_pc,
                    frameless: specialized
                        && base.is_some()
                        && !preserve_return_pc
                        && !return_state.shared_return,
                    borrowed,
                    borrowed_sources,
                },
                body,
                position,
            }))
        } else {
            // No prologue has been emitted yet. The caller will generate the
            // ordinary call, including both entry and return PC initialization.
            Ok(reference)
        }
    }

    pub(super) fn has_direct_return(&self, callee: FunctionId) -> bool {
        std::env::var("BFC_EVAL_DIRECT_RETURN").as_deref() == Ok("1")
            && self
                .direct_regions
                .returns
                .last()
                .is_some_and(|r| r.callee == callee)
    }

    pub(super) fn borrowed_location(
        &self,
        function: FunctionId,
        address: Address,
    ) -> Option<Location> {
        self.direct_regions
            .bindings
            .get(&(function, address))
            .map(|b| b.location)
    }

    pub(super) fn can_consume_borrowed(&self, function: FunctionId, address: Address) -> bool {
        self.direct_regions
            .bindings
            .get(&(function, address))
            .is_some_and(|b| {
                b.consumable
                    && self
                        .direct_regions
                        .bindings
                        .values()
                        .filter(|other| other.location == b.location)
                        .count()
                        == 1
            })
    }

    pub(super) fn forget_direct_binding(&mut self, function: FunctionId, address: Address) {
        self.direct_regions.bindings.remove(&(function, address));
    }

    fn direct_parameter_bindings(
        &self,
        call: ContinuationId,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
    ) -> Result<HashMap<(FunctionId, Address), DirectBinding>, AbiCodegenError> {
        let mut bindings = HashMap::new();
        let caller_base = self.fixed.and_then(|p| p.contexts.get(&caller)).copied();
        let function = self.function(callee)?;
        for (&parameter, &argument) in function.parameter_locations().iter().zip(arguments) {
            let ParameterLocation::Cell(slot) = parameter else {
                continue;
            };
            let key = (callee, Address::Frame(slot));
            bindings.remove(&key);
            let ValueOperand::Cell(source) = argument else {
                continue;
            };
            let local = matches!(
                source,
                Address::Frame(_)
                    | Address::AbiValue
                    | Address::ArrayElement {
                        array: AggregateRegion::Frame(_),
                        ..
                    }
            );
            let stable = if local {
                caller_base.is_some()
            } else {
                matches!(source, Address::Global(_))
                    && self.direct_address_read_only(callee, source)
            };
            if stable {
                let location = match self.address_location(source, caller)? {
                    Location::Relative(offset) => {
                        Location::Global(caller_base.unwrap().checked_add_signed(offset).unwrap())
                    }
                    location => location,
                };
                bindings.insert(
                    key,
                    DirectBinding {
                        location,
                        consumable: local && self.lifetime.terminal_dead(call, source),
                    },
                );
            }
        }
        Ok(bindings)
    }

    fn direct_address_read_only(&self, callee: FunctionId, source: Address) -> bool {
        use crate::cir::effects::{self, Effect};
        let mut stable = true;
        let mut write = |effect| match effect {
            Effect::Write(ValueOperand::Cell(a)) | Effect::Clobber(a) if a == source => {
                stable = false
            }
            _ => {}
        };
        let mut pending = Vec::new();
        for c in self
            .program
            .continuations()
            .iter()
            .filter(|c| c.function() == callee)
        {
            effects::terminator(c.terminator(), &mut write);
            pending.push(c.body());
        }
        while let Some(body) = pending.pop() {
            for instruction in body {
                effects::instruction(instruction, &mut write);
                match instruction {
                    FrameInstruction::Loop { body, .. } => pending.push(body),
                    FrameInstruction::Branch {
                        then_body,
                        else_body,
                        ..
                    } => pending.extend([then_body.as_slice(), else_body.as_slice()]),
                    _ => {}
                }
            }
        }
        stable
    }

    pub(super) fn prepare_direct_instruction_bindings(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        use crate::cir::effects::{self, Effect};
        if self.direct_regions.bindings.is_empty() {
            return Ok(());
        }
        // Materialize before control splits or repeats: codegen visits arms
        // sequentially, but runtime executes either arm (or zero iterations).
        if matches!(
            instruction,
            FrameInstruction::Branch { .. } | FrameInstruction::Loop { .. }
        ) {
            return self.materialize_direct_bindings(function);
        }
        let mut reads = HashSet::new();
        let mut writes = HashSet::new();
        effects::instruction(instruction, |effect| match effect {
            Effect::Read(ValueOperand::Cell(address)) => {
                reads.insert(address);
            }
            Effect::Write(ValueOperand::Cell(address)) | Effect::Clobber(address) => {
                writes.insert(address);
            }
            _ => {}
        });
        let mut writes = writes
            .into_iter()
            .filter(|&a| self.borrowed_location(function, a).is_some())
            .collect::<Vec<_>>();
        writes.sort_by_key(|a| match a {
            Address::Frame(slot) => slot.index(),
            _ => unreachable!(),
        });
        for address in writes {
            if reads.contains(&address) {
                self.materialize_direct_binding(function, address)?;
            } else {
                // An unconditional overwrite needs no snapshot of the old
                // parameter. The actual callee cell is still zero here.
                self.direct_regions.bindings.remove(&(function, address));
            }
        }
        Ok(())
    }

    pub(super) fn materialize_direct_bindings(
        &mut self,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let mut addresses = self
            .direct_regions
            .bindings
            .keys()
            .filter_map(|&(f, address)| (f == function).then_some(address))
            .collect::<Vec<_>>();
        addresses.sort_by_key(|a| match a {
            Address::Frame(slot) => slot.index(),
            _ => unreachable!(),
        });
        for address in addresses {
            self.materialize_direct_binding(function, address)?;
        }
        Ok(())
    }

    fn materialize_direct_binding(
        &mut self,
        function: FunctionId,
        address: Address,
    ) -> Result<(), AbiCodegenError> {
        let consume = self.can_consume_borrowed(function, address);
        let Some(binding) = self.direct_regions.bindings.remove(&(function, address)) else {
            return Ok(());
        };
        let destination = self.address_location(address, function)?;
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        self.with_profile_site(
            "abi",
            "abi.region.binding.materialize",
            "materialize borrowed parameter",
            |e| {
                if consume {
                    e.move_location(binding.location, destination);
                } else {
                    e.copy_locations(binding.location, destination, restore);
                }
                Ok(())
            },
        )
    }

    pub(super) fn direct_fixed_result(&self, callee: FunctionId) -> Option<StaticResume> {
        if !self.has_direct_return(callee) || self.fixed_context.is_none() {
            return None;
        }
        let known = self.direct_regions.returns.last()?;
        let resume = known.fixed?;
        let caller = self.program.continuation(known.target)?.function();
        self.fixed?.contexts.contains_key(&caller).then_some(resume)
    }

    pub(super) fn plan_direct_return_resume(
        &mut self,
        callee: FunctionId,
    ) -> Result<Option<DirectResume>, AbiCodegenError> {
        if !self.has_direct_return(callee) {
            return Ok(None);
        }
        // Header/copy reductions inside a caller suffix must not make a new
        // resume expansion pass the budget either.
        let specialized = !self.direct_regions.reference_trial
            && matches!(
                std::env::var("BFC_EVAL_DIRECT_BINDINGS").as_deref(),
                Ok("1" | "2")
            );
        let reference = if specialized {
            let previous_returns = self.direct_regions.returns.clone();
            self.direct_regions.reference_trial = true;
            let reference = self.plan_direct_return_resume(callee);
            self.direct_regions.reference_trial = false;
            self.direct_regions.returns = previous_returns;
            let Some(reference) = reference? else {
                self.direct_regions
                    .returns
                    .last_mut()
                    .unwrap()
                    .shared_return = true;
                return Ok(None);
            };
            Some(reference)
        } else {
            None
        };
        // Pop this activation while emitting its caller. Recursion can give
        // both activations the same FunctionId, but different known returns.
        let delivered = self.direct_fixed_result(callee).is_some();
        let known = self.direct_regions.returns.pop().unwrap();
        let previous = self.fixed_context;
        let previous_position = self.position;
        let previous_depth = self.branch_temporary_depth;
        let previous_loops = std::mem::take(&mut self.region_loops);
        let previous_bindings = std::mem::take(&mut self.direct_regions.bindings);
        self.position = 0;
        self.branch_temporary_depth = 0;
        let body = self.capture(|e| {
            e.with_profile_site("abi", "abi.region.return", "direct caller resume", |e| {
                e.clear_abi_field(AbiField::NextPcLow)?;
                e.clear_abi_field(AbiField::NextPcHigh)?;
                e.move_to(0);
                match known.fixed {
                    Some(resume) => e.emit_static_resume_with_result(resume, delivered),
                    None => e.emit_function_entry(e.program.continuation(known.target).unwrap()),
                }
            })
        });
        let position = self.position;
        // Alternate region terminal cases still begin in the callee context.
        self.fixed_context = previous;
        self.position = previous_position;
        self.branch_temporary_depth = previous_depth;
        self.region_loops = previous_loops;
        self.direct_regions.bindings = previous_bindings;
        self.direct_regions.returns.push(known);
        let body = body?;
        let (_, code) = emitted_size(&body);
        let limit = std::env::var("BFC_EVAL_DIRECT_RESUME_CODE_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| limits().2);
        if code > limit {
            if let Some(reference) = reference {
                return Ok(Some(reference));
            }
            self.direct_regions
                .returns
                .last_mut()
                .unwrap()
                .shared_return = true;
        }
        Ok((code <= limit).then_some(DirectResume {
            delivered,
            body,
            position,
        }))
    }

    pub(super) fn set_known_return_pc(
        &mut self,
        callee: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        if self.has_direct_return(callee) {
            let known = self.direct_regions.returns.last().unwrap();
            self.set_next_pc(known.fixed.map_or(known.target, |r| r.id))?;
        }
        Ok(())
    }
}
