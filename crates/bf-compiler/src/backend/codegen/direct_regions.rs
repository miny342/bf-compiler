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
    pub(super) fn emit_direct_call_entry(
        &mut self,
        callee: FunctionId,
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let (depth, raw_limit, code_limit) = limits();
        if self.regions.is_none() || self.direct_regions.depth >= depth {
            return Ok(());
        }
        let base = self.fixed.and_then(|p| p.contexts.get(&callee)).copied();
        if base.is_none() && std::env::var("BFC_EVAL_DIRECT_DYNAMIC").as_deref() != Ok("1") {
            return Ok(());
        }
        let known = DirectReturn {
            callee,
            target: return_to,
            fixed: base.map(|_| self.fixed.unwrap().resume_for_call(callee, return_to)),
        };
        let entry = self.function(callee)?.entry();
        let previous = self.fixed_context;
        let previous_depth = self.branch_temporary_depth;
        let previous_loops = std::mem::take(&mut self.region_loops);
        self.fixed_context = base;
        self.branch_temporary_depth = 0;
        self.direct_regions.depth += 1;
        self.direct_regions.returns.push(known);
        let body = self.capture(|e| {
            e.clear_abi_field(AbiField::NextPcLow)?;
            e.clear_abi_field(AbiField::NextPcHigh)?;
            e.move_to(0);
            e.with_profile_site("abi", "abi.region.call", "direct callee entry", |e| {
                e.emit_function_entry(e.program.continuation(entry).unwrap())
            })
        });
        self.direct_regions.returns.pop();
        self.direct_regions.depth -= 1;
        self.fixed_context = previous;
        self.branch_temporary_depth = previous_depth;
        self.region_loops = previous_loops;
        let body = body?;
        let (raw, code) = emitted_size(&body);
        if raw <= raw_limit && code <= code_limit {
            self.output.extend(body);
        } else {
            // The trial emitted no runtime commands. The call's original
            // initialized NextPc still selects the shared callee entry.
            self.position = 0;
        }
        Ok(())
    }

    pub(super) fn emit_direct_return_resume(
        &mut self,
        callee: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        if std::env::var("BFC_EVAL_DIRECT_RETURN").as_deref() != Ok("1")
            || self
                .direct_regions
                .returns
                .last()
                .is_none_or(|r| r.callee != callee)
        {
            return Ok(());
        }
        // Pop this activation while emitting its caller. Recursion can give
        // both activations the same FunctionId, but different known returns.
        let known = self.direct_regions.returns.pop().unwrap();
        let previous = self.fixed_context;
        let result =
            self.with_profile_site("abi", "abi.region.return", "direct caller resume", |e| {
                e.clear_abi_field(AbiField::NextPcLow)?;
                e.clear_abi_field(AbiField::NextPcHigh)?;
                e.move_to(0);
                match known.fixed {
                    Some(resume) => e.emit_static_resume(resume),
                    None => e.emit_function_entry(e.program.continuation(known.target).unwrap()),
                }
            });
        // Alternate region terminal cases still begin in the callee context.
        self.fixed_context = previous;
        self.direct_regions.returns.push(known);
        result
    }
}
