//! Inline single-use void functions when allocation keeps the caller frame compact.

use crate::hir::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CallSite {
    caller: FunctionId,
    callee: FunctionId,
}

struct CallGraph {
    calls: Vec<CallSite>,
    incoming: Vec<Vec<usize>>,
    outgoing: Vec<Vec<usize>>,
    reachable: crate::hir_reachability::ReachableFunctions,
}

impl CallGraph {
    fn build(hir: &HirProgram) -> Self {
        let reachable = crate::hir_reachability::ReachableFunctions::analyze(hir);
        let mut calls = Vec::new();
        for function in &hir.functions {
            if reachable.contains(function.id) {
                crate::hir_reachability::visit_statement_calls(&function.body, &mut |callee| {
                    calls.push(CallSite {
                        caller: function.id,
                        callee,
                    });
                });
            }
        }
        for global in &hir.globals {
            if let Some(initializer) = &global.initializer {
                crate::hir_reachability::visit_expression_calls(initializer, &mut |callee| {
                    calls.push(CallSite {
                        caller: hir.entry,
                        callee,
                    });
                });
            }
        }

        let mut incoming = vec![vec![]; hir.functions.len()];
        let mut outgoing = vec![vec![]; hir.functions.len()];

        for (site_idx, call) in calls.iter().enumerate() {
            incoming[call.callee.index()].push(site_idx);
            outgoing[call.caller.index()].push(site_idx);
        }

        CallGraph {
            calls,
            incoming,
            outgoing,
            reachable,
        }
    }

    fn check_recursive(&self, function: FunctionId) -> bool {
        let mut visited = vec![false; self.outgoing.len()];
        let mut pending = vec![function];
        while let Some(current) = pending.pop() {
            if std::mem::replace(&mut visited[current.index()], true) {
                continue;
            }
            for &site in &self.outgoing[current.index()] {
                let callee = self.calls[site].callee;
                if callee == function {
                    return true;
                }
                pending.push(callee);
            }
        }
        false
    }
}

fn contains_return(stmt: &HirStatement) -> bool {
    match &stmt.kind {
        HirStatementKind::Block(hir_statements) => hir_statements.iter().any(contains_return),
        HirStatementKind::Return(_) => true,
        HirStatementKind::If {
            condition: _,
            then_branch,
            else_branch,
        } => {
            contains_return(then_branch) || else_branch.as_ref().is_some_and(|e| contains_return(e))
        }
        HirStatementKind::While { condition: _, body } => contains_return(body),
        _ => false,
    }
}

// Semantic analysis appends a final return to void functions. Only strip that
// return when no earlier statement can return from the caller after inlining.
fn take_simple_last_return(body: &mut HirStatement) -> Option<Option<HirExpression>> {
    if matches!(body.kind, HirStatementKind::Return(_)) {
        let old = std::mem::replace(&mut body.kind, HirStatementKind::Empty);
        let HirStatementKind::Return(value) = old else {
            unreachable!();
        };
        return Some(value);
    }

    let HirStatementKind::Block(stmt) = &mut body.kind else {
        return None;
    };

    let (last, prefix) = stmt.split_last_mut()?;

    if prefix.iter().any(contains_return) {
        return None;
    }

    take_simple_last_return(last)
}

fn import_locals(caller: &HirFunction, callee: &HirFunction) -> (Vec<HirLocal>, Vec<LocalId>) {
    let mut caller_import = vec![];
    let mut map = vec![LocalId::new(0); callee.locals.len()];

    for (i, local) in callee.locals.iter().enumerate() {
        let new_id = LocalId::new(caller.locals.len() + i);

        let mut new_local = local.clone();
        new_local.id = new_id;
        new_local.name = format!("{}#inline_fn_{}", new_local.name, callee.name);

        caller_import.push(new_local);
        map[local.id.index()] = new_id;
    }

    (caller_import, map)
}

fn remap_place(place: &mut HirPlace, map: &[LocalId]) {
    if let VariableRef::Local(local) = &mut place.root {
        *local = map[local.index()];
    }

    for projection in place.projections.iter_mut() {
        remap_projection(projection, map);
    }
}

fn remap_projection(proj: &mut Projection, map: &[LocalId]) {
    if let Projection::Index {
        index: ArrayIndex::Dynamic(index),
        ..
    } = proj
    {
        remap_expr(index, map);
    }
}

fn remap_expr(expr: &mut HirExpression, map: &[LocalId]) {
    match &mut expr.kind {
        HirExpressionKind::Literal(_) => {}
        HirExpressionKind::EnumVariant(_) => {}
        HirExpressionKind::StringLiteral(_) => {}
        HirExpressionKind::Place(hir_place) => {
            remap_place(hir_place, map);
        }
        HirExpressionKind::Project { base, projections } => {
            remap_expr(base, map);
            for proj in projections.iter_mut() {
                remap_projection(proj, map);
            }
        }
        HirExpressionKind::Input => {}
        HirExpressionKind::Unary {
            operator: _,
            operand,
        } => {
            remap_expr(operand, map);
        }
        HirExpressionKind::Binary {
            operator: _,
            left,
            right,
        } => {
            remap_expr(left, map);
            remap_expr(right, map);
        }
        HirExpressionKind::Call {
            function: _,
            arguments,
        } => {
            for arg in arguments.iter_mut() {
                remap_expr(arg, map);
            }
        }
    }
}

fn remap_stmt(stmt: &mut HirStatement, map: &[LocalId]) {
    match &mut stmt.kind {
        HirStatementKind::Empty => {}
        HirStatementKind::Block(hir_statements) => {
            for s in hir_statements.iter_mut() {
                remap_stmt(s, map);
            }
        }
        HirStatementKind::Declaration { local, initializer } => {
            *local = map[local.index()];

            if let Some(init) = initializer {
                remap_expr(init, map);
            }
        }
        HirStatementKind::Assignment {
            value,
            target,
            operator: _,
        } => {
            remap_expr(value, map);
            remap_place(target, map);
        }
        HirStatementKind::Output(hir_expression) => {
            remap_expr(hir_expression, map);
        }
        HirStatementKind::Call {
            function: _,
            arguments,
        } => {
            for arg in arguments.iter_mut() {
                remap_expr(arg, map);
            }
        }
        HirStatementKind::Abort => {}
        HirStatementKind::Return(hir_expression) => {
            if let Some(value) = hir_expression {
                remap_expr(value, map);
            }
        }
        HirStatementKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            remap_expr(condition, map);
            remap_stmt(then_branch, map);
            if let Some(else_stmt) = else_branch {
                remap_stmt(else_stmt, map);
            }
        }
        HirStatementKind::While { condition, body } => {
            remap_expr(condition, map);
            remap_stmt(body, map);
        }
    }
}

fn replace_call_stmt(
    stmt: &mut HirStatement,
    inline_fn: &HirFunction,
    map: &[LocalId],
    remapped_body: &HirStatement,
) -> bool {
    // Semantic analysis only permits void calls in statement position.
    match &mut stmt.kind {
        HirStatementKind::Block(hir_statements) => {
            for s in hir_statements.iter_mut() {
                if replace_call_stmt(s, inline_fn, map, remapped_body) {
                    return true;
                }
            }
            false
        }

        HirStatementKind::If {
            condition: _,
            then_branch,
            else_branch,
        } => {
            if replace_call_stmt(then_branch, inline_fn, map, remapped_body) {
                return true;
            }
            if let Some(else_branch) = else_branch
                && replace_call_stmt(else_branch, inline_fn, map, remapped_body)
            {
                return true;
            }
            false
        }

        HirStatementKind::While { body, .. } => {
            replace_call_stmt(body, inline_fn, map, remapped_body)
        }

        HirStatementKind::Call {
            function,
            arguments,
        } => {
            let mut statements = Vec::new();
            if *function != inline_fn.id {
                return false;
            }

            for (param, arg) in inline_fn.parameters.iter().zip(arguments) {
                statements.push(HirStatement {
                    kind: HirStatementKind::Declaration {
                        local: map[param.local.index()],
                        initializer: Some(arg.clone()),
                    },
                    offset: stmt.offset,
                });
            }
            statements.push(remapped_body.clone());

            let insert_stmt = HirStatement {
                kind: HirStatementKind::Block(statements),
                offset: stmt.offset,
            };

            *stmt = insert_stmt;
            true
        }

        _ => false,
    }
}

// A larger caller frame stays allocated even outside the inlined call. In this
// ABI that makes every later global navigation traverse extra stack chunks.
// Only accept inlining when slot reuse absorbs the callee without increasing
// the caller's persistent frame at either supported chunk size.
fn inline_function(
    hir: &mut HirProgram,
    graph: &CallGraph,
    function: FunctionId,
    costs: &mut [Option<usize>],
) -> bool {
    if function == hir.entry
        || !graph.reachable.contains(function)
        || graph.incoming[function.index()].len() != 1
        || hir.functions[function.index()].signature.return_type != TypeId::VOID
        || graph.check_recursive(function)
    {
        return false;
    }
    let caller = graph.calls[graph.incoming[function.index()][0]].caller;
    let mut body = hir.functions[function.index()].body.clone();
    if !matches!(take_simple_last_return(&mut body), Some(None)) {
        return false;
    }
    let before = match costs[caller.index()] {
        Some(cost) => cost,
        None => {
            let Ok(cost) = crate::continuation_lowering::allocated_frame_chunks(hir, caller) else {
                return false;
            };
            costs[caller.index()] = Some(cost);
            cost
        }
    };
    let original = hir.functions[caller.index()].clone();
    let [caller_function, callee_function] = hir
        .functions
        .get_disjoint_mut([caller.index(), function.index()])
        .expect("distinct valid functions");
    let (locals, mapping) = import_locals(caller_function, callee_function);
    remap_stmt(&mut body, &mapping);
    if !replace_call_stmt(&mut caller_function.body, callee_function, &mapping, &body) {
        return false;
    }
    caller_function.locals.extend(locals);
    if let Ok(after) = crate::continuation_lowering::allocated_frame_chunks(hir, caller)
        && after <= before
    {
        costs[caller.index()] = Some(after);
        return true;
    }
    hir.functions[caller.index()] = original;
    false
}

pub(crate) fn inline_single_use_functions(hir: &mut HirProgram) {
    let mut costs = vec![None; hir.functions.len()];
    loop {
        let graph = CallGraph::build(hir);
        let changed = (0..hir.functions.len())
            .any(|index| inline_function(hir, &graph, FunctionId::new(index), &mut costs));
        if !changed {
            break;
        }
    }
}
