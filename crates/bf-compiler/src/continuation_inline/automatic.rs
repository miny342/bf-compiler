//! Trial allocation includes backend region gates and portal scratch. Global
//! navigation traverses the caller's persistent frame, so transitive global
//! users may not grow that frame. Global-free callers may grow within ABI limits.

use super::*;
use crate::ContinuationOptimizationOptions;
use crate::continuation_effects::{self, Effect};

fn visit_body(body: &[I], visit: &mut impl FnMut(Effect)) {
    let mut pending = vec![body];
    while let Some(body) = pending.pop() {
        for i in body {
            continuation_effects::instruction(i, &mut *visit);
            match i {
                I::Loop { body, .. } => pending.push(body),
                I::Branch {
                    then_body,
                    else_body,
                    ..
                } => {
                    pending.push(then_body);
                    pending.push(else_body);
                }
                _ => {}
            }
        }
    }
}

fn global_operand(operand: ValueOperand) -> bool {
    matches!(
        operand,
        ValueOperand::Cell(Address::Global(_))
            | ValueOperand::Cell(Address::ArrayElement {
                array: AggregateRegion::Global(_),
                ..
            })
            | ValueOperand::Array(AggregateRegion::Global(_))
            | ValueOperand::Aggregate {
                region: AggregateRegion::Global(_),
                ..
            }
    )
}

fn global_users(program: &ContinuationProgram) -> HashSet<FunctionId> {
    let mut users = HashSet::new();
    let mut calls = Vec::new();
    for c in program.continuations() {
        let mut inspect = |effect| {
            let global = match effect {
                Effect::Read(operand) | Effect::Write(operand) => global_operand(operand),
                Effect::Clobber(address) => global_operand(ValueOperand::Cell(address)),
                Effect::MayWrite(region) => global_operand(ValueOperand::Array(region)),
                Effect::CallResult(callee) => {
                    calls.push((c.function(), callee));
                    false
                }
            };
            if global {
                users.insert(c.function());
            }
        };
        visit_body(c.body(), &mut inspect);
        continuation_effects::terminator(c.terminator(), inspect);
    }
    loop {
        let mut changed = false;
        for &(caller, callee) in &calls {
            if users.contains(&callee) {
                changed |= users.insert(caller);
            }
        }
        if !changed {
            return users;
        }
    }
}

fn ranks(program: &ContinuationProgram) -> HashMap<FunctionId, usize> {
    let mut edges = HashMap::<FunctionId, Vec<FunctionId>>::new();
    for c in program.continuations() {
        if let Some(callee) = c.terminator().callee() {
            edges.entry(c.function()).or_default().push(callee);
        }
    }
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

fn weight<'a>(nodes: impl Iterator<Item = &'a Continuation>) -> usize {
    nodes.fold(0usize, |sum, c| {
        let mut count = 1usize;
        let mut pending = vec![c.body()];
        while let Some(body) = pending.pop() {
            count = count.saturating_add(body.len());
            for i in body {
                match i {
                    I::Loop { body, .. } => pending.push(body),
                    I::Branch {
                        then_body,
                        else_body,
                        ..
                    } => {
                        pending.push(then_body);
                        pending.push(else_body);
                    }
                    _ => {}
                }
            }
        }
        sum.saturating_add(count)
    })
}

fn allocated_costs(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
) -> Result<HashMap<FunctionId, usize>, String> {
    let (allocated, _) =
        crate::continuation_pipeline::finish(program, options).map_err(|e| e.to_string())?;
    crate::abi_codegen::estimated_frame_chunks(&allocated).map_err(|e| e.to_string())
}

fn caller_cost(
    graph: &Graph,
    original: &ContinuationProgram,
    caller: FunctionId,
    options: ContinuationOptimizationOptions,
    global_portal: bool,
) -> Result<(usize, ContinuationProgram), String> {
    // A splice changes only its caller. Callee signatures remain available
    // for validation, but other bodies are stubbed before cleanup/allocation.
    let projection = graph.clone().materialize_caller(original, caller)?;
    let projection = crate::virtual_cleanup::cleanup(&projection).map_err(|e| e.to_string())?;
    let (allocated, _) =
        crate::continuation_pipeline::finish(&projection, options).map_err(|e| e.to_string())?;
    let costs = crate::abi_codegen::estimated_frame_chunks_with_route(&allocated, global_portal)
        .map_err(|e| e.to_string())?;
    Ok((costs[&caller], projection))
}

pub(crate) fn inline_automatic(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
) -> Result<(ContinuationProgram, InlineStats), String> {
    // Bound compiler work against exponential call-DAG expansion. This is not
    // a BF-size profitability test; ordinary code growth is allowed.
    let budget = weight(program.continuations().iter())
        .saturating_mul(8)
        .clamp(8192, 1_000_000);
    inline_with_budget(program, options, budget)
}

fn inline_with_budget(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
    budget: usize,
) -> Result<(ContinuationProgram, InlineStats), String> {
    let cleaned = crate::virtual_cleanup::cleanup(program).map_err(|e| e.to_string())?;
    let mut stats = InlineStats::default();
    let Ok(mut costs) = allocated_costs(&cleaned, options) else {
        // Optimization must not prevent a caller from selecting another backend
        // configuration (e.g. imported/unbounded programs). Its normal codegen
        // path remains responsible for reporting any actual layout error.
        return Ok((cleaned, stats));
    };
    let global = global_users(&cleaned);
    let recursive = recursive_functions(&cleaned);
    let ranks = ranks(&cleaned);
    let mut spent = 0usize;
    let mut graph = Graph::normalize(&cleaned)?;
    let mut skipped = HashSet::new();
    loop {
        let reachable = graph.reachable(cleaned.main());
        let candidate = graph
            .nodes
            .iter()
            .filter(|c| reachable.contains(&c.id()) && !skipped.contains(&c.id()))
            .filter_map(|c| c.terminator().callee().map(|callee| (c.id(), callee)))
            .min_by_key(|&(site, callee)| (ranks[&callee], site));
        let Some((site, callee)) = candidate else {
            break;
        };
        skipped.insert(site);
        if recursive.contains(&callee) {
            stats.recursive_calls_preserved += 1;
            continue;
        }
        let clones = graph
            .nodes
            .iter()
            .filter(|c| c.function() == callee)
            .count();
        if graph.nodes.len() + clones + graph.results.len() + clones > usize::from(u16::MAX) {
            stats.id_limit_calls_preserved += 1;
            continue;
        }
        let work = weight(graph.nodes.iter().filter(|c| c.function() == callee));
        if spent.saturating_add(work) > budget {
            stats.work_limit_calls_preserved += 1;
            continue;
        }
        spent += work;
        let caller = graph
            .nodes
            .iter()
            .find(|c| c.id() == site)
            .unwrap()
            .function();
        let mut candidate = graph.trial(caller, callee);
        let copied = candidate.splice(site)?;
        let Ok((new_cost, projection)) = caller_cost(
            &candidate,
            &cleaned,
            caller,
            options,
            crate::abi_codegen::has_global_portal(&cleaned),
        ) else {
            stats.frame_limit_calls_preserved += 1;
            continue;
        };
        if global.contains(&caller) && new_cost > costs[&caller] {
            stats.frame_limit_calls_preserved += 1;
            continue;
        }
        // Keep the accepted template compact too. Otherwise dead storage from
        // earlier splices is zeroed again when this caller is itself cloned,
        // causing quadratic initialization growth despite clean final output.
        let refreshed = Graph::normalize(&projection)?;
        let old_nodes: HashSet<_> = candidate
            .nodes
            .iter()
            .filter(|c| c.function() == caller)
            .map(Continuation::id)
            .collect();
        graph.nodes.retain(|c| c.function() != caller);
        graph.results.retain(|id, _| !old_nodes.contains(id));
        for c in refreshed
            .nodes
            .into_iter()
            .filter(|c| c.function() == caller)
        {
            if let Some(result) = refreshed.results.get(&c.id()) {
                graph.results.insert(c.id(), *result);
            }
            graph.nodes.push(c);
        }
        let descriptor = graph
            .functions
            .iter_mut()
            .find(|f| f.id() == caller)
            .unwrap();
        *descriptor = refreshed
            .functions
            .into_iter()
            .find(|f| f.id() == caller)
            .unwrap();
        graph.owners.insert(caller, refreshed.owners[&caller]);
        graph.ids.used.extend(candidate.ids.used);
        graph.ids.used.extend(refreshed.ids.used);
        #[cfg(test)]
        {
            let materialized = graph.clone().materialize(&cleaned)?;
            let materialized =
                crate::virtual_cleanup::cleanup(&materialized).map_err(|e| e.to_string())?;
            assert_eq!(
                new_cost,
                allocated_costs(&materialized, options)?[&caller],
                "isolated caller cost must match full program layout"
            );
        }
        costs.insert(caller, new_cost);
        stats.calls_inlined += 1;
        stats.blocks_cloned += copied;
    }
    if stats.calls_inlined == 0 {
        return Ok((cleaned, stats));
    }
    let materialized = graph.materialize(&cleaned)?;
    let result = crate::virtual_cleanup::cleanup(&materialized).map_err(|e| e.to_string())?;
    Ok((result, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_inline_retains_recursive_calls_and_respects_work_budget() {
        let ast = crate::parser::parse(crate::lexer::lex(
            "cell leaf(cell n) { return n+1; } cell rec(cell n) { if(n) { return rec(n-1)+1; } return 0; } void main() { output(leaf(input())); output(rec(input())); }",
        ).unwrap()).unwrap();
        let hir = crate::semantic::analyze(&ast).unwrap();
        let program = crate::continuation_lowering::lower_hir_unallocated(&hir).unwrap();
        for (budget, expected_inline) in [(0, 0), (8192, 1)] {
            let (after, stats) = inline_with_budget(&program, Default::default(), budget).unwrap();
            assert_eq!(stats.calls_inlined, expected_inline);
            assert_eq!(stats.recursive_calls_preserved, 2);
            assert_eq!(stats.work_limit_calls_preserved, usize::from(budget == 0));
            let mut output = Vec::new();
            crate::run_continuations_with_io(
                &after,
                &mut &[4, 3][..],
                &mut output,
                Default::default(),
                |_| {},
            )
            .unwrap();
            assert_eq!(output, [5, 3]);
        }
    }
}
