//! Source-level name resolution and scalar type checking.

use std::collections::HashMap;

use crate::ast;
use crate::frontend::FrontendError;
use crate::hir::{
    self, FunctionId, FunctionSignature, HirExpression, HirExpressionKind, HirFunction,
    HirParameter, HirProgram, HirStatement, HirStatementKind, LocalId,
};

/// Resolve and type-check a parsed source program.
pub(crate) fn analyze(program: &ast::AstProgram) -> Result<HirProgram, FrontendError> {
    let signatures = collect_signatures(program)?;
    let entry = validate_main(program, &signatures)?;
    let functions = program
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            FunctionAnalyzer::new(&signatures, hir_type(function.return_type))
                .analyze(FunctionId::new(index), function)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(HirProgram { entry, functions })
}

#[derive(Debug, Clone)]
struct RegisteredFunction {
    id: FunctionId,
    return_type: hir::Type,
    parameter_count: usize,
}

fn collect_signatures(
    program: &ast::AstProgram,
) -> Result<HashMap<String, RegisteredFunction>, FrontendError> {
    let mut signatures = HashMap::new();
    for (index, function) in program.functions.iter().enumerate() {
        let registered = RegisteredFunction {
            id: FunctionId::new(index),
            return_type: hir_type(function.return_type),
            parameter_count: function.parameters.len(),
        };
        if signatures
            .insert(function.name.text.clone(), registered)
            .is_some()
        {
            return Err(FrontendError::at(
                function.name.offset,
                format!("function {:?} is already defined", function.name.text),
            ));
        }
    }
    Ok(signatures)
}

fn validate_main(
    program: &ast::AstProgram,
    signatures: &HashMap<String, RegisteredFunction>,
) -> Result<FunctionId, FrontendError> {
    let Some(main) = signatures.get("main") else {
        return Err(FrontendError::at(
            program.eof_offset,
            "program must define exactly one 'void main()' function",
        ));
    };
    let definition = &program.functions[main.id.index()];
    if main.return_type != hir::Type::Void {
        return Err(FrontendError::at(
            definition.name.offset,
            "main must have return type 'void'",
        ));
    }
    if main.parameter_count != 0 {
        return Err(FrontendError::at(
            definition.name.offset,
            "main must not have parameters",
        ));
    }
    Ok(main.id)
}

struct FunctionAnalyzer<'a> {
    signatures: &'a HashMap<String, RegisteredFunction>,
    return_type: hir::Type,
    scopes: Vec<HashMap<String, LocalBinding>>,
    next_local: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalBinding {
    Cell(LocalId),
    Array { base: LocalId, length: usize },
}

impl<'a> FunctionAnalyzer<'a> {
    fn new(signatures: &'a HashMap<String, RegisteredFunction>, return_type: hir::Type) -> Self {
        Self {
            signatures,
            return_type,
            scopes: vec![HashMap::new()],
            next_local: 0,
        }
    }

    fn analyze(
        mut self,
        id: FunctionId,
        function: &ast::Function,
    ) -> Result<HirFunction, FrontendError> {
        let mut parameters = Vec::with_capacity(function.parameters.len());
        for parameter in &function.parameters {
            let local = self.declare_cell(&parameter.name)?;
            parameters.push(HirParameter {
                local,
                offset: parameter.name.offset,
            });
        }

        let (mut body, flow, closing_offset) = self.analyze_function_body(&function.body)?;
        match (self.return_type, flow) {
            (hir::Type::Cell, Flow::FallsThrough) => {
                return Err(FrontendError::at(
                    closing_offset,
                    format!(
                        "cell function {:?} does not return a value on every path",
                        function.name.text
                    ),
                ));
            }
            (hir::Type::Void, Flow::FallsThrough) => {
                let HirStatementKind::Block(statements) = &mut body.kind else {
                    unreachable!("function bodies are always blocks")
                };
                statements.push(HirStatement {
                    kind: HirStatementKind::Return(None),
                    offset: closing_offset,
                });
            }
            _ => {}
        }

        Ok(HirFunction {
            id,
            name: function.name.text.clone(),
            offset: function.name.offset,
            signature: FunctionSignature {
                return_type: self.return_type,
                parameter_types: vec![hir::Type::Cell; parameters.len()],
            },
            parameters,
            local_count: self.next_local,
            body,
        })
    }

    fn analyze_function_body(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<(HirStatement, Flow, usize), FrontendError> {
        let ast::StatementKind::Block {
            statements,
            closing_offset,
        } = &statement.kind
        else {
            unreachable!("the parser only produces block function bodies")
        };

        // Parameters and declarations in the outer function body intentionally
        // share a scope. Nested blocks go through `analyze_statement` and push
        // their own scope.
        let (statements, flow) = self.analyze_statements(statements)?;
        Ok((
            HirStatement {
                kind: HirStatementKind::Block(statements),
                offset: statement.offset,
            },
            flow,
            *closing_offset,
        ))
    }

    fn analyze_statements(
        &mut self,
        statements: &[ast::Statement],
    ) -> Result<(Vec<HirStatement>, Flow), FrontendError> {
        let mut output = Vec::with_capacity(statements.len());
        let mut flow = Flow::FallsThrough;
        for statement in statements {
            let (statement, statement_flow) = self.analyze_statement(statement)?;
            if flow == Flow::FallsThrough {
                flow = statement_flow;
            }
            output.push(statement);
        }
        Ok((output, flow))
    }

    fn analyze_statement(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<(HirStatement, Flow), FrontendError> {
        let (kind, flow) = match &statement.kind {
            ast::StatementKind::Empty => (HirStatementKind::Empty, Flow::FallsThrough),
            ast::StatementKind::Block { statements, .. } => {
                self.scopes.push(HashMap::new());
                let result = self.analyze_statements(statements);
                self.scopes.pop();
                let (statements, flow) = result?;
                (HirStatementKind::Block(statements), flow)
            }
            ast::StatementKind::Declaration {
                name,
                array_length,
                initializer,
            } => {
                if self.scopes.last().unwrap().contains_key(&name.text) {
                    return Err(already_declared(name));
                }
                // The declared name is not visible in its own initializer.
                let initializer = initializer
                    .as_ref()
                    .map(|expression| self.analyze_expression(expression))
                    .transpose()?;
                if let Some(length) = array_length {
                    debug_assert!(initializer.is_none());
                    let base = self.declare_array(name, *length)?;
                    let declarations = (0..*length)
                        .map(|index| HirStatement {
                            kind: HirStatementKind::Declaration {
                                local: LocalId::new(base.index() + index),
                                initializer: None,
                            },
                            offset: statement.offset,
                        })
                        .collect();
                    (HirStatementKind::Block(declarations), Flow::FallsThrough)
                } else {
                    let local = self.declare_cell(name)?;
                    (
                        HirStatementKind::Declaration { local, initializer },
                        Flow::FallsThrough,
                    )
                }
            }
            ast::StatementKind::Assignment {
                target,
                operator,
                value,
            } => {
                let value = self.analyze_expression(value)?;
                let local = self.resolve_place(target)?;
                (
                    HirStatementKind::Assignment {
                        local,
                        operator: assignment_operator(*operator),
                        value,
                    },
                    Flow::FallsThrough,
                )
            }
            ast::StatementKind::Output(value) => (
                HirStatementKind::Output(self.analyze_expression(value)?),
                Flow::FallsThrough,
            ),
            ast::StatementKind::Call { name, arguments } => {
                let (function, return_type, arguments) = self.analyze_call(name, arguments)?;
                if return_type != hir::Type::Void {
                    return Err(FrontendError::at(
                        name.offset,
                        format!(
                            "cell-returning function {:?} cannot be used as a statement",
                            name.text
                        ),
                    ));
                }
                (
                    HirStatementKind::Call {
                        function,
                        arguments,
                    },
                    Flow::FallsThrough,
                )
            }
            ast::StatementKind::Return(value) => {
                let value = match (self.return_type, value) {
                    (hir::Type::Cell, Some(value)) => Some(self.analyze_expression(value)?),
                    (hir::Type::Cell, None) => {
                        return Err(FrontendError::at(
                            statement.offset,
                            "cell function must return a value",
                        ));
                    }
                    (hir::Type::Void, Some(value)) => {
                        return Err(FrontendError::at(
                            value.offset,
                            "void function must not return a value",
                        ));
                    }
                    (hir::Type::Void, None) => None,
                };
                (HirStatementKind::Return(value), Flow::Returns)
            }
            ast::StatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.analyze_expression(condition)?;
                let constant_condition =
                    hir::constant_cell_value(&condition).map(|value| value != 0);
                let (then_branch, then_flow) = self.analyze_statement(then_branch)?;
                let (else_branch, else_flow) = if let Some(else_branch) = else_branch {
                    let (branch, flow) = self.analyze_statement(else_branch)?;
                    (Some(Box::new(branch)), flow)
                } else {
                    (None, Flow::FallsThrough)
                };
                let flow = match constant_condition {
                    Some(true) => then_flow,
                    Some(false) => else_flow,
                    None if then_flow == Flow::Returns && else_flow == Flow::Returns => {
                        Flow::Returns
                    }
                    None => Flow::FallsThrough,
                };
                (
                    HirStatementKind::If {
                        condition,
                        then_branch: Box::new(then_branch),
                        else_branch,
                    },
                    flow,
                )
            }
            ast::StatementKind::While { condition, body } => {
                let condition = self.analyze_expression(condition)?;
                let constant_condition =
                    hir::constant_cell_value(&condition).map(|value| value != 0);
                let (body, body_flow) = self.analyze_statement(body)?;
                let flow = if constant_condition == Some(true) && body_flow == Flow::Returns {
                    Flow::Returns
                } else {
                    Flow::FallsThrough
                };
                (
                    HirStatementKind::While {
                        condition,
                        body: Box::new(body),
                    },
                    flow,
                )
            }
        };

        Ok((
            HirStatement {
                kind,
                offset: statement.offset,
            },
            flow,
        ))
    }

    fn analyze_expression(
        &mut self,
        expression: &ast::Expression,
    ) -> Result<HirExpression, FrontendError> {
        let kind = match &expression.kind {
            ast::ExpressionKind::Literal(value) => HirExpressionKind::Literal(*value),
            ast::ExpressionKind::Variable(name) => {
                HirExpressionKind::Local(self.resolve_cell(name)?)
            }
            ast::ExpressionKind::ArrayElement { array, index } => {
                HirExpressionKind::Local(self.resolve_indexed(array, index)?)
            }
            ast::ExpressionKind::Input => HirExpressionKind::Input,
            ast::ExpressionKind::Call { name, arguments } => {
                let (function, return_type, arguments) = self.analyze_call(name, arguments)?;
                if return_type != hir::Type::Cell {
                    return Err(FrontendError::at(
                        name.offset,
                        format!("void function {:?} cannot be used as a value", name.text),
                    ));
                }
                HirExpressionKind::Call {
                    function,
                    arguments,
                }
            }
            ast::ExpressionKind::Unary { operator, operand } => HirExpressionKind::Unary {
                operator: unary_operator(*operator),
                operand: Box::new(self.analyze_expression(operand)?),
            },
            ast::ExpressionKind::Binary {
                operator,
                left,
                right,
            } => HirExpressionKind::Binary {
                operator: binary_operator(*operator),
                left: Box::new(self.analyze_expression(left)?),
                right: Box::new(self.analyze_expression(right)?),
            },
        };
        Ok(HirExpression {
            kind,
            ty: hir::Type::Cell,
            offset: expression.offset,
        })
    }

    fn analyze_call(
        &mut self,
        name: &ast::Name,
        arguments: &[ast::Expression],
    ) -> Result<(FunctionId, hir::Type, Vec<HirExpression>), FrontendError> {
        let Some(signature) = self.signatures.get(&name.text) else {
            return Err(FrontendError::at(
                name.offset,
                format!("undefined function {:?}", name.text),
            ));
        };
        if name.text == "main" {
            return Err(FrontendError::at(name.offset, "main cannot be called"));
        }
        if arguments.len() != signature.parameter_count {
            return Err(FrontendError::at(
                name.offset,
                format!(
                    "function {:?} expects {} argument(s), but {} were provided",
                    name.text,
                    signature.parameter_count,
                    arguments.len()
                ),
            ));
        }
        let arguments = arguments
            .iter()
            .map(|argument| self.analyze_expression(argument))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((signature.id, signature.return_type, arguments))
    }

    fn declare_cell(&mut self, name: &ast::Name) -> Result<LocalId, FrontendError> {
        if self.scopes.last().unwrap().contains_key(&name.text) {
            return Err(already_declared(name));
        }
        let local = LocalId::new(self.next_local);
        self.next_local += 1;
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.text.clone(), LocalBinding::Cell(local));
        Ok(local)
    }

    fn declare_array(&mut self, name: &ast::Name, length: usize) -> Result<LocalId, FrontendError> {
        if self.scopes.last().unwrap().contains_key(&name.text) {
            return Err(already_declared(name));
        }
        let base = LocalId::new(self.next_local);
        self.next_local += length;
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.text.clone(), LocalBinding::Array { base, length });
        Ok(base)
    }

    fn resolve_binding(&self, name: &ast::Name) -> Result<LocalBinding, FrontendError> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name.text).copied())
            .ok_or_else(|| {
                FrontendError::at(name.offset, format!("undefined variable {:?}", name.text))
            })
    }

    fn resolve_cell(&self, name: &ast::Name) -> Result<LocalId, FrontendError> {
        match self.resolve_binding(name)? {
            LocalBinding::Cell(local) => Ok(local),
            LocalBinding::Array { .. } => Err(FrontendError::at(
                name.offset,
                format!(
                    "array {:?} cannot be used as a cell value; whole-array operations are not implemented",
                    name.text
                ),
            )),
        }
    }

    fn resolve_place(&mut self, place: &ast::Place) -> Result<LocalId, FrontendError> {
        match &place.index {
            Some(index) => self.resolve_indexed(&place.name, index),
            None => match self.resolve_binding(&place.name)? {
                LocalBinding::Cell(local) => Ok(local),
                LocalBinding::Array { .. } => Err(FrontendError::at(
                    place.name.offset,
                    format!(
                        "whole-array assignment to {:?} is not implemented",
                        place.name.text
                    ),
                )),
            },
        }
    }

    fn resolve_indexed(
        &mut self,
        name: &ast::Name,
        index: &ast::Expression,
    ) -> Result<LocalId, FrontendError> {
        let binding = self.resolve_binding(name)?;
        let LocalBinding::Array { base, length } = binding else {
            return Err(FrontendError::at(
                name.offset,
                format!("cell variable {:?} cannot be indexed", name.text),
            ));
        };
        let index_expression = self.analyze_expression(index)?;
        let Some(index_value) = hir::constant_cell_value(&index_expression) else {
            return Err(FrontendError::at(
                index.offset,
                "dynamic array indices are not implemented in this compiler stage",
            ));
        };
        let index_value = usize::from(index_value);
        if index_value >= length {
            return Err(FrontendError::at(
                index.offset,
                format!(
                    "array index {index_value} is out of bounds for {:?} with length {length}",
                    name.text
                ),
            ));
        }
        Ok(LocalId::new(base.index() + index_value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    FallsThrough,
    Returns,
}

fn already_declared(name: &ast::Name) -> FrontendError {
    FrontendError::at(
        name.offset,
        format!("variable {:?} is already declared in this scope", name.text),
    )
}

const fn hir_type(ty: ast::Type) -> hir::Type {
    match ty {
        ast::Type::Cell => hir::Type::Cell,
        ast::Type::Void => hir::Type::Void,
    }
}

const fn assignment_operator(operator: ast::AssignmentOperator) -> hir::AssignmentOperator {
    match operator {
        ast::AssignmentOperator::Set => hir::AssignmentOperator::Set,
        ast::AssignmentOperator::Add => hir::AssignmentOperator::Add,
        ast::AssignmentOperator::Subtract => hir::AssignmentOperator::Subtract,
    }
}

const fn unary_operator(operator: ast::UnaryOperator) -> hir::UnaryOperator {
    match operator {
        ast::UnaryOperator::Plus => hir::UnaryOperator::Plus,
        ast::UnaryOperator::Negate => hir::UnaryOperator::Negate,
        ast::UnaryOperator::Not => hir::UnaryOperator::Not,
    }
}

const fn binary_operator(operator: ast::BinaryOperator) -> hir::BinaryOperator {
    match operator {
        ast::BinaryOperator::Add => hir::BinaryOperator::Add,
        ast::BinaryOperator::Subtract => hir::BinaryOperator::Subtract,
        ast::BinaryOperator::Less => hir::BinaryOperator::Less,
        ast::BinaryOperator::LessEqual => hir::BinaryOperator::LessEqual,
        ast::BinaryOperator::Greater => hir::BinaryOperator::Greater,
        ast::BinaryOperator::GreaterEqual => hir::BinaryOperator::GreaterEqual,
        ast::BinaryOperator::Equal => hir::BinaryOperator::Equal,
        ast::BinaryOperator::NotEqual => hir::BinaryOperator::NotEqual,
        ast::BinaryOperator::LogicalAnd => hir::BinaryOperator::LogicalAnd,
        ast::BinaryOperator::LogicalOr => hir::BinaryOperator::LogicalOr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser};

    fn analyze_source(source: &str) -> Result<HirProgram, FrontendError> {
        let tokens = lexer::lex(source)?;
        let ast = parser::parse(tokens)?;
        analyze(&ast)
    }

    #[test]
    fn resolves_forward_and_mutual_calls_in_source_order() {
        let program = analyze_source(
            "cell first(cell n) { if (n) return second(n - 1); else return 0; }\n\
             cell second(cell n) { if (n) return first(n - 1); else return 1; }\n\
             void main() { output(first(2)); }",
        )
        .unwrap();

        assert_eq!(program.functions[0].id, FunctionId::new(0));
        assert_eq!(program.functions[1].id, FunctionId::new(1));
        assert_eq!(program.entry, FunctionId::new(2));
    }

    #[test]
    fn parameters_share_the_function_body_scope() {
        let error =
            analyze_source("cell same(cell value) { cell value; return value; } void main() {}")
                .unwrap_err();
        assert!(error.message().contains("already declared"));
    }

    #[test]
    fn permits_shadowing_in_nested_blocks() {
        let program = analyze_source(
            "cell same(cell value) { { cell value = 2; } return value; } void main() {}",
        )
        .unwrap();
        assert_eq!(program.functions[0].local_count, 2);
    }

    #[test]
    fn enforces_call_context_and_arity() {
        let void_value =
            analyze_source("void helper() {} void main() { cell x = helper(); }").unwrap_err();
        assert!(void_value.message().contains("cannot be used as a value"));

        let cell_statement =
            analyze_source("cell helper() { return 1; } void main() { helper(); }").unwrap_err();
        assert!(
            cell_statement
                .message()
                .contains("cannot be used as a statement")
        );

        let arity =
            analyze_source("cell helper(cell x) { return x; } void main() { output(helper()); }")
                .unwrap_err();
        assert!(arity.message().contains("expects 1 argument"));
    }

    #[test]
    fn requires_cell_returns_and_inserts_void_returns() {
        let missing =
            analyze_source("cell choose(cell x) { if (x) return 1; } void main() {}").unwrap_err();
        assert!(missing.message().contains("every path"));

        let program = analyze_source("void helper() {} void main() {} ").unwrap();
        for function in program.functions {
            let HirStatementKind::Block(statements) = function.body.kind else {
                panic!("expected block")
            };
            assert!(matches!(
                statements.last().map(|statement| &statement.kind),
                Some(HirStatementKind::Return(None))
            ));
        }
    }

    #[test]
    fn constant_conditions_refine_definite_return_flow() {
        let program = analyze_source(
            "cell from_if() { if (!(1 - 1)) return 7; }\n\
             cell from_while() { while (2 > 1 && 3) return 8; }\n\
             void main() { output(from_if()); output(from_while()); }",
        )
        .unwrap();
        crate::continuation_lowering::lower_hir(&program).unwrap();

        for source in [
            "cell bad() { if (0) return 1; } void main() {}",
            "cell bad() { while (1 == 0) return 1; } void main() {}",
        ] {
            let error = analyze_source(source).unwrap_err();
            assert!(error.message().contains("every path"));
        }
    }

    #[test]
    fn validates_main_and_forbids_calls_to_it() {
        assert!(
            analyze_source("void helper() {}")
                .unwrap_err()
                .message()
                .contains("void main()")
        );
        assert!(
            analyze_source("cell main() { return 0; }")
                .unwrap_err()
                .message()
                .contains("return type 'void'")
        );
        assert!(
            analyze_source("void helper() { main(); } void main() {}")
                .unwrap_err()
                .message()
                .contains("cannot be called")
        );
    }

    #[test]
    fn flattens_local_arrays_and_resolves_constant_expression_indices() {
        let program = analyze_source(
            "void main() { cell before; cell[4] values; values[1 + 2] = 7; output(values[3]); }",
        )
        .unwrap();
        let main = &program.functions[0];
        assert_eq!(main.local_count, 5);
        let HirStatementKind::Block(statements) = &main.body.kind else {
            panic!("expected function body block")
        };
        let HirStatementKind::Block(array_declarations) = &statements[1].kind else {
            panic!("expected flattened array declarations")
        };
        assert_eq!(array_declarations.len(), 4);
        for (index, declaration) in array_declarations.iter().enumerate() {
            assert!(matches!(
                declaration.kind,
                HirStatementKind::Declaration {
                    local,
                    initializer: None
                } if local == LocalId::new(index + 1)
            ));
        }
        assert!(matches!(
            statements[2].kind,
            HirStatementKind::Assignment {
                local,
                operator: hir::AssignmentOperator::Set,
                ..
            } if local == LocalId::new(4)
        ));
        assert!(matches!(
            statements[3].kind,
            HirStatementKind::Output(HirExpression {
                kind: HirExpressionKind::Local(local),
                ..
            }) if local == LocalId::new(4)
        ));
    }

    #[test]
    fn array_bindings_shadow_and_obey_scope_rules() {
        let program = analyze_source(
            "void main() { cell value; { cell[2] value; value[1] = 3; } value = 4; }",
        )
        .unwrap();
        assert_eq!(program.functions[0].local_count, 3);

        let duplicate = analyze_source("void main() { cell[2] values; cell values; }").unwrap_err();
        assert!(duplicate.message().contains("already declared"));
    }

    #[test]
    fn rejects_dynamic_out_of_bounds_and_invalid_array_uses() {
        let cases = [
            (
                "void main() { cell[4] values; cell i; output(values[i]); }",
                "dynamic array indices",
            ),
            (
                "void main() { cell[4] values; output(values[4]); }",
                "out of bounds",
            ),
            (
                "void main() { cell[4] values; output(values); }",
                "whole-array operations",
            ),
            (
                "void main() { cell[4] values; values = 0; }",
                "whole-array assignment",
            ),
            (
                "void main() { cell value; output(value[0]); }",
                "cannot be indexed",
            ),
            (
                "void consume(cell value) {} void main() { cell[4] values; consume(values); }",
                "whole-array operations",
            ),
        ];

        for (source, expected) in cases {
            let error = analyze_source(source).unwrap_err();
            assert!(
                error.message().contains(expected),
                "expected {expected:?} in {:?}",
                error.message()
            );
        }
    }
}
