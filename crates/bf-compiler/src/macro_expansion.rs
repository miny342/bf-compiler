//! Hygienic expansion of file-scope block macros.
//!
//! Expansion runs after parsing and before semantic analysis. It deliberately
//! operates on expressions rather than tokens: parameters may replace only an
//! expression/place occurrence, macro locals receive fresh identities, and
//! definition-site free names cannot be captured by call-site locals.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    ArrayLength, AstProgram, Expression, ExpressionKind, MacroDefinition, Name, NameContext,
    Statement, StatementKind, TopLevelItem, Type,
};
use crate::frontend::FrontendError;

/// Expand every macro invocation in function bodies.
///
/// Macro definitions remain in the returned top-level item list so the normal
/// file-scope duplicate-name checks can still account for them. No invocation
/// remains in a successfully expanded function body.
pub(crate) fn expand(mut program: AstProgram) -> Result<AstProgram, FrontendError> {
    let macros = collect_macros(&program)?;
    let mut expander = Expander {
        macros,
        next_synthetic: 0,
        active: Vec::new(),
    };
    // Macro bodies are declarations, so diagnose structurally invalid bodies,
    // bad nested invocations, and expansion cycles even when no function
    // happens to invoke them.  Literal placeholders are sufficient because
    // this pass deliberately leaves ordinary type/place validation to HIR
    // construction.
    let definitions: Vec<_> = program
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Macro(definition) => Some(definition.clone()),
            _ => None,
        })
        .collect();
    for definition in definitions {
        let arguments = definition
            .parameters
            .iter()
            .map(|parameter| Expression {
                kind: ExpressionKind::Literal(0),
                offset: parameter.offset,
            })
            .collect::<Vec<_>>();
        expander.instantiate_macro(&definition.name, &arguments, definition.name.offset)?;
    }
    for item in &mut program.items {
        if let TopLevelItem::Function(function) = item {
            function.body = expander.expand_call_site_statement(&function.body)?;
        }
    }
    Ok(program)
}

fn collect_macros(program: &AstProgram) -> Result<HashMap<String, MacroDefinition>, FrontendError> {
    let mut macros = HashMap::new();
    for item in &program.items {
        let TopLevelItem::Macro(definition) = item else {
            continue;
        };
        if macros
            .insert(definition.name.text.clone(), definition.clone())
            .is_some()
        {
            return Err(FrontendError::at(
                definition.name.offset,
                format!("macro {:?} is already defined", definition.name.text),
            ));
        }
        let mut parameters = HashSet::new();
        for parameter in &definition.parameters {
            if !parameters.insert(parameter.text.clone()) {
                return Err(FrontendError::at(
                    parameter.offset,
                    format!("macro parameter {:?} is duplicated", parameter.text),
                ));
            }
        }
    }
    Ok(macros)
}

struct Expander {
    macros: HashMap<String, MacroDefinition>,
    next_synthetic: u64,
    active: Vec<String>,
}

impl Expander {
    /// Walk ordinary source without changing its name contexts. Only actual
    /// macro invocation statements cross into definition-site instantiation.
    fn expand_call_site_statement(
        &mut self,
        statement: &Statement,
    ) -> Result<Statement, FrontendError> {
        let kind = match &statement.kind {
            StatementKind::Block {
                statements,
                closing_offset,
            } => StatementKind::Block {
                statements: statements
                    .iter()
                    .map(|statement| self.expand_call_site_statement(statement))
                    .collect::<Result<_, _>>()?,
                closing_offset: *closing_offset,
            },
            StatementKind::MacroInvocation { name, arguments } => {
                return self.instantiate_macro(name, arguments, statement.offset);
            }
            StatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => StatementKind::If {
                condition: condition.clone(),
                then_branch: Box::new(self.expand_call_site_statement(then_branch)?),
                else_branch: else_branch
                    .as_deref()
                    .map(|branch| self.expand_call_site_statement(branch).map(Box::new))
                    .transpose()?,
            },
            StatementKind::While { condition, body } => StatementKind::While {
                condition: condition.clone(),
                body: Box::new(self.expand_call_site_statement(body)?),
            },
            _ => statement.kind.clone(),
        };
        Ok(Statement {
            kind,
            offset: statement.offset,
        })
    }

    fn instantiate_macro(
        &mut self,
        name: &Name,
        arguments: &[Expression],
        invocation_offset: usize,
    ) -> Result<Statement, FrontendError> {
        let Some(definition) = self.macros.get(&name.text).cloned() else {
            return Err(FrontendError::at(
                name.offset,
                format!("undefined macro {:?}", name.text),
            ));
        };
        if definition.parameters.len() != arguments.len() {
            return Err(FrontendError::at(
                name.offset,
                format!(
                    "macro {:?} expects {} argument(s), but {} were provided",
                    name.text,
                    definition.parameters.len(),
                    arguments.len()
                ),
            ));
        }
        if let Some(cycle_start) = self.active.iter().position(|active| active == &name.text) {
            let mut cycle = self.active[cycle_start..].to_vec();
            cycle.push(name.text.clone());
            return Err(FrontendError::at(
                name.offset,
                format!("macro expansion cycle: {}", cycle.join(" -> ")),
            ));
        }

        let parameters = definition
            .parameters
            .iter()
            .zip(arguments)
            .map(|(parameter, argument)| (parameter.text.clone(), argument.clone()))
            .collect();
        self.active.push(name.text.clone());
        let mut environment = DefinitionEnvironment {
            parameters,
            scopes: Vec::new(),
        };
        let result = self.instantiate_statement(&definition.body, &mut environment);
        self.active.pop();
        let mut result = result?;
        debug_assert!(matches!(result.kind, StatementKind::Block { .. }));
        result.offset = invocation_offset;
        Ok(result)
    }

    fn instantiate_statement(
        &mut self,
        statement: &Statement,
        environment: &mut DefinitionEnvironment,
    ) -> Result<Statement, FrontendError> {
        let kind = match &statement.kind {
            StatementKind::Empty => StatementKind::Empty,
            StatementKind::Block {
                statements,
                closing_offset,
            } => {
                environment.scopes.push(HashMap::new());
                let result = statements
                    .iter()
                    .map(|statement| self.instantiate_statement(statement, environment))
                    .collect::<Result<Vec<_>, _>>();
                environment.scopes.pop();
                StatementKind::Block {
                    statements: result?,
                    closing_offset: *closing_offset,
                }
            }
            StatementKind::Declaration {
                ty,
                name,
                initializer,
            } => {
                self.reject_parameter_in_non_expression_name(
                    name,
                    environment,
                    "declaration name",
                )?;
                let ty = self.instantiate_type(ty, environment)?;
                // The declared local is not visible in its own initializer.
                let initializer = initializer
                    .as_ref()
                    .map(|value| self.instantiate_expression(value, environment))
                    .transpose()?;
                let scope = environment
                    .scopes
                    .last_mut()
                    .expect("macro body is a block");
                if scope.contains_key(&name.text) {
                    return Err(FrontendError::at(
                        name.offset,
                        format!(
                            "macro local {:?} is already declared in this scope",
                            name.text
                        ),
                    ));
                }
                let identity = self.fresh_identity();
                scope.insert(name.text.clone(), identity);
                StatementKind::Declaration {
                    ty,
                    name: synthetic_name(name, identity),
                    initializer,
                }
            }
            StatementKind::Assignment {
                target,
                operator,
                value,
            } => StatementKind::Assignment {
                target: self.instantiate_expression(target, environment)?,
                operator: *operator,
                value: self.instantiate_expression(value, environment)?,
            },
            StatementKind::Output(value) => {
                StatementKind::Output(self.instantiate_expression(value, environment)?)
            }
            StatementKind::Call(value) => {
                StatementKind::Call(self.instantiate_expression(value, environment)?)
            }
            StatementKind::MacroInvocation { name, arguments } => {
                self.reject_parameter_in_non_expression_name(name, environment, "macro name")?;
                let arguments = arguments
                    .iter()
                    .map(|argument| self.instantiate_expression(argument, environment))
                    .collect::<Result<Vec<_>, _>>()?;
                return self.instantiate_macro(name, &arguments, statement.offset);
            }
            StatementKind::Abort => StatementKind::Abort,
            StatementKind::Return(value) => StatementKind::Return(
                value
                    .as_ref()
                    .map(|value| self.instantiate_expression(value, environment))
                    .transpose()?,
            ),
            StatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => StatementKind::If {
                condition: self.instantiate_expression(condition, environment)?,
                then_branch: Box::new(self.instantiate_statement(then_branch, environment)?),
                else_branch: else_branch
                    .as_deref()
                    .map(|branch| {
                        self.instantiate_statement(branch, environment)
                            .map(Box::new)
                    })
                    .transpose()?,
            },
            StatementKind::While { condition, body } => StatementKind::While {
                condition: self.instantiate_expression(condition, environment)?,
                body: Box::new(self.instantiate_statement(body, environment)?),
            },
        };
        Ok(Statement {
            kind,
            offset: statement.offset,
        })
    }

    fn instantiate_expression(
        &mut self,
        expression: &Expression,
        environment: &DefinitionEnvironment,
    ) -> Result<Expression, FrontendError> {
        let kind = match &expression.kind {
            ExpressionKind::Literal(value) => ExpressionKind::Literal(*value),
            ExpressionKind::StringLiteral(value) => ExpressionKind::StringLiteral(value.clone()),
            ExpressionKind::Name(name) => {
                if let Some(argument) = environment.parameter(name) {
                    // Do not traverse a substituted argument: all of its names
                    // retain their call-site/outer-expansion identities.
                    return Ok(argument.clone());
                }
                ExpressionKind::Name(environment.bind_value_name(name))
            }
            ExpressionKind::EnumVariant { enum_name, variant } => {
                self.reject_parameter_in_non_expression_name(
                    enum_name,
                    environment,
                    "enum type name",
                )?;
                self.reject_parameter_in_non_expression_name(
                    variant,
                    environment,
                    "enum variant name",
                )?;
                ExpressionKind::EnumVariant {
                    enum_name: definition_name(enum_name),
                    variant: definition_name(variant),
                }
            }
            ExpressionKind::Input => ExpressionKind::Input,
            ExpressionKind::Call { name, arguments } => {
                self.reject_parameter_in_non_expression_name(name, environment, "function name")?;
                ExpressionKind::Call {
                    name: definition_name(name),
                    arguments: arguments
                        .iter()
                        .map(|argument| self.instantiate_expression(argument, environment))
                        .collect::<Result<_, _>>()?,
                }
            }
            ExpressionKind::MethodCall {
                receiver,
                name,
                arguments,
            } => {
                self.reject_parameter_in_non_expression_name(name, environment, "method name")?;
                ExpressionKind::MethodCall {
                    receiver: Box::new(self.instantiate_expression(receiver, environment)?),
                    name: definition_name(name),
                    arguments: arguments
                        .iter()
                        .map(|argument| self.instantiate_expression(argument, environment))
                        .collect::<Result<_, _>>()?,
                }
            }
            ExpressionKind::Field { base, field } => {
                self.reject_parameter_in_non_expression_name(field, environment, "field name")?;
                ExpressionKind::Field {
                    base: Box::new(self.instantiate_expression(base, environment)?),
                    field: definition_name(field),
                }
            }
            ExpressionKind::Index { base, index } => ExpressionKind::Index {
                base: Box::new(self.instantiate_expression(base, environment)?),
                index: Box::new(self.instantiate_expression(index, environment)?),
            },
            ExpressionKind::Len(value) => {
                ExpressionKind::Len(Box::new(self.instantiate_expression(value, environment)?))
            }
            ExpressionKind::Unary { operator, operand } => ExpressionKind::Unary {
                operator: *operator,
                operand: Box::new(self.instantiate_expression(operand, environment)?),
            },
            ExpressionKind::Binary {
                operator,
                left,
                right,
            } => ExpressionKind::Binary {
                operator: *operator,
                left: Box::new(self.instantiate_expression(left, environment)?),
                right: Box::new(self.instantiate_expression(right, environment)?),
            },
        };
        Ok(Expression {
            kind,
            offset: expression.offset,
        })
    }

    fn instantiate_type(
        &self,
        ty: &Type,
        environment: &DefinitionEnvironment,
    ) -> Result<Type, FrontendError> {
        Ok(match ty {
            Type::Cell => Type::Cell,
            Type::Named(name) => {
                self.reject_parameter_in_non_expression_name(name, environment, "type name")?;
                Type::Named(definition_name(name))
            }
            Type::Array { element, length } => Type::Array {
                element: Box::new(self.instantiate_type(element, environment)?),
                length: match length {
                    ArrayLength::Literal { value, offset } => ArrayLength::Literal {
                        value: *value,
                        offset: *offset,
                    },
                    ArrayLength::Constant(name) => {
                        self.reject_parameter_in_non_expression_name(
                            name,
                            environment,
                            "array length name",
                        )?;
                        ArrayLength::Constant(definition_name(name))
                    }
                },
            },
            Type::InferredCellArray => Type::InferredCellArray,
            Type::Void => Type::Void,
        })
    }

    fn reject_parameter_in_non_expression_name(
        &self,
        name: &Name,
        environment: &DefinitionEnvironment,
        role: &str,
    ) -> Result<(), FrontendError> {
        if environment.parameter(name).is_some() {
            return Err(FrontendError::at(
                name.offset,
                format!("macro parameter {:?} cannot be used as a {role}", name.text),
            ));
        }
        Ok(())
    }

    fn fresh_identity(&mut self) -> u64 {
        let identity = self.next_synthetic;
        self.next_synthetic = self
            .next_synthetic
            .checked_add(1)
            .expect("macro expansion identity space exhausted");
        identity
    }
}

struct DefinitionEnvironment {
    parameters: HashMap<String, Expression>,
    scopes: Vec<HashMap<String, u64>>,
}

impl DefinitionEnvironment {
    fn parameter(&self, name: &Name) -> Option<&Expression> {
        if name.context != NameContext::CallSite {
            return None;
        }
        self.parameters.get(&name.text)
    }

    fn bind_value_name(&self, name: &Name) -> Name {
        if name.context == NameContext::CallSite
            && let Some(identity) = self
                .scopes
                .iter()
                .rev()
                .find_map(|scope| scope.get(&name.text).copied())
        {
            return synthetic_name(name, identity);
        }
        definition_name(name)
    }
}

fn synthetic_name(name: &Name, identity: u64) -> Name {
    Name {
        text: name.text.clone(),
        offset: name.offset,
        context: NameContext::Synthetic(identity),
    }
}

fn definition_name(name: &Name) -> Name {
    Name {
        text: name.text.clone(),
        offset: name.offset,
        context: match name.context {
            NameContext::CallSite => NameContext::DefinitionSite,
            context => context,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser};

    fn expanded(source: &str) -> Result<AstProgram, FrontendError> {
        expand(parser::parse(lexer::lex(source)?)?)
    }

    fn main_body(program: &AstProgram) -> &[Statement] {
        let function = program
            .items
            .iter()
            .find_map(|item| match item {
                TopLevelItem::Function(function) if function.name.text == "main" => Some(function),
                _ => None,
            })
            .unwrap();
        let StatementKind::Block { statements, .. } = &function.body.kind else {
            panic!()
        };
        statements
    }

    #[test]
    fn substitutes_expression_and_place_parameters_without_capture() {
        let program = expanded(
            "macro assign(place, value) { cell temporary = value; place = temporary; } \
             void main() { cell temporary; cell[4] array; assign!(array[temporary], temporary + 1); }",
        )
        .unwrap();
        let StatementKind::Block { statements, .. } = &main_body(&program)[2].kind else {
            panic!()
        };
        let StatementKind::Declaration {
            name,
            initializer: Some(initializer),
            ..
        } = &statements[0].kind
        else {
            panic!()
        };
        let NameContext::Synthetic(identity) = name.context else {
            panic!()
        };
        let ExpressionKind::Binary { left, .. } = &initializer.kind else {
            panic!()
        };
        let ExpressionKind::Name(argument_name) = &left.kind else {
            panic!()
        };
        assert_eq!(argument_name.context, NameContext::CallSite);
        let StatementKind::Assignment { target, value, .. } = &statements[1].kind else {
            panic!()
        };
        assert!(matches!(target.kind, ExpressionKind::Index { .. }));
        assert!(matches!(
            value.kind,
            ExpressionKind::Name(Name { context: NameContext::Synthetic(found), .. }) if found == identity
        ));
    }

    #[test]
    fn every_expansion_allocates_fresh_local_identities() {
        let program = expanded(
            "macro emit(value) { cell saved = value; output(saved); } \
             void main() { emit!(1); emit!(2); }",
        )
        .unwrap();
        let identities: Vec<_> = main_body(&program)
            .iter()
            .map(|statement| {
                let StatementKind::Block { statements, .. } = &statement.kind else {
                    panic!()
                };
                let StatementKind::Declaration { name, .. } = &statements[0].kind else {
                    panic!()
                };
                name.context
            })
            .collect();
        assert!(matches!(identities[0], NameContext::Synthetic(_)));
        assert_ne!(identities[0], identities[1]);
    }

    #[test]
    fn free_names_bind_at_definition_site() {
        let program = expanded(
            "cell value; macro emit() { output(value); } \
             void main() { cell value = 7; emit!(); }",
        )
        .unwrap();
        let StatementKind::Block { statements, .. } = &main_body(&program)[1].kind else {
            panic!()
        };
        let StatementKind::Output(expression) = &statements[0].kind else {
            panic!()
        };
        let ExpressionKind::Name(name) = &expression.kind else {
            panic!()
        };
        assert_eq!(name.context, NameContext::DefinitionSite);
    }

    #[test]
    fn expands_nested_macros_and_preserves_a_block_per_invocation() {
        let program = expanded(
            "macro inner(x) { output(x); } macro outer(x) { inner!(x); } \
             void main() { outer!(9); }",
        )
        .unwrap();
        let StatementKind::Block {
            statements: outer, ..
        } = &main_body(&program)[0].kind
        else {
            panic!()
        };
        let StatementKind::Block {
            statements: inner, ..
        } = &outer[0].kind
        else {
            panic!()
        };
        assert!(matches!(
            inner[0].kind,
            StatementKind::Output(Expression {
                kind: ExpressionKind::Literal(9),
                ..
            })
        ));
    }

    #[test]
    fn macro_locals_obey_lexical_shadowing() {
        let program = expanded(
            "macro shadow() { cell value; { cell value; output(value); } output(value); } \
             void main() { shadow!(); }",
        )
        .unwrap();
        let StatementKind::Block { statements, .. } = &main_body(&program)[0].kind else {
            panic!()
        };
        let StatementKind::Declaration { name: outer, .. } = &statements[0].kind else {
            panic!()
        };
        let StatementKind::Block {
            statements: nested, ..
        } = &statements[1].kind
        else {
            panic!()
        };
        let StatementKind::Declaration { name: inner, .. } = &nested[0].kind else {
            panic!()
        };
        let StatementKind::Output(inner_use) = &nested[1].kind else {
            panic!()
        };
        let StatementKind::Output(outer_use) = &statements[2].kind else {
            panic!()
        };
        assert_ne!(outer.context, inner.context);
        assert!(matches!(
            inner_use.kind,
            ExpressionKind::Name(Name { context, .. }) if context == inner.context
        ));
        assert!(matches!(
            outer_use.kind,
            ExpressionKind::Name(Name { context, .. }) if context == outer.context
        ));
    }

    #[test]
    fn return_and_abort_keep_statement_semantics() {
        let program = expanded(
            "macro stop(condition) { if (condition) return; else abort(); } \
             void main() { stop!(input()); }",
        )
        .unwrap();
        let StatementKind::Block { statements, .. } = &main_body(&program)[0].kind else {
            panic!()
        };
        let StatementKind::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        } = &statements[0].kind
        else {
            panic!()
        };
        assert!(matches!(then_branch.kind, StatementKind::Return(None)));
        assert!(matches!(else_branch.kind, StatementKind::Abort));
    }

    #[test]
    fn diagnoses_arity_unknown_macro_and_cycles() {
        let cases = [
            (
                "macro one(x) {} void main() { one!(); }",
                "expects 1 argument",
            ),
            ("void main() { missing!(); }", "undefined macro"),
            (
                "macro first() { second!(); } macro second() { first!(); } void main() { first!(); }",
                "first -> second -> first",
            ),
        ];
        for (source, expected) in cases {
            let error = expanded(source).unwrap_err();
            assert!(error.message().contains(expected), "{:?}", error.message());
        }
    }

    #[test]
    fn rejects_parameters_in_token_generating_positions() {
        for source in [
            "macro bad(name) { name(); } void main() { bad!(other); }",
            "macro bad(name) { cell name; } void main() { bad!(other); }",
            "macro bad(name) { cell[name] value; } void main() { bad!(other); }",
        ] {
            let error = expanded(source).unwrap_err();
            assert!(
                error.message().contains("cannot be used"),
                "source {source:?}: {:?}",
                error.message()
            );
        }
    }

    #[test]
    fn validates_unreferenced_macro_declarations() {
        for (source, expected) in [
            (
                "macro first() { second!(); } macro second() { first!(); } void main() {}",
                "first -> second -> first",
            ),
            (
                "macro bad(name) { cell name; } void main() {}",
                "cannot be used",
            ),
            (
                "macro bad() { missing!(); } void main() {}",
                "undefined macro",
            ),
        ] {
            let error = expanded(source).unwrap_err();
            assert!(
                error.message().contains(expected),
                "source {source:?}: {:?}",
                error.message()
            );
        }
    }
}
