//! Execute existing entry regions directly; keep all shared resume identities.
use super::*;

#[derive(Clone, Copy)]
struct DirectReturn {
    callee: FunctionId,
    target: ContinuationId,
    fixed: Option<StaticResume>,
}

#[derive(Default)]
pub(super) struct DirectRegionState {
    depth: usize,
    returns: Vec<DirectReturn>,
}

#[derive(Clone, Copy)]
pub(super) struct DirectCallSetup {
    pub preserve_return_pc: bool,
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
        callee: FunctionId,
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
        self.direct_regions.returns.pop();
        self.direct_regions.depth -= 1;
        self.fixed_context = previous;
        self.position = previous_position;
        self.branch_temporary_depth = previous_depth;
        self.region_loops = previous_loops;
        let body = body?;
        let (raw, code) = emitted_size(&body);
        if raw <= raw_limit && code <= code_limit {
            Ok(Some(DirectCall {
                setup: DirectCallSetup { preserve_return_pc },
                body,
                position,
            }))
        } else {
            // No prologue has been emitted yet. The caller will generate the
            // ordinary call, including both entry and return PC initialization.
            Ok(None)
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
        // Pop this activation while emitting its caller. Recursion can give
        // both activations the same FunctionId, but different known returns.
        let delivered = self.direct_fixed_result(callee).is_some();
        let known = self.direct_regions.returns.pop().unwrap();
        let previous = self.fixed_context;
        let previous_position = self.position;
        let previous_depth = self.branch_temporary_depth;
        let previous_loops = std::mem::take(&mut self.region_loops);
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
        self.direct_regions.returns.push(known);
        let body = body?;
        let (_, code) = emitted_size(&body);
        let limit = std::env::var("BFC_EVAL_DIRECT_RESUME_CODE_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| limits().2);
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
