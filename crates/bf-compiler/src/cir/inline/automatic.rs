//! Trial allocation includes backend region gates and portal scratch. Global
//! navigation traverses the caller's persistent frame, so transitive global
//! users may not grow that frame. Global-free callers may grow within ABI limits.

use super::*;
use crate::ContinuationOptimizationOptions;
use crate::cir::analysis::call_graph::callee_ranks;
use crate::cir::effects::{self, Effect};

#[derive(Clone, Copy)]
enum Selection {
    Automatic,
    Closed {
        allow_fixed_growth: bool,
        max_weight: usize,
        forward_wrappers: bool,
    },
}

fn closed_functions(graph: &Graph) -> HashSet<FunctionId> {
    let mut closed: HashSet<_> = graph.nodes.iter().map(Continuation::function).collect();
    for c in &graph.nodes {
        if matches!(
            c.terminator().boundary(),
            crate::cir::ir::BoundaryKind::Hard
        ) {
            closed.remove(&c.function());
        }
    }
    closed
}

fn dispatch_entries(program: &ContinuationProgram) -> HashMap<FunctionId, usize> {
    let plan = crate::backend::regions::RegionPlan::new(program);
    let mut counts = HashMap::new();
    for id in plan.entries {
        *counts
            .entry(program.continuation(id).unwrap().function())
            .or_default() += 1;
    }
    counts
}

fn visit_body(body: &[I], visit: &mut impl FnMut(Effect)) {
    let mut pending = vec![body];
    while let Some(body) = pending.pop() {
        for i in body {
            effects::instruction(i, &mut *visit);
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
        effects::terminator(c.terminator(), inspect);
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

/// Give each closed global context its own allowance, measured before any
/// splice. A growing callee must not exhaust the allowance of unrelated global
/// callers. Dynamic callers retain the shared work limit. Failed trials are
/// charged too; repeatedly rejecting large templates still costs compiler work.
struct WorkBudgets {
    shared_limit: usize,
    shared_spent: usize,
    fixed: HashMap<FunctionId, (usize, usize)>,
}

impl WorkBudgets {
    fn new(program: &ContinuationProgram, shared_limit: usize) -> Self {
        let contexts = crate::cir::analysis::call_graph::global_context_functions(program);
        let mut weights = HashMap::<FunctionId, usize>::new();
        for continuation in program.continuations() {
            if contexts.contains(&continuation.function()) {
                let total = weights.entry(continuation.function()).or_default();
                *total = total.saturating_add(weight(std::iter::once(continuation)));
            }
        }
        let fixed = weights
            .into_iter()
            .map(|(function, weight)| {
                let limit = weight
                    .saturating_mul(8)
                    .clamp(1024, 1_000_000)
                    .min(shared_limit);
                (function, (limit, 0))
            })
            .collect();
        Self {
            shared_limit,
            shared_spent: 0,
            fixed,
        }
    }

    fn charge(&mut self, caller: FunctionId, work: usize) -> bool {
        let (limit, spent) = match self.fixed.get_mut(&caller) {
            Some((limit, spent)) => (*limit, spent),
            None => (self.shared_limit, &mut self.shared_spent),
        };
        if spent.saturating_add(work) > limit {
            return false;
        }
        *spent += work;
        true
    }
}

fn allocated_costs(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
) -> Result<HashMap<FunctionId, usize>, String> {
    let (allocated, _) = crate::cir::pipeline::optimize_for_inline_cost(program, options)
        .map_err(|e| e.to_string())?;
    crate::backend::layout_plan::estimated_frame_chunks(&allocated).map_err(|e| e.to_string())
}

fn caller_cost(
    graph: &Graph,
    original: &ContinuationProgram,
    caller: FunctionId,
    options: ContinuationOptimizationOptions,
    global_portal: bool,
    selection: Selection,
) -> Result<(usize, usize, ContinuationProgram), String> {
    // A splice changes only its caller. Callee signatures remain available
    // for validation, but other bodies are stubbed before cleanup/allocation.
    let projection = graph.clone().materialize_caller(original, caller)?;
    let projection =
        crate::cir::virtual_cleanup::cleanup(&projection).map_err(|e| e.to_string())?;
    let (allocated, _) = match selection {
        Selection::Automatic => {
            crate::cir::pipeline::optimize_for_inline_cost(&projection, options)
        }
        Selection::Closed { .. } => {
            crate::cir::pipeline::optimize_and_allocate(&projection, options)
        }
    }
    .map_err(|e| e.to_string())?;
    let costs =
        crate::backend::layout_plan::estimated_frame_chunks_with_route(&allocated, global_portal)
            .map_err(|e| e.to_string())?;
    let entries = match selection {
        Selection::Automatic => 0,
        Selection::Closed { .. } => dispatch_entries(&allocated)[&caller],
    };
    Ok((costs[&caller], entries, projection))
}

pub(crate) fn inline_automatic(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
) -> Result<(ContinuationProgram, InlineStats), String> {
    // Bound compiler work against exponential call-DAG expansion. Closed global
    // contexts receive separate, source-sized limits inside inline_with_budget.
    // This is not a BF-size profitability test; ordinary code growth is allowed.
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
    inline_with_selection(program, options, budget, Selection::Automatic)
}

/// Trial only callees without Call/portal boundaries. Soft bodies may be
/// duplicated, but no hard resume is cloned. Run the existing value passes
/// before measuring layout and reject newly required dispatcher roots.
pub(crate) fn inline_closed(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
    allow_fixed_growth: bool,
    max_weight: usize,
) -> Result<(ContinuationProgram, InlineStats), String> {
    let budget = weight(program.continuations().iter())
        .saturating_mul(8)
        .clamp(8192, 1_000_000);
    inline_with_selection(
        program,
        options,
        budget,
        Selection::Closed {
            allow_fixed_growth,
            max_weight,
            forward_wrappers: false,
        },
    )
}

pub(crate) fn inline_forwarding(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
    max_weight: usize,
) -> Result<(ContinuationProgram, InlineStats), String> {
    let budget = weight(program.continuations().iter())
        .saturating_mul(8)
        .clamp(8192, 1_000_000);
    inline_with_selection(
        program,
        options,
        budget,
        Selection::Closed {
            allow_fixed_growth: true,
            max_weight,
            forward_wrappers: true,
        },
    )
}

fn inline_with_selection(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
    budget: usize,
    selection: Selection,
) -> Result<(ContinuationProgram, InlineStats), String> {
    let cleaned = crate::cir::virtual_cleanup::cleanup(program).map_err(|e| e.to_string())?;
    let mut stats = InlineStats::default();
    let Ok(mut costs) = allocated_costs(&cleaned, options) else {
        // Optimization must not prevent a caller from selecting another backend
        // configuration (e.g. imported/unbounded programs). Its normal codegen
        // path remains responsible for reporting any actual layout error.
        return Ok((cleaned, stats));
    };
    let global = global_users(&cleaned);
    let is_closed = matches!(selection, Selection::Closed { .. });
    let mut entries = if is_closed {
        let (allocated, _) = crate::cir::pipeline::optimize_and_allocate(&cleaned, options)
            .map_err(|e| e.to_string())?;
        costs = crate::backend::layout_plan::estimated_frame_chunks(&allocated)
            .map_err(|e| e.to_string())?;
        dispatch_entries(&allocated)
    } else {
        HashMap::new()
    };
    let fixed = if matches!(
        selection,
        Selection::Closed {
            allow_fixed_growth: true,
            ..
        }
    ) {
        crate::cir::analysis::call_graph::global_context_functions(&cleaned)
    } else {
        HashSet::new()
    };
    let recursive = recursive_functions(&cleaned);
    let forward_wrappers = matches!(
        selection,
        Selection::Closed {
            forward_wrappers: true,
            ..
        }
    );
    let stops = if forward_wrappers {
        crate::cir::analysis::call_graph::recursive_roots(&cleaned)
    } else {
        recursive.clone()
    };
    let ranks = callee_ranks(&cleaned);
    let mut budgets = WorkBudgets::new(&cleaned, budget);
    let mut graph = Graph::normalize(&cleaned)?;
    let mut eligible = if is_closed {
        closed_functions(&graph)
    } else {
        HashSet::new()
    };
    if forward_wrappers {
        eligible.extend(
            graph
                .functions
                .iter()
                .filter(|f| graph.forwards_calls(f.id()))
                .map(FunctionDescriptor::id),
        );
    }
    let mut skipped = HashSet::new();
    loop {
        let reachable = graph.reachable(cleaned.main());
        let candidate = graph
            .nodes
            .iter()
            .filter(|c| reachable.contains(&c.id()) && !skipped.contains(&c.id()))
            .filter_map(|c| c.terminator().callee().map(|callee| (c.id(), callee)))
            .filter(|&(_, callee)| !is_closed || eligible.contains(&callee))
            .min_by_key(|&(site, callee)| (ranks[&callee], site));
        let Some((site, callee)) = candidate else {
            break;
        };
        skipped.insert(site);
        let caller = graph
            .nodes
            .iter()
            .find(|c| c.id() == site)
            .unwrap()
            .function();
        if stops.contains(&callee) || caller == callee {
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
        if matches!(selection, Selection::Closed { max_weight, .. } if work > max_weight) {
            stats.work_limit_calls_preserved += 1;
            continue;
        }
        if !budgets.charge(caller, work) {
            stats.work_limit_calls_preserved += 1;
            continue;
        }
        let mut candidate = graph.trial(caller, callee);
        let copied = if forward_wrappers {
            candidate.splice_forwarding(site)?
        } else {
            candidate.splice(site)?
        };
        let Ok((new_cost, new_entries, projection)) = caller_cost(
            &candidate,
            &cleaned,
            caller,
            options,
            crate::backend::layout_plan::has_global_portal(&cleaned),
            selection,
        ) else {
            stats.frame_limit_calls_preserved += 1;
            continue;
        };
        let grow = matches!(
            selection,
            Selection::Closed {
                allow_fixed_growth: true,
                ..
            }
        ) && fixed.contains(&caller);
        if (global.contains(&caller) && !grow && new_cost > costs[&caller])
            || (is_closed && new_entries > entries[&caller])
        {
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
        if is_closed {
            let closed = graph
                .nodes
                .iter()
                .filter(|c| c.function() == caller)
                .all(|c| c.terminator().boundary() != crate::cir::ir::BoundaryKind::Hard);
            if closed || (forward_wrappers && graph.forwards_calls(caller)) {
                eligible.insert(caller);
            } else {
                eligible.remove(&caller);
            }
        }
        #[cfg(test)]
        {
            let materialized = graph.clone().materialize(&cleaned)?;
            let materialized =
                crate::cir::virtual_cleanup::cleanup(&materialized).map_err(|e| e.to_string())?;
            if is_closed {
                let (allocated, _) =
                    crate::cir::pipeline::optimize_and_allocate(&materialized, options)
                        .map_err(|e| e.to_string())?;
                let full_costs = crate::backend::layout_plan::estimated_frame_chunks_with_route(
                    &allocated,
                    crate::backend::layout_plan::has_global_portal(&cleaned),
                )
                .map_err(|e| e.to_string())?;
                assert_eq!(
                    new_cost, full_costs[&caller],
                    "closed caller cost must match full program layout"
                );
                assert_eq!(
                    new_entries,
                    dispatch_entries(&allocated)[&caller],
                    "closed caller roots must match full program layout"
                );
            } else {
                assert_eq!(
                    new_cost,
                    allocated_costs(&materialized, options)?[&caller],
                    "isolated caller cost must match full program layout"
                );
            }
        }
        costs.insert(caller, new_cost);
        if is_closed {
            entries.insert(caller, new_entries);
        }
        stats.calls_inlined += 1;
        stats.blocks_cloned += copied;
    }
    if stats.calls_inlined == 0 {
        return Ok((cleaned, stats));
    }
    let materialized = graph.materialize(&cleaned)?;
    let result = crate::cir::virtual_cleanup::cleanup(&materialized).map_err(|e| e.to_string())?;
    Ok((result, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> ContinuationProgram {
        let ast =
            crate::frontend::parser::parse(crate::frontend::lexer::lex(text).unwrap()).unwrap();
        let hir = crate::frontend::semantic::analyze(&ast).unwrap();
        crate::frontend::lowering::lower_hir_unallocated(&hir).unwrap()
    }

    #[test]
    fn closed_global_contexts_have_independent_work_limits() {
        let program = source(
            "cell g; cell leaf(cell x) { return x+g; } cell left(cell x) { return leaf(x); } cell right(cell x) { return leaf(x); } cell rec(cell n) { if(n) { return rec(n-1)+1; } return 0; } void main() { g=input(); output(left(input())); output(right(input())); output(rec(input())); }",
        );
        let function = |name| {
            program
                .functions()
                .iter()
                .find(|f| f.name() == Some(name))
                .unwrap()
                .id()
        };
        let mut budgets = WorkBudgets::new(&program, 8);
        assert!(budgets.charge(function("left"), 8));
        assert!(!budgets.charge(function("left"), 1));
        assert!(budgets.charge(function("right"), 8));
        assert!(!budgets.charge(function("right"), 1));
        // main reaches recursion, so it and rec share the dynamic allowance.
        assert!(budgets.charge(program.main(), 8));
        assert!(!budgets.charge(function("rec"), 1));
        let mut disabled = WorkBudgets::new(&program, 0);
        assert!(!disabled.charge(function("left"), 1));
        assert!(!disabled.charge(program.main(), 1));
    }

    #[test]
    fn exhausting_one_global_caller_does_not_prevent_other_callers_from_inlining() {
        let program = source(
            "cell g; cell leaf(cell x) { return x+g; } cell left(cell x) { return leaf(x); } cell right(cell x) { return leaf(x); } cell rec(cell n) { if(n) { return rec(n-1)+1; } return 0; } void main() { g=input(); output(left(input())); output(right(input())); output(rec(input())); }",
        );
        let leaf = program
            .functions()
            .iter()
            .find(|f| f.name() == Some("leaf"))
            .unwrap()
            .id();
        let budget = weight(
            program
                .continuations()
                .iter()
                .filter(|c| c.function() == leaf),
        );
        let (after, stats) = inline_with_budget(&program, Default::default(), budget).unwrap();
        assert!(stats.calls_inlined >= 2);
        assert!(stats.work_limit_calls_preserved > 0);
        assert!(
            after
                .continuations()
                .iter()
                .all(|c| c.terminator().callee() != Some(leaf))
        );
        for g in [0u8, 1, 128, 255] {
            let mut output = Vec::new();
            crate::run_continuations_with_io(
                &after,
                &mut &[g, 4, 8, 3][..],
                &mut output,
                Default::default(),
                |_| {},
            )
            .unwrap();
            assert_eq!(output, [g.wrapping_add(4), g.wrapping_add(8), 3]);
        }
    }

    #[test]
    fn automatic_inline_retains_recursive_calls_and_respects_work_budget() {
        let ast = crate::frontend::parser::parse(crate::frontend::lexer::lex(
            "cell leaf(cell n) { return n+1; } cell rec(cell n) { if(n) { return rec(n-1)+1; } return 0; } void main() { output(leaf(input())); output(rec(input())); }",
        ).unwrap()).unwrap();
        let hir = crate::frontend::semantic::analyze(&ast).unwrap();
        let program = crate::frontend::lowering::lower_hir_unallocated(&hir).unwrap();
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
