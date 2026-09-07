//! Function reachability used to omit unused functions during lowering.

use crate::hir::{
    ArrayIndex, FunctionId, HirExpression, HirExpressionKind, HirProgram, HirStatement,
    HirStatementKind, Projection,
};

pub(crate) struct ReachableFunctions {
    reachable: Vec<bool>,
}

impl ReachableFunctions {
    /// Global initializers and main are roots. Both sides of branches are kept.
    pub(crate) fn analyze(program: &HirProgram) -> Self {
        let mut reachable = vec![false; program.functions.len()];
        let mut pending = vec![program.entry];
        for global in &program.globals {
            if let Some(initializer) = &global.initializer {
                visit_expression_calls(initializer, &mut |function| pending.push(function));
            }
        }
        while let Some(function) = pending.pop() {
            if std::mem::replace(&mut reachable[function.index()], true) {
                continue;
            }
            visit_statement_calls(&program.functions[function.index()].body, &mut |callee| {
                pending.push(callee)
            });
        }
        Self { reachable }
    }
    pub(crate) fn contains(&self, function: FunctionId) -> bool {
        self.reachable[function.index()]
    }
    pub(crate) fn len(&self) -> usize {
        self.reachable.iter().filter(|&&live| live).count()
    }
}

fn visit_projections(projections: &[Projection], visit: &mut impl FnMut(FunctionId)) {
    for projection in projections {
        if let Projection::Index {
            index: ArrayIndex::Dynamic(index),
            ..
        } = projection
        {
            visit_expression_calls(index, visit);
        }
    }
}
fn visit_call(
    function: FunctionId,
    arguments: &[HirExpression],
    visit: &mut impl FnMut(FunctionId),
) {
    visit(function);
    for argument in arguments {
        visit_expression_calls(argument, visit);
    }
}
pub(crate) fn visit_expression_calls(
    expression: &HirExpression,
    visit: &mut impl FnMut(FunctionId),
) {
    match &expression.kind {
        HirExpressionKind::Literal(_)
        | HirExpressionKind::EnumVariant(_)
        | HirExpressionKind::StringLiteral(_)
        | HirExpressionKind::Input => {}
        HirExpressionKind::Place(place) => visit_projections(&place.projections, visit),
        HirExpressionKind::Project { base, projections } => {
            visit_expression_calls(base, visit);
            visit_projections(projections, visit);
        }
        HirExpressionKind::Unary { operand, .. } => visit_expression_calls(operand, visit),
        HirExpressionKind::Binary { left, right, .. } => {
            visit_expression_calls(left, visit);
            visit_expression_calls(right, visit);
        }
        HirExpressionKind::Call {
            function,
            arguments,
        } => visit_call(*function, arguments, visit),
    }
}
pub(crate) fn visit_statement_calls(statement: &HirStatement, visit: &mut impl FnMut(FunctionId)) {
    match &statement.kind {
        HirStatementKind::Empty | HirStatementKind::Abort => {}
        HirStatementKind::Block(statements) => {
            for statement in statements {
                visit_statement_calls(statement, visit);
            }
        }
        HirStatementKind::Declaration { initializer, .. }
        | HirStatementKind::Return(initializer) => {
            if let Some(expression) = initializer {
                visit_expression_calls(expression, visit);
            }
        }
        HirStatementKind::Assignment { value, target, .. } => {
            visit_expression_calls(value, visit);
            visit_projections(&target.projections, visit);
        }
        HirStatementKind::Output(expression) => visit_expression_calls(expression, visit),
        HirStatementKind::Call {
            function,
            arguments,
        } => visit_call(*function, arguments, visit),
        HirStatementKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visit_expression_calls(condition, visit);
            visit_statement_calls(then_branch, visit);
            if let Some(branch) = else_branch {
                visit_statement_calls(branch, visit);
            }
        }
        HirStatementKind::While { condition, body } => {
            visit_expression_calls(condition, visit);
            visit_statement_calls(body, visit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser, semantic};

    #[test]
    fn follows_calls_in_globals_expressions_and_recursive_functions() {
        let source = r"
            cell global = initialize();
            cell initialize() { return seed(); }
            cell seed() { return 1; }
            void unused() { unused_cycle(); }
            void unused_cycle() { unused(); }
            void main() {
                cell[3] data;
                cell value = data[read_index()];
                data[write_index()] = -operand() + identity(argument());
                if (condition()) { recursive(); } else { cycle_a(); }
                while (loop_condition()) { loop_body(); }
                output(output_value());
            }
            cell read_index() { return 0; }
            cell write_index() { return 1; }
            cell operand() { return 0; }
            cell identity(cell value) { return value; }
            cell argument() { return 1; }
            cell condition() { return 0; }
            void recursive() { recursive(); recursive(); }
            void cycle_a() { cycle_b(); }
            void cycle_b() { cycle_a(); }
            cell loop_condition() { return 0; }
            void loop_body() {}
            cell output_value() { return 1; }
        ";
        let ast = parser::parse(lexer::lex(source).unwrap()).unwrap();
        let program = semantic::analyze(&ast).unwrap();
        let reachable = ReachableFunctions::analyze(&program);
        for function in &program.functions {
            assert_eq!(
                reachable.contains(function.id),
                !function.name.starts_with("unused"),
                "{}",
                function.name,
            );
        }
        assert_eq!(reachable.len(), program.functions.len() - 2);
    }
}
