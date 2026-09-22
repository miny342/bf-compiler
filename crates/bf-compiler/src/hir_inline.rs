//! Inline small functions when allocation keeps the caller frame compact.

use crate::hir;
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
    global_use: Vec<bool>,
}

struct FrameOnlyContext<'a> {
    types: &'a TypeTable,
    globals: &'a [HirGlobal],
}

fn projection_uses_global(projection: &Projection) -> bool {
    match projection {
        Projection::Field { .. }
        | Projection::Index {
            index: ArrayIndex::Constant(_),
            ..
        } => false,
        Projection::Index {
            index: ArrayIndex::Dynamic(index),
            ..
        } => expression_uses_global(index),
    }
}

fn place_uses_global(place: &HirPlace) -> bool {
    matches!(place.root, VariableRef::Global(_))
        || place.projections.iter().any(projection_uses_global)
}

fn expression_uses_global(expression: &HirExpression) -> bool {
    match &expression.kind {
        HirExpressionKind::Literal(_)
        | HirExpressionKind::EnumVariant(_)
        | HirExpressionKind::StringLiteral(_)
        | HirExpressionKind::Input => false,
        HirExpressionKind::Place(place) => place_uses_global(place),
        HirExpressionKind::Project { base, projections } => {
            expression_uses_global(base) || projections.iter().any(projection_uses_global)
        }
        HirExpressionKind::Call { arguments, .. } => arguments.iter().any(expression_uses_global),
        HirExpressionKind::Unary { operand, .. } => expression_uses_global(operand),
        HirExpressionKind::Binary { left, right, .. } => {
            expression_uses_global(left) || expression_uses_global(right)
        }
    }
}

fn statement_uses_global(statement: &HirStatement) -> bool {
    match &statement.kind {
        HirStatementKind::Empty | HirStatementKind::Abort => false,
        HirStatementKind::Block(statements) => statements.iter().any(statement_uses_global),
        HirStatementKind::Declaration { initializer, .. } => {
            initializer.as_ref().is_some_and(expression_uses_global)
        }
        HirStatementKind::Assignment { value, target, .. } => {
            expression_uses_global(value) || place_uses_global(target)
        }
        HirStatementKind::Output(expression) | HirStatementKind::Return(Some(expression)) => {
            expression_uses_global(expression)
        }
        HirStatementKind::Return(None) => false,
        HirStatementKind::Call { arguments, .. } => arguments.iter().any(expression_uses_global),
        HirStatementKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expression_uses_global(condition)
                || statement_uses_global(then_branch)
                || else_branch.as_deref().is_some_and(statement_uses_global)
        }
        HirStatementKind::While { condition, body } => {
            expression_uses_global(condition) || statement_uses_global(body)
        }
    }
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

        let mut global_use = hir
            .functions
            .iter()
            .map(|function| statement_uses_global(&function.body))
            .collect::<Vec<_>>();
        if hir.globals.iter().any(|global| {
            global
                .initializer
                .as_ref()
                .is_some_and(expression_uses_global)
        }) {
            global_use[hir.entry.index()] = true;
        }
        loop {
            let mut changed = false;
            for caller in 0..outgoing.len() {
                if global_use[caller]
                    || !outgoing[caller]
                        .iter()
                        .any(|&site| global_use[calls[site].callee.index()])
                {
                    continue;
                }
                global_use[caller] = true;
                changed = true;
            }
            if !changed {
                break;
            }
        }

        CallGraph {
            calls,
            incoming,
            outgoing,
            reachable,
            global_use,
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

    /// Return reachable functions in callee-before-caller order.  Inlining a
    /// caller before its callees would copy calls that a later pass could have
    /// removed, multiplying both HIR and continuation work.
    fn bottom_up_order(&self) -> Vec<FunctionId> {
        fn visit(
            graph: &CallGraph,
            function: FunctionId,
            seen: &mut [bool],
            order: &mut Vec<FunctionId>,
        ) {
            if std::mem::replace(&mut seen[function.index()], true) {
                return;
            }
            for &site in &graph.outgoing[function.index()] {
                visit(graph, graph.calls[site].callee, seen, order);
            }
            order.push(function);
        }

        let mut seen = vec![false; self.outgoing.len()];
        let mut order = Vec::with_capacity(self.reachable.len());
        for index in 0..self.outgoing.len() {
            let function = FunctionId::new(index);
            if self.reachable.contains(function) {
                visit(self, function, &mut seen, &mut order);
            }
        }
        order
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

fn import_locals(
    caller_locals: &[HirLocal],
    callee: &HirFunction,
) -> (Vec<HirLocal>, Vec<LocalId>) {
    let mut caller_import = vec![];
    let mut map = vec![LocalId::new(0); callee.locals.len()];

    for (i, local) in callee.locals.iter().enumerate() {
        let new_id = LocalId::new(caller_locals.len() + i);

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

fn replace_call_stmts(
    context: &FrameOnlyContext<'_>,
    caller_locals: &[HirLocal],
    stmt: &mut HirStatement,
    inline_fn: &HirFunction,
    map: &[LocalId],
    remapped_body: &HirStatement,
) -> usize {
    // Semantic analysis only permits void calls in statement position.
    match &mut stmt.kind {
        HirStatementKind::Block(hir_statements) => {
            let mut replaced = 0;
            for s in hir_statements.iter_mut() {
                replaced +=
                    replace_call_stmts(context, caller_locals, s, inline_fn, map, remapped_body);
            }
            replaced
        }

        HirStatementKind::If {
            condition: _,
            then_branch,
            else_branch,
        } => {
            let mut replaced = replace_call_stmts(
                context,
                caller_locals,
                then_branch,
                inline_fn,
                map,
                remapped_body,
            );
            if let Some(else_branch) = else_branch {
                replaced += replace_call_stmts(
                    context,
                    caller_locals,
                    else_branch,
                    inline_fn,
                    map,
                    remapped_body,
                );
            }
            replaced
        }

        HirStatementKind::While { body, .. } => {
            replace_call_stmts(context, caller_locals, body, inline_fn, map, remapped_body)
        }

        HirStatementKind::Call {
            function,
            arguments,
        } => {
            let mut statements = Vec::new();
            if *function != inline_fn.id
                || !arguments
                    .iter()
                    .all(|argument| frame_only_expression(context, caller_locals, argument))
            {
                return 0;
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
            1
        }

        _ => 0,
    }
}

fn simple_void_body(function: &HirFunction) -> Option<HirStatement> {
    let mut body = function.body.clone();
    if !matches!(take_simple_last_return(&mut body), Some(None)) {
        return None;
    }
    Some(body)
}

fn variable_type(
    context: &FrameOnlyContext<'_>,
    locals: &[HirLocal],
    variable: VariableRef,
) -> Option<TypeId> {
    match variable {
        VariableRef::Global(global) => context
            .globals
            .get(global.index())
            .filter(|item| item.id == global)
            .map(|item| item.ty),
        VariableRef::Local(local) => locals
            .get(local.index())
            .filter(|item| item.id == local)
            .map(|item| item.ty),
    }
}

fn frame_only_projection(
    context: &FrameOnlyContext<'_>,
    locals: &[HirLocal],
    root_type: TypeId,
    result_type: TypeId,
    projection: &Projection,
) -> bool {
    match projection {
        Projection::Field { .. }
        | Projection::Index {
            index: ArrayIndex::Constant(_),
            ..
        } => true,
        Projection::Index {
            index: ArrayIndex::Dynamic(index),
            ..
        } => {
            (context.types.cells(root_type) == 0 || context.types.cells(result_type) == 0)
                && frame_only_expression(context, locals, index)
        }
    }
}

fn frame_only_place(context: &FrameOnlyContext<'_>, locals: &[HirLocal], place: &HirPlace) -> bool {
    let Some(root_type) = variable_type(context, locals, place.root) else {
        return false;
    };
    place
        .projections
        .iter()
        .all(|projection| frame_only_projection(context, locals, root_type, place.ty, projection))
}

fn frame_only_expression(
    context: &FrameOnlyContext<'_>,
    locals: &[HirLocal],
    expression: &HirExpression,
) -> bool {
    match &expression.kind {
        HirExpressionKind::Literal(_)
        | HirExpressionKind::EnumVariant(_)
        | HirExpressionKind::StringLiteral(_)
        | HirExpressionKind::Input => true,
        HirExpressionKind::Place(place) => frame_only_place(context, locals, place),
        HirExpressionKind::Project { base, projections } => {
            let root_type = base.ty;
            frame_only_expression(context, locals, base)
                && projections.iter().all(|projection| {
                    frame_only_projection(context, locals, root_type, expression.ty, projection)
                })
        }
        HirExpressionKind::Call { .. } => false,
        HirExpressionKind::Unary { operand, .. } => frame_only_expression(context, locals, operand),
        HirExpressionKind::Binary {
            operator,
            left,
            right,
        } => {
            if !frame_only_expression(context, locals, left) {
                return false;
            }
            let short = match (operator, hir::constant_cell_value(left)) {
                (BinaryOperator::LogicalAnd, Some(0)) => true,
                (BinaryOperator::LogicalOr, Some(value)) if value != 0 => true,
                _ => false,
            };
            short || frame_only_expression(context, locals, right)
        }
    }
}

fn frame_only_statement(
    context: &FrameOnlyContext<'_>,
    locals: &[HirLocal],
    statement: &HirStatement,
) -> bool {
    match &statement.kind {
        HirStatementKind::Empty => true,
        HirStatementKind::Block(statements) => statements
            .iter()
            .all(|statement| frame_only_statement(context, locals, statement)),
        HirStatementKind::Declaration { initializer, .. } => initializer
            .as_ref()
            .is_none_or(|value| frame_only_expression(context, locals, value)),
        HirStatementKind::Assignment { value, target, .. } => {
            frame_only_expression(context, locals, value)
                && frame_only_place(context, locals, target)
        }
        HirStatementKind::Output(value) => frame_only_expression(context, locals, value),
        HirStatementKind::Call { .. } | HirStatementKind::Abort | HirStatementKind::Return(_) => {
            false
        }
        HirStatementKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if let Some(value) = hir::constant_cell_value(condition) {
                return if value != 0 {
                    frame_only_statement(context, locals, then_branch)
                } else {
                    else_branch
                        .as_deref()
                        .is_none_or(|branch| frame_only_statement(context, locals, branch))
                };
            }
            frame_only_expression(context, locals, condition)
                && frame_only_statement(context, locals, then_branch)
                && else_branch
                    .as_deref()
                    .is_none_or(|branch| frame_only_statement(context, locals, branch))
        }
        HirStatementKind::While { condition, body } => {
            if hir::constant_cell_value(condition) == Some(0) {
                true
            } else {
                frame_only_expression(context, locals, condition)
                    && frame_only_statement(context, locals, body)
            }
        }
    }
}

fn simple_frame_void_body(
    context: &FrameOnlyContext<'_>,
    function: &HirFunction,
) -> Option<HirStatement> {
    let body = simple_void_body(function)?;
    frame_only_statement(context, &function.locals, &body).then_some(body)
}

fn pure_projection(projection: &Projection) -> bool {
    match projection {
        Projection::Field { .. }
        | Projection::Index {
            index: ArrayIndex::Constant(_),
            ..
        } => true,
        Projection::Index {
            index: ArrayIndex::Dynamic(index),
            ..
        } => pure_expression(index),
    }
}

fn pure_place(place: &HirPlace) -> bool {
    match place.root {
        VariableRef::Global(_) => place.projections.iter().all(pure_projection),
        VariableRef::Local(_) => place.projections.is_empty(),
    }
}

fn pure_expression(expression: &HirExpression) -> bool {
    match &expression.kind {
        HirExpressionKind::Literal(_)
        | HirExpressionKind::EnumVariant(_)
        | HirExpressionKind::StringLiteral(_) => true,
        HirExpressionKind::Place(place) => pure_place(place),
        HirExpressionKind::Project { base, projections } => {
            pure_expression(base) && projections.iter().all(pure_projection)
        }
        HirExpressionKind::Input | HirExpressionKind::Call { .. } => false,
        HirExpressionKind::Unary { operand, .. } => pure_expression(operand),
        HirExpressionKind::Binary { left, right, .. } => {
            pure_expression(left) && pure_expression(right)
        }
    }
}

fn substitute_projection_with_bindings(
    projection: &Projection,
    parameters: &[HirParameter],
    arguments: &[HirExpression],
    bindings: &[Option<HirExpression>],
) -> Option<Projection> {
    Some(match projection {
        Projection::Field { cell_offset } => Projection::Field {
            cell_offset: *cell_offset,
        },
        Projection::Index {
            index,
            length,
            element_cells,
        } => Projection::Index {
            index: match index {
                ArrayIndex::Constant(value) => ArrayIndex::Constant(*value),
                ArrayIndex::Dynamic(index) => ArrayIndex::Dynamic(Box::new(
                    substitute_expression_with_bindings(index, parameters, arguments, bindings)?,
                )),
            },
            length: *length,
            element_cells: *element_cells,
        },
    })
}

fn substitute_expression(
    expression: &HirExpression,
    parameters: &[HirParameter],
    arguments: &[HirExpression],
) -> Option<HirExpression> {
    substitute_expression_with_bindings(expression, parameters, arguments, &[])
}

fn substitute_expression_with_bindings(
    expression: &HirExpression,
    parameters: &[HirParameter],
    arguments: &[HirExpression],
    bindings: &[Option<HirExpression>],
) -> Option<HirExpression> {
    Some(match &expression.kind {
        HirExpressionKind::Literal(_)
        | HirExpressionKind::EnumVariant(_)
        | HirExpressionKind::StringLiteral(_) => expression.clone(),
        HirExpressionKind::Place(place) => {
            if place.projections.is_empty()
                && let VariableRef::Local(local) = place.root
                && let Some(parameter) = parameters
                    .iter()
                    .position(|parameter| parameter.local == local)
            {
                arguments
                    .get(parameter)
                    .cloned()
                    .unwrap_or_else(|| expression.clone())
            } else if place.projections.is_empty()
                && let VariableRef::Local(local) = place.root
                && let Some(Some(binding)) = bindings.get(local.index())
            {
                binding.clone()
            } else {
                let VariableRef::Global(_) = place.root else {
                    return None;
                };
                HirExpression {
                    kind: HirExpressionKind::Place(HirPlace {
                        root: place.root,
                        projections: place
                            .projections
                            .iter()
                            .map(|projection| {
                                substitute_projection_with_bindings(
                                    projection, parameters, arguments, bindings,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        ty: place.ty,
                    }),
                    ty: expression.ty,
                    offset: expression.offset,
                }
            }
        }
        HirExpressionKind::Project { base, projections } => HirExpression {
            kind: HirExpressionKind::Project {
                base: Box::new(substitute_expression_with_bindings(
                    base, parameters, arguments, bindings,
                )?),
                projections: projections
                    .iter()
                    .map(|projection| {
                        substitute_projection_with_bindings(
                            projection, parameters, arguments, bindings,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?,
            },
            ty: expression.ty,
            offset: expression.offset,
        },
        HirExpressionKind::Input => expression.clone(),
        HirExpressionKind::Call {
            function,
            arguments: call_arguments,
        } => HirExpression {
            kind: HirExpressionKind::Call {
                function: *function,
                arguments: call_arguments
                    .iter()
                    .map(|argument| {
                        substitute_expression_with_bindings(
                            argument, parameters, arguments, bindings,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?,
            },
            ty: expression.ty,
            offset: expression.offset,
        },
        HirExpressionKind::Unary { operator, operand } => HirExpression {
            kind: HirExpressionKind::Unary {
                operator: *operator,
                operand: Box::new(substitute_expression_with_bindings(
                    operand, parameters, arguments, bindings,
                )?),
            },
            ty: expression.ty,
            offset: expression.offset,
        },
        HirExpressionKind::Binary {
            operator,
            left,
            right,
        } => HirExpression {
            kind: HirExpressionKind::Binary {
                operator: *operator,
                left: Box::new(substitute_expression_with_bindings(
                    left, parameters, arguments, bindings,
                )?),
                right: Box::new(substitute_expression_with_bindings(
                    right, parameters, arguments, bindings,
                )?),
            },
            ty: expression.ty,
            offset: expression.offset,
        },
    })
}

fn simple_cell_return(
    context: &FrameOnlyContext<'_>,
    function: &HirFunction,
) -> Option<HirExpression> {
    if function.signature.return_type != TypeId::CELL
        || function
            .signature
            .parameter_types
            .iter()
            .any(|&ty| !context.types.is_scalar(ty))
    {
        return None;
    }
    let (statements, final_expression) = match &function.body.kind {
        HirStatementKind::Return(Some(expression)) => (&[][..], expression),
        HirStatementKind::Block(statements) => {
            let (last, prefix) = statements.split_last()?;
            let HirStatementKind::Return(Some(expression)) = &last.kind else {
                return None;
            };
            (prefix, expression)
        }
        _ => return None,
    };

    let mut bindings = vec![None; function.locals.len()];
    for statement in statements {
        let HirStatementKind::Declaration {
            local,
            initializer: Some(initializer),
        } = &statement.kind
        else {
            return None;
        };
        let local_info = function.locals.get(local.index())?;
        if !context.types.is_scalar(local_info.ty) || !pure_expression(initializer) {
            return None;
        }
        bindings[local.index()] = Some(substitute_expression_with_bindings(
            initializer,
            &function.parameters,
            &[],
            &bindings,
        )?);
    }
    let result = substitute_expression_with_bindings(
        final_expression,
        &function.parameters,
        &[],
        &bindings,
    )?;
    frame_only_expression(context, &function.locals, &result).then_some(result)
}

fn replace_cell_calls_expression(
    expression: &mut HirExpression,
    inline_fn: &HirFunction,
    return_expression: &HirExpression,
) -> usize {
    if let HirExpressionKind::Call {
        function,
        arguments,
    } = &expression.kind
        && *function == inline_fn.id
        && arguments.len() == inline_fn.parameters.len()
        && arguments.iter().all(pure_expression)
        && let Some(replacement) =
            substitute_expression(return_expression, &inline_fn.parameters, arguments)
    {
        *expression = replacement;
        return 1;
    }

    match &mut expression.kind {
        HirExpressionKind::Place(place) => {
            replace_cell_calls_place(place, inline_fn, return_expression)
        }
        HirExpressionKind::Project { base, projections } => {
            let mut replaced = replace_cell_calls_expression(base, inline_fn, return_expression);
            for projection in projections {
                replaced += replace_cell_calls_projection(projection, inline_fn, return_expression);
            }
            replaced
        }
        HirExpressionKind::Unary { operand, .. } => {
            replace_cell_calls_expression(operand, inline_fn, return_expression)
        }
        HirExpressionKind::Binary { left, right, .. } => {
            replace_cell_calls_expression(left, inline_fn, return_expression)
                + replace_cell_calls_expression(right, inline_fn, return_expression)
        }
        HirExpressionKind::Call { arguments, .. } => arguments
            .iter_mut()
            .map(|argument| replace_cell_calls_expression(argument, inline_fn, return_expression))
            .sum(),
        HirExpressionKind::Literal(_)
        | HirExpressionKind::EnumVariant(_)
        | HirExpressionKind::StringLiteral(_)
        | HirExpressionKind::Input => 0,
    }
}

fn replace_cell_calls_projection(
    projection: &mut Projection,
    inline_fn: &HirFunction,
    return_expression: &HirExpression,
) -> usize {
    match projection {
        Projection::Field { .. }
        | Projection::Index {
            index: ArrayIndex::Constant(_),
            ..
        } => 0,
        Projection::Index {
            index: ArrayIndex::Dynamic(index),
            ..
        } => replace_cell_calls_expression(index, inline_fn, return_expression),
    }
}

fn replace_cell_calls_place(
    place: &mut HirPlace,
    inline_fn: &HirFunction,
    return_expression: &HirExpression,
) -> usize {
    place
        .projections
        .iter_mut()
        .map(|projection| replace_cell_calls_projection(projection, inline_fn, return_expression))
        .sum()
}

fn replace_cell_calls_statement(
    statement: &mut HirStatement,
    inline_fn: &HirFunction,
    return_expression: &HirExpression,
) -> usize {
    match &mut statement.kind {
        HirStatementKind::Empty | HirStatementKind::Abort => 0,
        HirStatementKind::Block(statements) => statements
            .iter_mut()
            .map(|statement| replace_cell_calls_statement(statement, inline_fn, return_expression))
            .sum(),
        HirStatementKind::Declaration { initializer, .. } => initializer
            .as_mut()
            .map(|initializer| {
                replace_cell_calls_expression(initializer, inline_fn, return_expression)
            })
            .unwrap_or(0),
        HirStatementKind::Assignment { value, target, .. } => {
            replace_cell_calls_expression(value, inline_fn, return_expression)
                + replace_cell_calls_place(target, inline_fn, return_expression)
        }
        HirStatementKind::Output(expression) | HirStatementKind::Return(Some(expression)) => {
            replace_cell_calls_expression(expression, inline_fn, return_expression)
        }
        HirStatementKind::Return(None) => 0,
        HirStatementKind::Call { arguments, .. } => arguments
            .iter_mut()
            .map(|argument| replace_cell_calls_expression(argument, inline_fn, return_expression))
            .sum(),
        HirStatementKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            replace_cell_calls_expression(condition, inline_fn, return_expression)
                + replace_cell_calls_statement(then_branch, inline_fn, return_expression)
                + else_branch
                    .as_mut()
                    .map(|branch| {
                        replace_cell_calls_statement(branch, inline_fn, return_expression)
                    })
                    .unwrap_or(0)
        }
        HirStatementKind::While { condition, body } => {
            replace_cell_calls_expression(condition, inline_fn, return_expression)
                + replace_cell_calls_statement(body, inline_fn, return_expression)
        }
    }
}

fn replace_all_void_calls(
    context: &FrameOnlyContext<'_>,
    context_locals: &[HirLocal],
    statement: &mut HirStatement,
    inline_fn: &HirFunction,
    body_template: &HirStatement,
    caller_locals: &mut Vec<HirLocal>,
) -> usize {
    match &mut statement.kind {
        HirStatementKind::Block(statements) => statements
            .iter_mut()
            .map(|statement| {
                replace_all_void_calls(
                    context,
                    context_locals,
                    statement,
                    inline_fn,
                    body_template,
                    caller_locals,
                )
            })
            .sum(),
        HirStatementKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            replace_all_void_calls(
                context,
                context_locals,
                then_branch,
                inline_fn,
                body_template,
                caller_locals,
            ) + else_branch
                .as_mut()
                .map(|branch| {
                    replace_all_void_calls(
                        context,
                        context_locals,
                        branch,
                        inline_fn,
                        body_template,
                        caller_locals,
                    )
                })
                .unwrap_or(0)
        }
        HirStatementKind::While { body, .. } => replace_all_void_calls(
            context,
            context_locals,
            body,
            inline_fn,
            body_template,
            caller_locals,
        ),
        HirStatementKind::Call {
            function,
            arguments,
        } if *function == inline_fn.id
            && arguments
                .iter()
                .all(|argument| frame_only_expression(context, context_locals, argument)) =>
        {
            let (locals, mapping) = import_locals(caller_locals, inline_fn);
            let mut body = body_template.clone();
            remap_stmt(&mut body, &mapping);
            let mut statements = Vec::with_capacity(inline_fn.parameters.len() + 1);
            for (parameter, argument) in inline_fn.parameters.iter().zip(arguments) {
                statements.push(HirStatement {
                    kind: HirStatementKind::Declaration {
                        local: mapping[parameter.local.index()],
                        initializer: Some(argument.clone()),
                    },
                    offset: statement.offset,
                });
            }
            statements.push(body);
            *statement = HirStatement {
                kind: HirStatementKind::Block(statements),
                offset: statement.offset,
            };
            caller_locals.extend(locals);
            1
        }
        _ => 0,
    }
}

fn inline_cell_function(
    hir: &mut HirProgram,
    graph: &CallGraph,
    function: FunctionId,
    costs: &mut [Option<usize>],
) -> bool {
    if function == hir.entry
        || !graph.reachable.contains(function)
        || graph.check_recursive(function)
    {
        return false;
    }
    let inline_fn = hir.functions[function.index()].clone();
    let Some(return_expression) = ({
        let context = FrameOnlyContext {
            types: &hir.types,
            globals: &hir.globals,
        };
        simple_cell_return(&context, &inline_fn)
    }) else {
        return false;
    };
    let mut changed = false;

    for (caller_index, caller_cost) in costs.iter_mut().enumerate().take(hir.functions.len()) {
        let caller = FunctionId::new(caller_index);
        if caller == function || !graph.reachable.contains(caller) {
            continue;
        }
        let original = hir.functions[caller_index].clone();
        let replaced = replace_cell_calls_statement(
            &mut hir.functions[caller_index].body,
            &inline_fn,
            &return_expression,
        );
        if replaced == 0 {
            continue;
        }

        let before = match *caller_cost {
            Some(cost) => cost,
            None => {
                let Ok(cost) = crate::continuation_lowering::allocated_frame_chunks(hir, caller)
                else {
                    hir.functions[caller_index] = original;
                    continue;
                };
                *caller_cost = Some(cost);
                cost
            }
        };
        if let Ok(after) = crate::continuation_lowering::allocated_frame_chunks(hir, caller)
            && (after <= before || !graph.global_use[caller.index()])
        {
            *caller_cost = Some(after);
            changed = true;
        } else {
            hir.functions[caller_index] = original;
        }
    }
    changed
}

fn inline_multiple_use_void_function(
    hir: &mut HirProgram,
    graph: &CallGraph,
    function: FunctionId,
    costs: &mut [Option<usize>],
) -> bool {
    if function == hir.entry
        || !graph.reachable.contains(function)
        || graph.incoming[function.index()].len() <= 1
        || graph.check_recursive(function)
    {
        return false;
    }
    let inline_fn = hir.functions[function.index()].clone();
    let Some(body_template) = ({
        let context = FrameOnlyContext {
            types: &hir.types,
            globals: &hir.globals,
        };
        simple_frame_void_body(&context, &inline_fn)
    }) else {
        return false;
    };
    let callers = graph.incoming[function.index()]
        .iter()
        .map(|&site| graph.calls[site].caller)
        .collect::<Vec<_>>();
    let mut changed = false;

    for caller in callers {
        if caller == function || !graph.reachable.contains(caller) {
            continue;
        }
        let caller_index = caller.index();
        let original = hir.functions[caller_index].clone();
        let replaced = {
            let context = FrameOnlyContext {
                types: &hir.types,
                globals: &hir.globals,
            };
            let caller_function = &mut hir.functions[caller_index];
            let context_locals = caller_function.locals.clone();
            replace_all_void_calls(
                &context,
                &context_locals,
                &mut caller_function.body,
                &inline_fn,
                &body_template,
                &mut caller_function.locals,
            )
        };
        if replaced == 0 {
            continue;
        }

        let before = match costs[caller_index] {
            Some(cost) => cost,
            None => {
                let Ok(cost) = crate::continuation_lowering::allocated_frame_chunks(hir, caller)
                else {
                    hir.functions[caller_index] = original;
                    continue;
                };
                costs[caller_index] = Some(cost);
                cost
            }
        };
        if let Ok(after) = crate::continuation_lowering::allocated_frame_chunks(hir, caller)
            && (after <= before || !graph.global_use[caller.index()])
        {
            costs[caller_index] = Some(after);
            changed = true;
        } else {
            hir.functions[caller_index] = original;
        }
    }
    changed
}

// A larger caller frame stays allocated even outside the inlined call. In this
// ABI that makes every later global navigation traverse extra stack chunks.
// Global-free callers do not pay that navigation cost, so they may grow as long
// as the resulting frame still fits the ABI layout.
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
    let Some(mut body) = ({
        let context = FrameOnlyContext {
            types: &hir.types,
            globals: &hir.globals,
        };
        simple_frame_void_body(&context, &hir.functions[function.index()])
    }) else {
        return false;
    };
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
    let context_locals = caller_function.locals.clone();
    let (locals, mapping) = import_locals(&caller_function.locals, callee_function);
    remap_stmt(&mut body, &mapping);
    let replaced = {
        let context = FrameOnlyContext {
            types: &hir.types,
            globals: &hir.globals,
        };
        replace_call_stmts(
            &context,
            &context_locals,
            &mut caller_function.body,
            callee_function,
            &mapping,
            &body,
        )
    };
    if replaced == 0 {
        return false;
    }
    caller_function.locals.extend(locals);
    if let Ok(after) = crate::continuation_lowering::allocated_frame_chunks(hir, caller)
        && (after <= before || !graph.global_use[caller.index()])
    {
        costs[caller.index()] = Some(after);
        return true;
    }
    hir.functions[caller.index()] = original;
    false
}

pub(crate) fn inline_single_use_functions(hir: &mut HirProgram) {
    // Visit callees before callers so a frame-only caller can become inlineable
    // after its nested calls disappear. The graph is intentionally kept as a
    // snapshot: each callee is handled once, after all of its dependencies.
    let graph = CallGraph::build(hir);
    let order = graph.bottom_up_order();
    let mut cell_costs = vec![None; hir.functions.len()];
    let mut single_use_costs = vec![None; hir.functions.len()];
    let mut multiple_use_costs = vec![None; hir.functions.len()];
    for function in order {
        inline_cell_function(hir, &graph, function, &mut cell_costs);
        inline_function(hir, &graph, function, &mut single_use_costs);
        inline_multiple_use_void_function(hir, &graph, function, &mut multiple_use_costs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, macro_expansion, parser, semantic};

    fn analyze(source: &str) -> HirProgram {
        let ast = parser::parse(lexer::lex(source).unwrap()).unwrap();
        let ast = macro_expansion::expand(ast).unwrap();
        semantic::analyze(&ast).unwrap()
    }

    fn count_calls(statement: &HirStatement, function: FunctionId) -> usize {
        let mut count = 0;
        crate::hir_reachability::visit_statement_calls(statement, &mut |callee| {
            if callee == function {
                count += 1;
            }
        });
        count
    }

    #[test]
    fn inlines_multiple_call_sites_of_call_free_void_function() {
        let mut hir = analyze(
            "cell total; void add(cell value) { if (value != 0) { total += value; } } void main() { cell value = 3; while (value != 0) { add(value); add(value); value = value - 1; } output(total); }",
        );
        let add = hir
            .functions
            .iter()
            .find(|function| function.name == "add")
            .unwrap()
            .id;
        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, add), 2);

        inline_single_use_functions(&mut hir);

        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, add), 0);
    }

    #[test]
    fn inlines_multiple_use_void_functions_with_nested_side_effects() {
        let mut hir = analyze(
            "cell total; void bump(cell value) { total += value; } void wrapper(cell value) { bump(value); } void main() { wrapper(1); wrapper(2); output(total); }",
        );
        let wrapper = hir
            .functions
            .iter()
            .find(|function| function.name == "wrapper")
            .unwrap()
            .id;

        inline_single_use_functions(&mut hir);

        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, wrapper), 0);
        let (program, _) = crate::continuation_lowering::lower_hir_with_options(
            &hir,
            crate::ContinuationOptimizationOptions::default(),
        )
        .unwrap();
        let mut input = &[][..];
        let mut output = Vec::new();
        let stats = crate::run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            crate::ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(output, [3]);
        assert_eq!(stats.calls, 0);
    }

    #[test]
    fn inlines_deep_frame_only_calls_before_outer_control_flow() {
        let mut hir = analyze(
            "cell total; void bump(cell value) { total += value; } void wrapper(cell value) { if (value != 0) { bump(value); } } void main() { wrapper(1); wrapper(2); output(total); }",
        );
        let wrapper = hir
            .functions
            .iter()
            .find(|function| function.name == "wrapper")
            .unwrap()
            .id;

        inline_single_use_functions(&mut hir);

        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, wrapper), 0);
        let (program, _) = crate::continuation_lowering::lower_hir_with_options(
            &hir,
            crate::ContinuationOptimizationOptions::default(),
        )
        .unwrap();
        let mut input = &[][..];
        let mut output = Vec::new();
        let stats = crate::run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            crate::ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(output, [3]);
        assert_eq!(stats.calls, 0);
    }

    #[test]
    fn inlines_pure_cell_expression_at_multiple_sites() {
        let mut hir = analyze(
            "cell add1(cell value) { cell result = value + 1; return result; } void main() { cell value = 3; output(add1(value)); output(add1(value)); }",
        );
        let add1 = hir
            .functions
            .iter()
            .find(|function| function.name == "add1")
            .unwrap()
            .id;
        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, add1), 2);
        let add1_function = hir
            .functions
            .iter()
            .find(|function| function.id == add1)
            .unwrap();
        let context = FrameOnlyContext {
            types: &hir.types,
            globals: &hir.globals,
        };
        assert!(simple_cell_return(&context, add1_function).is_some());

        inline_single_use_functions(&mut hir);

        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, add1), 0);
    }

    #[test]
    fn does_not_duplicate_side_effecting_cell_arguments() {
        let mut hir = analyze(
            "cell add1(cell value) { return value + 1; } void main() { output(add1(input())); }",
        );
        let add1 = hir
            .functions
            .iter()
            .find(|function| function.name == "add1")
            .unwrap()
            .id;

        inline_single_use_functions(&mut hir);

        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, add1), 1);
    }

    #[test]
    fn preserves_side_effects_in_an_inlined_cell_return_expression() {
        let mut hir = analyze(
            "cell add_input(cell value) { return value + input(); } void main() { output(add_input(3)); }",
        );
        let add_input = hir
            .functions
            .iter()
            .find(|function| function.name == "add_input")
            .unwrap()
            .id;

        inline_single_use_functions(&mut hir);

        let main = hir
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert_eq!(count_calls(&main.body, add_input), 0);
        let (program, _) = crate::continuation_lowering::lower_hir_with_options(
            &hir,
            crate::ContinuationOptimizationOptions::default(),
        )
        .unwrap();
        let mut input = &[4][..];
        let mut output = Vec::new();
        let stats = crate::run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            crate::ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(output, [7]);
        assert_eq!(stats.input_operations, 1);
    }

    #[test]
    fn executes_inlined_pure_cell_expression_inside_loop() {
        let source = "cell twice(cell value) { cell result = value + value; return result; } void main() { cell value = input(); while (value != 0) { output(twice(value)); value = value - 1; } }";
        let mut hir = analyze(source);
        inline_single_use_functions(&mut hir);
        let (program, _) = crate::continuation_lowering::lower_hir_with_options(
            &hir,
            crate::ContinuationOptimizationOptions::default(),
        )
        .unwrap();
        let mut input = &[3][..];
        let mut output = Vec::new();
        let stats = crate::run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            crate::ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(output, [6, 4, 2]);
        assert_eq!(stats.calls, 0);
        assert_eq!(stats.returns, 0);
    }
}
