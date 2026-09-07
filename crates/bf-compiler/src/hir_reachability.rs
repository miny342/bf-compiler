//! Function reachability used to omit unused functions during lowering.

use crate::hir::{
    ArrayIndex, FunctionId, HirExpression, HirExpressionKind, HirProgram, HirStatement,
    HirStatementKind, Projection,
};

pub(crate) struct ReachableFunctions {
    reachable: Vec<bool>,
    pending: Vec<FunctionId>,
}

impl ReachableFunctions {
    /// Analyze semantically validated HIR. Global initializers execute before
    /// main, so their callees are roots too. Both sides of branches are kept:
    /// this pass only eliminates functions, not unreachable statements.
    pub(crate) fn analyze(program: &HirProgram) -> Self {
        let mut analysis = Self {
            reachable: vec![false; program.functions.len()],
            pending: Vec::new(),
        };
        analysis.visit_function(program.entry);
        for global in &program.globals {
            if let Some(initializer) = &global.initializer {
                analysis.visit_expression(initializer);
            }
        }
        // A worklist handles recursion and long call chains without recursively
        // walking function bodies on the Rust stack.
        while let Some(function) = analysis.pending.pop() {
            analysis.visit_statement(&program.functions[function.index()].body);
        }
        analysis
    }

    pub(crate) fn contains(&self, function: FunctionId) -> bool {
        self.reachable[function.index()]
    }

    pub(crate) fn len(&self) -> usize {
        self.reachable.iter().filter(|&&live| live).count()
    }

    fn visit_function(&mut self, function: FunctionId) {
        if !self.reachable[function.index()] {
            self.reachable[function.index()] = true;
            self.pending.push(function);
        }
    }

    fn visit_projections(&mut self, projections: &[Projection]) {
        for projection in projections {
            if let Projection::Index {
                index: ArrayIndex::Dynamic(index),
                ..
            } = projection
            {
                self.visit_expression(index);
            }
        }
    }

    fn visit_call(&mut self, function: FunctionId, arguments: &[HirExpression]) {
        self.visit_function(function);
        for argument in arguments {
            self.visit_expression(argument);
        }
    }

    fn visit_expression(&mut self, expression: &HirExpression) {
        match &expression.kind {
            HirExpressionKind::Literal(_)
            | HirExpressionKind::EnumVariant(_)
            | HirExpressionKind::StringLiteral(_)
            | HirExpressionKind::Input => {}
            HirExpressionKind::Place(place) => self.visit_projections(&place.projections),
            HirExpressionKind::Project { base, projections } => {
                self.visit_expression(base);
                self.visit_projections(projections);
            }
            HirExpressionKind::Unary { operand, .. } => self.visit_expression(operand),
            HirExpressionKind::Binary { left, right, .. } => {
                self.visit_expression(left);
                self.visit_expression(right);
            }
            HirExpressionKind::Call {
                function,
                arguments,
            } => self.visit_call(*function, arguments),
        }
    }

    fn visit_statement(&mut self, statement: &HirStatement) {
        match &statement.kind {
            HirStatementKind::Empty | HirStatementKind::Abort => {}
            HirStatementKind::Block(statements) => {
                for statement in statements {
                    self.visit_statement(statement);
                }
            }
            HirStatementKind::Declaration { initializer, .. }
            | HirStatementKind::Return(initializer) => {
                if let Some(expression) = initializer {
                    self.visit_expression(expression);
                }
            }
            HirStatementKind::Assignment { value, target, .. } => {
                self.visit_expression(value);
                self.visit_projections(&target.projections);
            }
            HirStatementKind::Output(expression) => self.visit_expression(expression),
            HirStatementKind::Call {
                function,
                arguments,
            } => self.visit_call(*function, arguments),
            HirStatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.visit_expression(condition);
                self.visit_statement(then_branch);
                if let Some(branch) = else_branch {
                    self.visit_statement(branch);
                }
            }
            HirStatementKind::While { condition, body } => {
                self.visit_expression(condition);
                self.visit_statement(body);
            }
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
