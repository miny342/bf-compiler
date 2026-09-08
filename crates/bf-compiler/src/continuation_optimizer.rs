//! Control-flow simplification for continuation IR.
//!
//! The lowering of structured source control flow deliberately keeps each
//! continuation boundary explicit.  Some of those boundaries have no frame
//! instructions and only jump to another continuation.  Threading those jumps
//! before ABI lowering removes dispatcher visits.  The surviving IDs are
//! compacted afterwards so the ABI can continue to use dense countdown pages.

use std::collections::{HashMap, HashSet};

use crate::continuation_ir::{
    Continuation, ContinuationId, ContinuationIrError, ContinuationProgram, FunctionDescriptor,
    FunctionId, Terminator,
};

/// Static measurements from the continuation CFG simplification pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContinuationOptimizationStats {
    pub continuations_before: usize,
    pub continuations_after: usize,
    pub empty_gotos_before: usize,
    pub empty_gotos_after: usize,
    pub empty_gotos_threaded: usize,
    pub unreachable_continuations_removed: usize,
    pub successor_references_rewritten: usize,
    pub function_entries_rewritten: usize,
    pub continuation_ids_compacted: bool,
}

impl ContinuationOptimizationStats {
    pub const fn continuations_removed(self) -> usize {
        self.continuations_before
            .saturating_sub(self.continuations_after)
    }
}

/// Thread empty `Goto` continuations and discard unreachable continuation
/// blocks.  Surviving IDs are compacted in original order when needed.
pub fn optimize_continuations(
    program: &ContinuationProgram,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), ContinuationIrError> {
    let continuations = program.continuations();
    let by_id = continuations
        .iter()
        .map(|continuation| (continuation.id(), continuation))
        .collect::<HashMap<_, _>>();
    let function_entries = program
        .functions()
        .iter()
        .map(|function| (function.id(), function.entry()))
        .collect::<HashMap<_, _>>();
    let empty_targets = continuations
        .iter()
        .filter_map(|continuation| match continuation.terminator() {
            Terminator::Goto { target } if continuation.body().is_empty() => {
                Some((continuation.id(), *target))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    let mut resolver = TargetResolver::new(&by_id, &empty_targets, &function_entries);
    let remap = continuations
        .iter()
        .map(|continuation| {
            let id = continuation.id();
            (id, resolver.resolve(id))
        })
        .collect::<HashMap<_, _>>();

    let mut successor_references_rewritten = 0;
    let mut rewritten = Vec::with_capacity(continuations.len());
    for continuation in continuations {
        let mut terminator = continuation.terminator().clone();
        map_successor_references(&mut terminator, &remap, &mut successor_references_rewritten);
        rewritten.push(Continuation::new(
            continuation.id(),
            continuation.function(),
            continuation.body().to_vec(),
            terminator,
        ));
    }

    let mut function_entries_rewritten = 0;
    let functions = program
        .functions()
        .iter()
        .cloned()
        .map(|function| {
            let entry = remap[&function.entry()];
            if entry != function.entry() {
                function_entries_rewritten += 1;
            }
            function.with_entry(entry)
        })
        .collect::<Vec<FunctionDescriptor>>();

    let function_entries = functions
        .iter()
        .map(|function| (function.id(), function.entry()))
        .collect::<HashMap<_, _>>();
    let rewritten_by_id = rewritten
        .iter()
        .map(|continuation| (continuation.id(), continuation))
        .collect::<HashMap<_, _>>();
    let reachable = reachable_continuations(&functions, &rewritten_by_id, &function_entries);
    let rewritten_count = rewritten.len();
    let optimized_continuations = rewritten
        .into_iter()
        .filter(|continuation| reachable.contains(&continuation.id()))
        .collect::<Vec<_>>();

    let empty_gotos_before = empty_targets.len();
    let empty_gotos_after = optimized_continuations
        .iter()
        .filter(|continuation| {
            continuation.body().is_empty()
                && matches!(continuation.terminator(), Terminator::Goto { .. })
        })
        .count();
    let mut functions = functions;
    let mut optimized_continuations = optimized_continuations;
    let ids_compacted = compact_ids(&mut functions, &mut optimized_continuations);
    let stats = ContinuationOptimizationStats {
        continuations_before: continuations.len(),
        continuations_after: optimized_continuations.len(),
        empty_gotos_before,
        empty_gotos_after,
        empty_gotos_threaded: remap
            .iter()
            .filter(|(source, target)| **source != **target)
            .filter(|(source, _)| empty_targets.contains_key(source))
            .count(),
        unreachable_continuations_removed: rewritten_count - optimized_continuations.len(),
        successor_references_rewritten,
        function_entries_rewritten,
        continuation_ids_compacted: ids_compacted,
    };

    let optimized = ContinuationProgram::new_with_globals(
        program.main(),
        program.globals().to_vec(),
        functions,
        optimized_continuations,
    )?;
    Ok((optimized, stats))
}

fn compact_ids(functions: &mut [FunctionDescriptor], continuations: &mut [Continuation]) -> bool {
    let remap = continuations
        .iter()
        .enumerate()
        .map(|(index, continuation)| {
            let new_id = ContinuationId::new(u16::try_from(index + 1).expect("continuation count"))
                .expect("continuation ID zero is reserved");
            (continuation.id(), new_id)
        })
        .collect::<HashMap<_, _>>();
    let changed = continuations
        .iter()
        .enumerate()
        .any(|(index, continuation)| {
            continuation.id()
                != ContinuationId::new(u16::try_from(index + 1).expect("continuation count"))
                    .expect("continuation ID zero is reserved")
        });
    if !changed {
        return false;
    }

    for function in functions {
        *function = function.clone().with_entry(remap[&function.entry()]);
    }
    for continuation in continuations.iter_mut() {
        let mut terminator = continuation.terminator().clone();
        let mut ignored = 0;
        map_successor_references(&mut terminator, &remap, &mut ignored);
        *continuation = Continuation::new(
            remap[&continuation.id()],
            continuation.function(),
            continuation.body().to_vec(),
            terminator,
        );
    }
    true
}

struct TargetResolver<'a> {
    continuations: &'a HashMap<ContinuationId, &'a Continuation>,
    empty_targets: &'a HashMap<ContinuationId, ContinuationId>,
    function_entries: &'a HashMap<FunctionId, ContinuationId>,
    memo: HashMap<ContinuationId, ContinuationId>,
    active: HashMap<ContinuationId, usize>,
    stack: Vec<ContinuationId>,
}

impl<'a> TargetResolver<'a> {
    fn new(
        continuations: &'a HashMap<ContinuationId, &'a Continuation>,
        empty_targets: &'a HashMap<ContinuationId, ContinuationId>,
        function_entries: &'a HashMap<FunctionId, ContinuationId>,
    ) -> Self {
        Self {
            continuations,
            empty_targets,
            function_entries,
            memo: HashMap::new(),
            active: HashMap::new(),
            stack: Vec::new(),
        }
    }

    fn resolve(&mut self, id: ContinuationId) -> ContinuationId {
        if let Some(&resolved) = self.memo.get(&id) {
            return resolved;
        }
        if let Some(&cycle_start) = self.active.get(&id) {
            let cycle = &self.stack[cycle_start..];
            let function = self.continuations[&id].function();
            let representative = cycle
                .iter()
                .copied()
                .find(|candidate| self.function_entries.get(&function) == Some(candidate))
                .unwrap_or_else(|| cycle.iter().copied().min().expect("non-empty cycle"));
            for &member in cycle {
                self.memo.insert(member, representative);
            }
            return representative;
        }
        let Some(&target) = self.empty_targets.get(&id) else {
            self.memo.insert(id, id);
            return id;
        };
        self.active.insert(id, self.stack.len());
        self.stack.push(id);
        let resolved = self.resolve(target);
        self.stack.pop();
        self.active.remove(&id);
        self.memo.insert(id, resolved);
        resolved
    }
}

fn map_successor_references(
    terminator: &mut Terminator,
    remap: &HashMap<ContinuationId, ContinuationId>,
    changed: &mut usize,
) {
    let map = |target: &mut ContinuationId, changed: &mut usize| {
        let original = *target;
        *target = remap[&original];
        if *target != original {
            *changed += 1;
        }
    };
    match terminator {
        Terminator::Goto { target } => map(target, changed),
        Terminator::Branch {
            then_target,
            else_target,
            ..
        } => {
            map(then_target, changed);
            map(else_target, changed);
        }
        Terminator::Call { return_to, .. }
        | Terminator::ArrayLoad { return_to, .. }
        | Terminator::ArrayStore { return_to, .. }
        | Terminator::AggregateLoad { return_to, .. }
        | Terminator::AggregateStore { return_to, .. } => map(return_to, changed),
        Terminator::Return { .. } | Terminator::Abort | Terminator::Halt => {}
    }
}

fn reachable_continuations(
    functions: &[FunctionDescriptor],
    continuations: &HashMap<ContinuationId, &Continuation>,
    function_entries: &HashMap<FunctionId, ContinuationId>,
) -> HashSet<ContinuationId> {
    let mut reachable = HashSet::new();
    let mut pending = functions
        .iter()
        .map(FunctionDescriptor::entry)
        .collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let continuation = continuations[&id];
        match continuation.terminator() {
            Terminator::Goto { target } => pending.push(*target),
            Terminator::Branch {
                then_target,
                else_target,
                ..
            } => pending.extend([*then_target, *else_target]),
            Terminator::Call {
                callee, return_to, ..
            } => {
                pending.push(*return_to);
                pending.push(function_entries[callee]);
            }
            Terminator::ArrayLoad { return_to, .. }
            | Terminator::ArrayStore { return_to, .. }
            | Terminator::AggregateLoad { return_to, .. }
            | Terminator::AggregateStore { return_to, .. } => pending.push(*return_to),
            Terminator::Return { .. } | Terminator::Abort | Terminator::Halt => {}
        }
    }
    reachable
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuation_ir::{Address, FrameInstruction, FrameSlot, ValueType};

    fn id(value: u16) -> ContinuationId {
        ContinuationId::new(value).unwrap()
    }

    fn empty_goto(id_value: u16, target: u16) -> Continuation {
        Continuation::new(
            id(id_value),
            FunctionId::new(0),
            Vec::new(),
            Terminator::Goto { target: id(target) },
        )
    }

    #[test]
    fn threads_empty_gotos_and_updates_function_entry() {
        let function = FunctionDescriptor::new(
            FunctionId::new(0),
            Vec::<FrameSlot>::new(),
            1,
            ValueType::Void,
            id(1),
        );
        let program = ContinuationProgram::new(
            FunctionId::new(0),
            vec![function],
            vec![
                empty_goto(1, 2),
                empty_goto(2, 3),
                Continuation::new(
                    id(3),
                    FunctionId::new(0),
                    vec![FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: 7,
                    }],
                    Terminator::Halt,
                ),
            ],
        )
        .unwrap();

        let (optimized, stats) = optimize_continuations(&program).unwrap();
        assert_eq!(optimized.functions()[0].entry(), id(1));
        assert_eq!(optimized.continuations().len(), 1);
        assert_eq!(stats.empty_gotos_threaded, 2);
        assert_eq!(stats.continuations_removed(), 2);
        assert!(stats.continuation_ids_compacted);
    }

    #[test]
    fn keeps_one_node_for_an_empty_goto_cycle() {
        let function = FunctionDescriptor::new(
            FunctionId::new(0),
            Vec::<FrameSlot>::new(),
            0,
            ValueType::Void,
            id(1),
        );
        let program = ContinuationProgram::new(
            FunctionId::new(0),
            vec![function],
            vec![empty_goto(1, 2), empty_goto(2, 1)],
        )
        .unwrap();

        let (optimized, stats) = optimize_continuations(&program).unwrap();
        assert_eq!(optimized.functions()[0].entry(), id(1));
        assert_eq!(optimized.continuations().len(), 1);
        assert_eq!(
            optimized.continuations()[0].terminator(),
            &Terminator::Goto { target: id(1) }
        );
        assert_eq!(stats.empty_gotos_after, 1);
    }
}
