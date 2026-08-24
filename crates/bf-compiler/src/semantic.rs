//! Source-level name resolution and type checking.

use std::collections::HashMap;

use crate::ast;
use crate::frontend::FrontendError;
use crate::hir::{
    self, ArrayIndex, FunctionId, FunctionSignature, GlobalId, HirExpression, HirExpressionKind,
    HirFunction, HirGlobal, HirLocal, HirParameter, HirPlace, HirProgram, HirStatement,
    HirStatementKind, LocalId, VariableRef,
};

pub(crate) fn analyze(program: &ast::AstProgram) -> Result<HirProgram, FrontendError> {
    let file_scope = collect_file_scope(program)?;
    let entry = validate_main(program, &file_scope)?;

    let mut globals = Vec::new();
    for item in &program.items {
        let ast::TopLevelItem::Global(global) = item else {
            continue;
        };
        let FileSymbol::Global(registered) = file_scope[&global.name.text] else {
            unreachable!()
        };
        let initializer = global
            .initializer
            .as_ref()
            .map(|value| {
                let mut analyzer = FunctionAnalyzer::new(&file_scope, hir::Type::Void);
                let value = analyzer.analyze_expression(value)?;
                require_type(&value, hir::Type::Cell, "global initializer")?;
                Ok(value)
            })
            .transpose()?;
        globals.push(HirGlobal {
            id: registered.id,
            name: global.name.text.clone(),
            offset: global.name.offset,
            ty: registered.ty,
            initializer,
        });
    }

    let mut functions = Vec::new();
    for item in &program.items {
        let ast::TopLevelItem::Function(function) = item else {
            continue;
        };
        let FileSymbol::Function(registered) = &file_scope[&function.name.text] else {
            unreachable!()
        };
        functions.push(
            FunctionAnalyzer::new(&file_scope, registered.signature.return_type)
                .analyze(registered.id, function)?,
        );
    }

    Ok(HirProgram {
        entry,
        globals,
        functions,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredFunction {
    id: FunctionId,
    signature: FunctionSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisteredGlobal {
    id: GlobalId,
    ty: hir::Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileSymbol {
    Function(RegisteredFunction),
    Global(RegisteredGlobal),
}

fn collect_file_scope(
    program: &ast::AstProgram,
) -> Result<HashMap<String, FileSymbol>, FrontendError> {
    let mut scope = HashMap::new();
    let mut next_function = 0;
    let mut next_global = 0;
    for item in &program.items {
        let (name, symbol) = match item {
            ast::TopLevelItem::Function(function) => {
                let symbol = FileSymbol::Function(RegisteredFunction {
                    id: FunctionId::new(next_function),
                    signature: FunctionSignature {
                        return_type: hir_type(function.return_type),
                        parameter_types: function
                            .parameters
                            .iter()
                            .map(|parameter| hir_type(parameter.ty))
                            .collect(),
                    },
                });
                next_function += 1;
                (&function.name, symbol)
            }
            ast::TopLevelItem::Global(global) => {
                let symbol = FileSymbol::Global(RegisteredGlobal {
                    id: GlobalId::new(next_global),
                    ty: hir_type(global.ty),
                });
                next_global += 1;
                (&global.name, symbol)
            }
        };
        if scope.insert(name.text.clone(), symbol).is_some() {
            return Err(FrontendError::at(
                name.offset,
                format!("file-scope name {:?} is already defined", name.text),
            ));
        }
    }
    Ok(scope)
}

fn validate_main(
    program: &ast::AstProgram,
    scope: &HashMap<String, FileSymbol>,
) -> Result<FunctionId, FrontendError> {
    let Some(FileSymbol::Function(main)) = scope.get("main") else {
        return Err(FrontendError::at(
            program.eof_offset,
            "program must define exactly one 'void main()' function",
        ));
    };
    if main.signature.return_type != hir::Type::Void {
        return Err(FrontendError::at(
            function_offset(program, main.id),
            "main must have return type 'void'",
        ));
    }
    if !main.signature.parameter_types.is_empty() {
        return Err(FrontendError::at(
            function_offset(program, main.id),
            "main must not have parameters",
        ));
    }
    Ok(main.id)
}

fn function_offset(program: &ast::AstProgram, id: FunctionId) -> usize {
    program
        .items
        .iter()
        .filter_map(|item| match item {
            ast::TopLevelItem::Function(function) => Some(function.name.offset),
            ast::TopLevelItem::Global(_) => None,
        })
        .nth(id.index())
        .unwrap()
}

struct FunctionAnalyzer<'a> {
    file_scope: &'a HashMap<String, FileSymbol>,
    return_type: hir::Type,
    scopes: Vec<HashMap<String, LocalId>>,
    locals: Vec<HirLocal>,
}

impl<'a> FunctionAnalyzer<'a> {
    fn new(file_scope: &'a HashMap<String, FileSymbol>, return_type: hir::Type) -> Self {
        Self {
            file_scope,
            return_type,
            scopes: vec![HashMap::new()],
            locals: Vec::new(),
        }
    }

    fn analyze(
        mut self,
        id: FunctionId,
        function: &ast::Function,
    ) -> Result<HirFunction, FrontendError> {
        let mut parameters = Vec::with_capacity(function.parameters.len());
        for parameter in &function.parameters {
            let local = self.declare_local(&parameter.name, hir_type(parameter.ty))?;
            parameters.push(HirParameter {
                local,
                offset: parameter.name.offset,
            });
        }

        let (mut body, flow, closing_offset) = self.analyze_function_body(&function.body)?;
        match (self.return_type, flow) {
            (hir::Type::Cell | hir::Type::Array(_), Flow::FallsThrough) => {
                return Err(FrontendError::at(
                    closing_offset,
                    format!(
                        "{} function {:?} does not return a value on every path",
                        type_name(self.return_type),
                        function.name.text
                    ),
                ));
            }
            (hir::Type::Void, Flow::FallsThrough) => {
                let HirStatementKind::Block(statements) = &mut body.kind else {
                    unreachable!()
                };
                statements.push(HirStatement {
                    kind: HirStatementKind::Return(None),
                    offset: closing_offset,
                });
            }
            _ => {}
        }

        let FileSymbol::Function(registered) = &self.file_scope[&function.name.text] else {
            unreachable!()
        };
        Ok(HirFunction {
            id,
            name: function.name.text.clone(),
            offset: function.name.offset,
            signature: registered.signature.clone(),
            parameters,
            locals: self.locals,
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
            unreachable!("function bodies are blocks")
        };
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
                ty,
                name,
                initializer,
            } => {
                if self.scopes.last().unwrap().contains_key(&name.text) {
                    return Err(already_declared(name));
                }
                // A local is not visible in its own initializer.
                let initializer = initializer
                    .as_ref()
                    .map(|value| {
                        let value = self.analyze_expression(value)?;
                        require_type(&value, hir_type(*ty), "variable initializer")?;
                        Ok(value)
                    })
                    .transpose()?;
                let local = self.declare_local(name, hir_type(*ty))?;
                (
                    HirStatementKind::Declaration { local, initializer },
                    Flow::FallsThrough,
                )
            }
            ast::StatementKind::Assignment {
                target,
                operator,
                value,
            } => {
                // LANGUAGE.md specifies RHS evaluation before resolving/evaluating
                // the target index; HIR preserves that ordering explicitly.
                let value = self.analyze_expression(value)?;
                let target = self.resolve_place(target)?;
                match operator {
                    ast::AssignmentOperator::Set => {
                        require_type(&value, target.ty(), "assignment")?;
                    }
                    ast::AssignmentOperator::Add | ast::AssignmentOperator::Subtract => {
                        require_place_type(
                            &target,
                            hir::Type::Cell,
                            statement.offset,
                            "compound assignment target",
                        )?;
                        require_type(&value, hir::Type::Cell, "compound assignment value")?;
                    }
                }
                (
                    HirStatementKind::Assignment {
                        value,
                        target,
                        operator: assignment_operator(*operator),
                    },
                    Flow::FallsThrough,
                )
            }
            ast::StatementKind::Output(value) => {
                let value = self.analyze_expression(value)?;
                require_type(&value, hir::Type::Cell, "output argument")?;
                (HirStatementKind::Output(value), Flow::FallsThrough)
            }
            ast::StatementKind::Call { name, arguments } => {
                let (function, return_type, arguments) = self.analyze_call(name, arguments)?;
                if return_type != hir::Type::Void {
                    return Err(FrontendError::at(
                        name.offset,
                        format!(
                            "{}-returning function {:?} cannot be used as a statement",
                            type_name(return_type),
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
                    (hir::Type::Void, None) => None,
                    (hir::Type::Void, Some(value)) => {
                        return Err(FrontendError::at(
                            value.offset,
                            "void function must not return a value",
                        ));
                    }
                    (expected, Some(value)) => {
                        let value = self.analyze_expression(value)?;
                        require_type(&value, expected, "return value")?;
                        Some(value)
                    }
                    (expected, None) => {
                        return Err(FrontendError::at(
                            statement.offset,
                            format!("{} function must return a value", type_name(expected)),
                        ));
                    }
                };
                (HirStatementKind::Return(value), Flow::Returns)
            }
            ast::StatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.analyze_expression(condition)?;
                require_type(&condition, hir::Type::Cell, "if condition")?;
                let constant = hir::constant_cell_value(&condition).map(|value| value != 0);
                let (then_branch, then_flow) = self.analyze_statement(then_branch)?;
                let (else_branch, else_flow) = if let Some(branch) = else_branch {
                    let (branch, flow) = self.analyze_statement(branch)?;
                    (Some(Box::new(branch)), flow)
                } else {
                    (None, Flow::FallsThrough)
                };
                let flow = match constant {
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
                require_type(&condition, hir::Type::Cell, "while condition")?;
                let constant = hir::constant_cell_value(&condition).map(|value| value != 0);
                let (body, body_flow) = self.analyze_statement(body)?;
                let flow = if constant == Some(true) && body_flow == Flow::Returns {
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
        let (kind, ty) = match &expression.kind {
            ast::ExpressionKind::Literal(value) => {
                (HirExpressionKind::Literal(*value), hir::Type::Cell)
            }
            ast::ExpressionKind::Variable(name) => {
                let (variable, ty) = self.resolve_variable(name)?;
                (HirExpressionKind::Variable(variable), ty)
            }
            ast::ExpressionKind::ArrayElement { array, index } => {
                let (array, length, index) = self.resolve_indexed(array, index)?;
                (
                    HirExpressionKind::ArrayElement {
                        array,
                        length,
                        index,
                    },
                    hir::Type::Cell,
                )
            }
            ast::ExpressionKind::Input => (HirExpressionKind::Input, hir::Type::Cell),
            ast::ExpressionKind::Call { name, arguments } => {
                let (function, return_type, arguments) = self.analyze_call(name, arguments)?;
                if return_type == hir::Type::Void {
                    return Err(FrontendError::at(
                        name.offset,
                        format!("void function {:?} cannot be used as a value", name.text),
                    ));
                }
                (
                    HirExpressionKind::Call {
                        function,
                        arguments,
                    },
                    return_type,
                )
            }
            ast::ExpressionKind::Unary { operator, operand } => {
                let operand = self.analyze_expression(operand)?;
                require_type(&operand, hir::Type::Cell, "unary operand")?;
                (
                    HirExpressionKind::Unary {
                        operator: unary_operator(*operator),
                        operand: Box::new(operand),
                    },
                    hir::Type::Cell,
                )
            }
            ast::ExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.analyze_expression(left)?;
                require_type(&left, hir::Type::Cell, "left binary operand")?;
                let right = self.analyze_expression(right)?;
                require_type(&right, hir::Type::Cell, "right binary operand")?;
                (
                    HirExpressionKind::Binary {
                        operator: binary_operator(*operator),
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    hir::Type::Cell,
                )
            }
        };
        Ok(HirExpression {
            kind,
            ty,
            offset: expression.offset,
        })
    }

    fn analyze_call(
        &mut self,
        name: &ast::Name,
        arguments: &[ast::Expression],
    ) -> Result<(FunctionId, hir::Type, Vec<HirExpression>), FrontendError> {
        let Some(FileSymbol::Function(function)) = self.file_scope.get(&name.text) else {
            let message = if self.file_scope.contains_key(&name.text) {
                format!("global variable {:?} cannot be called", name.text)
            } else {
                format!("undefined function {:?}", name.text)
            };
            return Err(FrontendError::at(name.offset, message));
        };
        if name.text == "main" {
            return Err(FrontendError::at(name.offset, "main cannot be called"));
        }
        if arguments.len() != function.signature.parameter_types.len() {
            return Err(FrontendError::at(
                name.offset,
                format!(
                    "function {:?} expects {} argument(s), but {} were provided",
                    name.text,
                    function.signature.parameter_types.len(),
                    arguments.len()
                ),
            ));
        }
        let mut analyzed = Vec::with_capacity(arguments.len());
        for (index, (argument, expected)) in arguments
            .iter()
            .zip(&function.signature.parameter_types)
            .enumerate()
        {
            let argument = self.analyze_expression(argument)?;
            if argument.ty != *expected {
                return Err(FrontendError::at(
                    argument.offset,
                    format!(
                        "argument {} to {:?} has type {}, expected {}",
                        index + 1,
                        name.text,
                        type_name(argument.ty),
                        type_name(*expected)
                    ),
                ));
            }
            analyzed.push(argument);
        }
        Ok((function.id, function.signature.return_type, analyzed))
    }

    fn declare_local(&mut self, name: &ast::Name, ty: hir::Type) -> Result<LocalId, FrontendError> {
        if self.scopes.last().unwrap().contains_key(&name.text) {
            return Err(already_declared(name));
        }
        debug_assert!(ty.is_value());
        let id = LocalId::new(self.locals.len());
        self.locals.push(HirLocal {
            id,
            name: name.text.clone(),
            offset: name.offset,
            ty,
        });
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.text.clone(), id);
        Ok(id)
    }

    fn resolve_variable(
        &self,
        name: &ast::Name,
    ) -> Result<(VariableRef, hir::Type), FrontendError> {
        if let Some(local) = self
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name.text).copied())
        {
            return Ok((VariableRef::Local(local), self.locals[local.index()].ty));
        }
        match self.file_scope.get(&name.text) {
            Some(FileSymbol::Global(global)) => Ok((VariableRef::Global(global.id), global.ty)),
            Some(FileSymbol::Function(_)) => Err(FrontendError::at(
                name.offset,
                format!("function {:?} cannot be used as a variable", name.text),
            )),
            None => Err(FrontendError::at(
                name.offset,
                format!("undefined variable {:?}", name.text),
            )),
        }
    }

    fn resolve_place(&mut self, place: &ast::Place) -> Result<HirPlace, FrontendError> {
        let (variable, ty) = self.resolve_variable(&place.name)?;
        match (&place.index, ty) {
            (None, ty) => Ok(HirPlace::Variable { variable, ty }),
            (Some(index), hir::Type::Array(length)) => {
                let index = self.analyze_array_index(&place.name, length, index)?;
                Ok(HirPlace::ArrayElement {
                    array: variable,
                    length,
                    index,
                })
            }
            (Some(_), hir::Type::Cell) => Err(FrontendError::at(
                place.name.offset,
                format!("cell variable {:?} cannot be indexed", place.name.text),
            )),
            (_, hir::Type::Void) => unreachable!(),
        }
    }

    fn resolve_indexed(
        &mut self,
        name: &ast::Name,
        index: &ast::Expression,
    ) -> Result<(VariableRef, usize, ArrayIndex), FrontendError> {
        let (variable, ty) = self.resolve_variable(name)?;
        let hir::Type::Array(length) = ty else {
            return Err(FrontendError::at(
                name.offset,
                format!("cell variable {:?} cannot be indexed", name.text),
            ));
        };
        let index = self.analyze_array_index(name, length, index)?;
        Ok((variable, length, index))
    }

    fn analyze_array_index(
        &mut self,
        name: &ast::Name,
        length: usize,
        index: &ast::Expression,
    ) -> Result<ArrayIndex, FrontendError> {
        let index_expression = self.analyze_expression(index)?;
        require_type(&index_expression, hir::Type::Cell, "array index")?;
        if let Some(value) = hir::constant_cell_value(&index_expression) {
            if usize::from(value) >= length {
                return Err(FrontendError::at(
                    index.offset,
                    format!(
                        "array index {value} is out of bounds for {:?} with length {length}",
                        name.text
                    ),
                ));
            }
            Ok(ArrayIndex::Constant(value))
        } else {
            Ok(ArrayIndex::Dynamic(Box::new(index_expression)))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    FallsThrough,
    Returns,
}

fn require_type(
    expression: &HirExpression,
    expected: hir::Type,
    context: &str,
) -> Result<(), FrontendError> {
    if expression.ty == expected {
        return Ok(());
    }
    Err(FrontendError::at(
        expression.offset,
        format!(
            "{context} has type {}, expected {}",
            type_name(expression.ty),
            type_name(expected)
        ),
    ))
}

fn require_place_type(
    place: &HirPlace,
    expected: hir::Type,
    offset: usize,
    context: &str,
) -> Result<(), FrontendError> {
    if place.ty() == expected {
        return Ok(());
    }
    Err(FrontendError::at(
        offset,
        format!(
            "{context} has type {}, expected {}",
            type_name(place.ty()),
            type_name(expected)
        ),
    ))
}

fn type_name(ty: hir::Type) -> String {
    match ty {
        hir::Type::Cell => "cell".to_owned(),
        hir::Type::Array(length) => format!("cell[{length}]"),
        hir::Type::Void => "void".to_owned(),
    }
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
        ast::Type::Array(length) => hir::Type::Array(length),
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
        analyze(&parser::parse(lexer::lex(source)?)?)
    }

    #[test]
    fn resolves_file_scope_globals_functions_and_shadowing() {
        let program = analyze_source(
            "cell count = 1; cell[4] data; cell read() { return count; } \
             void main() { cell count = 2; data[count] = read(); output(data[2]); }",
        )
        .unwrap();
        assert_eq!(program.globals.len(), 2);
        assert_eq!(program.functions.len(), 2);
        assert_eq!(program.functions[1].locals.len(), 1);

        for source in [
            "cell same; void same() {} void main() {}",
            "cell same; cell same; void main() {}",
            "void same() {} void same() {} void main() {}",
        ] {
            assert!(
                analyze_source(source)
                    .unwrap_err()
                    .message()
                    .contains("already defined")
            );
        }
    }

    #[test]
    fn parameters_share_body_scope_and_nested_blocks_may_shadow() {
        let error =
            analyze_source("cell same(cell value) { cell value; return value; } void main() {}")
                .unwrap_err();
        assert!(error.message().contains("already declared"));

        let program = analyze_source(
            "cell[2] same(cell[2] value) { { cell value; } return value; } void main() {}",
        )
        .unwrap();
        assert_eq!(program.functions[0].locals.len(), 2);
        assert_eq!(program.functions[0].locals[0].ty, hir::Type::Array(2));
        assert_eq!(program.functions[0].locals[1].ty, hir::Type::Cell);
    }

    #[test]
    fn global_initializers_are_general_typed_expressions_in_declaration_order() {
        let program = analyze_source(
            "cell later = first(); cell earlier = input(); cell first() { return earlier; } void main() {}",
        )
        .unwrap();
        assert_eq!(program.globals[0].name, "later");
        assert!(matches!(
            program.globals[0].initializer.as_ref().unwrap().kind,
            HirExpressionKind::Call { .. }
        ));
        assert!(matches!(
            program.globals[1].initializer.as_ref().unwrap().kind,
            HirExpressionKind::Input
        ));
    }

    #[test]
    fn retains_array_identity_and_constant_or_dynamic_indices() {
        let program = analyze_source(
            "cell[4] global; void main() { cell[4] local; cell i; local[1 + 2] = global[i]; }",
        )
        .unwrap();
        assert_eq!(program.functions[0].locals.len(), 2);
        assert_eq!(program.functions[0].locals[0].ty, hir::Type::Array(4));
        let HirStatementKind::Block(body) = &program.functions[0].body.kind else {
            panic!()
        };
        let HirStatementKind::Assignment { target, value, .. } = &body[2].kind else {
            panic!()
        };
        assert!(matches!(
            target,
            HirPlace::ArrayElement {
                index: ArrayIndex::Constant(3),
                ..
            }
        ));
        assert!(matches!(
            value.kind,
            HirExpressionKind::ArrayElement {
                index: ArrayIndex::Dynamic(_),
                ..
            }
        ));
    }

    #[test]
    fn supports_typed_whole_array_assignment_parameters_and_returns() {
        let program = analyze_source(
            "cell[4] copy(cell[4] value) { cell[4] result; result = value; return result; } \
             void main() { cell[4] source; cell[4] target; target = copy(source); }",
        )
        .unwrap();
        assert_eq!(
            program.functions[0].signature.return_type,
            hir::Type::Array(4)
        );
        assert_eq!(
            program.functions[0].signature.parameter_types,
            vec![hir::Type::Array(4)]
        );
        let HirStatementKind::Block(body) = &program.functions[1].body.kind else {
            panic!()
        };
        let HirStatementKind::Assignment { target, value, .. } = &body[2].kind else {
            panic!()
        };
        assert_eq!(target.ty(), hir::Type::Array(4));
        assert_eq!(value.ty, hir::Type::Array(4));
    }

    #[test]
    fn enforces_call_arity_and_value_context() {
        let arity =
            analyze_source("cell helper(cell x) { return x; } void main() { output(helper()); }")
                .unwrap_err();
        assert!(arity.message().contains("expects 1 argument"));

        let void_value =
            analyze_source("void helper() {} void main() { cell x = helper(); }").unwrap_err();
        assert!(void_value.message().contains("cannot be used as a value"));

        let aggregate_statement =
            analyze_source("cell[2] helper() { cell[2] x; return x; } void main() { helper(); }")
                .unwrap_err();
        assert!(
            aggregate_statement
                .message()
                .contains("cannot be used as a statement")
        );
    }

    #[test]
    fn diagnoses_all_array_type_mismatches() {
        let cases = [
            (
                "void main() { cell[2] a; cell[3] b; a = b; }",
                "expected cell[2]",
            ),
            (
                "void take(cell[2] a) {} void main() { cell[3] b; take(b); }",
                "expected cell[2]",
            ),
            (
                "cell[2] make() { cell[3] a; return a; } void main() {}",
                "expected cell[2]",
            ),
            ("void main() { cell[2] a; output(a); }", "expected cell"),
            (
                "void main() { cell[2] a; a += a; }",
                "compound assignment target",
            ),
            ("void main() { cell[2] a; if (a) {} }", "if condition"),
            ("void main() { cell[2] a; output(a[2]); }", "out of bounds"),
        ];
        for (source, expected) in cases {
            let error = analyze_source(source).unwrap_err();
            assert!(error.message().contains(expected), "{:?}", error.message());
        }
    }

    #[test]
    fn keeps_rhs_before_dynamic_assignment_index_in_hir() {
        let program = analyze_source("void main() { cell[4] a; a[input()] = input(); }").unwrap();
        let HirStatementKind::Block(body) = &program.functions[0].body.kind else {
            panic!()
        };
        let HirStatementKind::Assignment { target, value, .. } = &body[1].kind else {
            panic!()
        };
        assert!(matches!(value.kind, HirExpressionKind::Input));
        assert!(matches!(
            target,
            HirPlace::ArrayElement {
                index: ArrayIndex::Dynamic(index),
                ..
            } if matches!(index.kind, HirExpressionKind::Input)
        ));
    }

    #[test]
    fn resolves_forward_and_mutual_calls_and_definite_returns() {
        let program = analyze_source(
            "cell first(cell n) { if (n) return second(n - 1); else return 0; } \
             cell second(cell n) { if (n) return first(n - 1); else return 1; } \
             void main() {}",
        )
        .unwrap();
        assert_eq!(program.entry, FunctionId::new(2));
        assert!(
            analyze_source("cell[2] bad() { cell[2] a; if (input()) return a; } void main() {}")
                .unwrap_err()
                .message()
                .contains("every path")
        );
    }

    #[test]
    fn constant_conditions_refine_definite_return_flow() {
        analyze_source(
            "cell from_if() { if (!(1 - 1)) return 7; } \
             cell[2] from_while() { cell[2] a; while (2 > 1 && 3) return a; } \
             void main() {}",
        )
        .unwrap();

        for source in [
            "cell bad() { if (0) return 1; } void main() {}",
            "cell[2] bad() { cell[2] a; while (0) return a; } void main() {}",
        ] {
            assert!(
                analyze_source(source)
                    .unwrap_err()
                    .message()
                    .contains("every path")
            );
        }
    }

    #[test]
    fn validates_main_and_forbids_calls_to_it() {
        assert!(
            analyze_source("cell global;")
                .unwrap_err()
                .message()
                .contains("void main()")
        );
        assert!(
            analyze_source("cell main;")
                .unwrap_err()
                .message()
                .contains("void main()")
        );
        assert!(
            analyze_source("cell main() { return 0; }")
                .unwrap_err()
                .message()
                .contains("return type")
        );
        assert!(
            analyze_source("void helper() { main(); } void main() {}")
                .unwrap_err()
                .message()
                .contains("cannot be called")
        );
    }
}
