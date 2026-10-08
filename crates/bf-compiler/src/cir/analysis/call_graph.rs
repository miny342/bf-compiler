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

/// Functions that reach any seed, including the seeds themselves.
fn callers_of(
    edges: &HashMap<FunctionId, Vec<FunctionId>>,
    mut selected: HashSet<FunctionId>,
) -> HashSet<FunctionId> {
    let mut callers = HashMap::<FunctionId, Vec<FunctionId>>::new();
    for (&caller, callees) in edges {
        for &callee in callees {
            callers.entry(callee).or_default().push(caller);
        }
    }
    let mut pending = selected.iter().copied().collect::<Vec<_>>();
    while let Some(callee) = pending.pop() {
        for &caller in callers.get(&callee).into_iter().flatten() {
            if selected.insert(caller) {
                pending.push(caller);
            }
        }
    }
    selected
}

/// Closed, acyclic call subgraphs that access globals. Descendants without
/// globals join their parent's fixed context execution; callers that reach a
/// recursive SCC retain the dynamic stack. Selection is independent of physical
/// placement; the backend can share storage across unrelated activation paths.
pub(crate) fn global_context_functions(program: &ContinuationProgram) -> HashSet<FunctionId> {
    let edges = call_edges(program);
    let reaches_recursion = callers_of(&edges, recursive_functions(program));
    let mut globals = HashSet::new();
    for continuation in program.continuations() {
        let mut body = continuation.body().to_vec();
        let mut terminal = continuation.terminator().clone();
        let mut global = false;
        let mut visit = |address: &mut crate::Address| {
            global |= matches!(
                address,
                crate::Address::Global(_)
                    | crate::Address::ArrayElement {
                        array: crate::AggregateRegion::Global(_),
                        ..
                    }
            );
        };
        super::super::operands::map_body(&mut body, &mut visit);
        super::super::operands::map_terminator(&mut terminal, &mut visit);
        if global {
            globals.insert(continuation.function());
        }
    }
    let mut selected = callers_of(&edges, globals);
    selected.retain(|function| !reaches_recursion.contains(function));
    let mut pending = selected.iter().copied().collect::<Vec<_>>();
    while let Some(caller) = pending.pop() {
        for &callee in edges.get(&caller).into_iter().flatten() {
            if selected.insert(callee) {
                pending.push(callee);
            }
        }
    }
    selected
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
