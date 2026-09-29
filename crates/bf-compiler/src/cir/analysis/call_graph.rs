//! Shared call-graph analysis for inlining and frame placement.

use crate::{ContinuationProgram, FunctionId};
use std::collections::{HashMap, HashSet};

fn call_edges(program: &ContinuationProgram) -> HashMap<FunctionId, Vec<FunctionId>> {
    let mut edges = HashMap::<FunctionId, Vec<FunctionId>>::new();
    for c in program.continuations() {
        if let Some(callee) = c.terminator().callee() {
            edges.entry(c.function()).or_default().push(callee);
        }
    }
    edges
}

/// Return functions on a call cycle, excluding callers that only reach one.
pub(crate) fn recursive_functions(program: &ContinuationProgram) -> HashSet<FunctionId> {
    let edges = call_edges(program);
    program
        .functions()
        .iter()
        .filter_map(|f| {
            let mut visited = HashSet::new();
            let mut pending = edges.get(&f.id()).cloned().unwrap_or_default();
            while let Some(id) = pending.pop() {
                if id == f.id() {
                    return Some(id);
                }
                if visited.insert(id) {
                    pending.extend(edges.get(&id).into_iter().flatten().copied());
                }
            }
            None
        })
        .collect()
}

/// Preserve deterministic callee-first traversal order when selecting inline sites.
pub(crate) fn callee_ranks(program: &ContinuationProgram) -> HashMap<FunctionId, usize> {
    let edges = call_edges(program);
    let mut visited = HashSet::new();
    let mut rank = HashMap::new();
    for f in program.functions() {
        let mut pending = vec![(f.id(), false)];
        while let Some((id, expanded)) = pending.pop() {
            if expanded {
                let next = rank.len();
                rank.insert(id, next);
            } else if visited.insert(id) {
                pending.push((id, true));
                pending.extend(
                    edges
                        .get(&id)
                        .into_iter()
                        .flatten()
                        .rev()
                        .map(|&callee| (callee, false)),
                );
            }
        }
    }
    rank
}
