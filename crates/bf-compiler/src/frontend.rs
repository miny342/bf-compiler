use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::ast::{
    AssignmentOperator, AstProgram, BinaryOperator, Expression, ExpressionKind, Name, Statement,
    UnaryOperator,
};
use crate::{
    CellId, CodegenError, Instruction, IrError, Program, TransferTarget, compile, lexer, parser,
};

/// A source-level compilation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendError {
    offset: Option<usize>,
    message: String,
}

impl FrontendError {
    pub(crate) fn at(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset: Some(offset),
            message: message.into(),
        }
    }

    fn without_offset(message: impl Into<String>) -> Self {
        Self {
            offset: None,
            message: message.into(),
        }
    }

    pub const fn offset(&self) -> Option<usize> {
        self.offset
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for FrontendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(offset) = self.offset {
            write!(f, "{} at byte offset {offset}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl Error for FrontendError {}

/// Error returned by [`compile_source`].
#[derive(Debug)]
pub enum SourceCompileError {
    Frontend(FrontendError),
    Codegen(CodegenError),
}

impl fmt::Display for SourceCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frontend(error) => error.fmt(f),
            Self::Codegen(error) => error.fmt(f),
        }
    }
}

impl Error for SourceCompileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frontend(error) => Some(error),
            Self::Codegen(error) => Some(error),
        }
    }
}

impl From<FrontendError> for SourceCompileError {
    fn from(error: FrontendError) -> Self {
        Self::Frontend(error)
    }
}

impl From<CodegenError> for SourceCompileError {
    fn from(error: CodegenError) -> Self {
        Self::Codegen(error)
    }
}

/// Parse and lower BFC source to validated cell IR.
pub fn lower_source(source: &str) -> Result<Program, FrontendError> {
    let tokens = lexer::lex(source)?;
    let ast = parser::parse(tokens)?;
    Lowerer::new().lower_program(&ast)
}

/// Compile BFC source directly to Brainfuck source.
pub fn compile_source(source: &str) -> Result<String, SourceCompileError> {
    let program = lower_source(source)?;
    Ok(compile(&program)?)
}

struct Lowerer {
    scopes: Vec<HashMap<String, CellId>>,
    next_cell: usize,
}

impl Lowerer {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            next_cell: 0,
        }
    }

    fn lower_program(mut self, program: &AstProgram) -> Result<Program, FrontendError> {
        let mut instructions = Vec::new();
        self.lower_statements(&program.main_body, &mut instructions)?;
        Program::new(self.next_cell, instructions).map_err(Self::ir_error)
    }

    fn lower_statements(
        &mut self,
        statements: &[Statement],
        output: &mut Vec<Instruction>,
    ) -> Result<(), FrontendError> {
        for statement in statements {
            self.lower_statement(statement, output)?;
        }
        Ok(())
    }

    fn lower_statement(
        &mut self,
        statement: &Statement,
        output: &mut Vec<Instruction>,
    ) -> Result<(), FrontendError> {
        match statement {
            Statement::Empty => {}
            Statement::Block(statements) => {
                self.scopes.push(HashMap::new());
                let result = self.lower_statements(statements, output);
                self.scopes.pop();
                result?;
            }
            Statement::Declaration { name, initializer } => {
                if self.scopes.last().unwrap().contains_key(&name.text) {
                    return Err(FrontendError::at(
                        name.offset,
                        format!("variable {:?} is already declared in this scope", name.text),
                    ));
                }

                let cell = self.allocate_cell();
                if let Some(initializer) = initializer {
                    let value = self.allocate_cell();
                    self.evaluate(initializer, value, output)?;
                    self.move_value(value, cell, output);
                } else {
                    output.push(Instruction::Set {
                        dst: cell,
                        value: 0,
                    });
                }
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(name.text.clone(), cell);
            }
            Statement::Assignment {
                name,
                operator,
                value,
            } => {
                let destination = self.resolve(name)?;
                let temporary = self.allocate_cell();
                self.evaluate(value, temporary, output)?;
                match operator {
                    AssignmentOperator::Set => self.move_value(temporary, destination, output),
                    AssignmentOperator::Add => self.transfer(temporary, destination, 1, output),
                    AssignmentOperator::Subtract => {
                        self.transfer(temporary, destination, 255, output)
                    }
                }
            }
            Statement::Output(expression) => {
                let value = self.allocate_cell();
                self.evaluate(expression, value, output)?;
                output.push(Instruction::Output { src: value });
            }
            Statement::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition_cell = self.allocate_cell();
                self.evaluate(condition, condition_cell, output)?;
                let mut then_body = Vec::new();
                self.lower_statement(then_branch, &mut then_body)?;
                let mut else_body = Vec::new();
                if let Some(else_branch) = else_branch {
                    self.lower_statement(else_branch, &mut else_body)?;
                }
                output.push(Instruction::Branch {
                    condition: condition_cell,
                    then_body,
                    else_body,
                });
            }
            Statement::While { condition, body } => {
                let condition_cell = self.allocate_cell();
                let mut evaluate_condition = Vec::new();
                self.evaluate(condition, condition_cell, &mut evaluate_condition)?;
                output.extend(evaluate_condition.iter().cloned());
                let mut loop_body = Vec::new();
                self.lower_statement(body, &mut loop_body)?;
                loop_body.extend(evaluate_condition);
                output.push(Instruction::Loop {
                    condition: condition_cell,
                    body: loop_body,
                });
            }
        }
        Ok(())
    }

    fn evaluate(
        &mut self,
        expression: &Expression,
        destination: CellId,
        output: &mut Vec<Instruction>,
    ) -> Result<(), FrontendError> {
        match &expression.kind {
            ExpressionKind::Literal(value) => output.push(Instruction::Set {
                dst: destination,
                value: *value,
            }),
            ExpressionKind::Variable(name) => {
                let source = self.resolve(name)?;
                self.copy_value(source, destination, output);
            }
            ExpressionKind::Input => output.push(Instruction::Input { dst: destination }),
            ExpressionKind::Unary { operator, operand } => match operator {
                UnaryOperator::Plus => self.evaluate(operand, destination, output)?,
                UnaryOperator::Negate => {
                    let value = self.allocate_cell();
                    self.evaluate(operand, value, output)?;
                    output.push(Instruction::Set {
                        dst: destination,
                        value: 0,
                    });
                    self.transfer(value, destination, 255, output);
                }
                UnaryOperator::Not => {
                    let value = self.allocate_cell();
                    self.evaluate(operand, value, output)?;
                    self.boolean_from(value, destination, 0, 1, output);
                }
            },
            ExpressionKind::Binary {
                operator,
                left,
                right,
            } => match operator {
                BinaryOperator::Add | BinaryOperator::Subtract => {
                    self.evaluate(left, destination, output)?;
                    let right_value = self.allocate_cell();
                    self.evaluate(right, right_value, output)?;
                    let factor = if *operator == BinaryOperator::Add {
                        1
                    } else {
                        255
                    };
                    self.transfer(right_value, destination, factor, output);
                }
                BinaryOperator::Equal | BinaryOperator::NotEqual => {
                    let left_value = self.allocate_cell();
                    self.evaluate(left, left_value, output)?;
                    let right_value = self.allocate_cell();
                    self.evaluate(right, right_value, output)?;
                    self.transfer(right_value, left_value, 255, output);
                    let (nonzero, zero) = if *operator == BinaryOperator::Equal {
                        (0, 1)
                    } else {
                        (1, 0)
                    };
                    self.boolean_from(left_value, destination, nonzero, zero, output);
                }
                BinaryOperator::Less
                | BinaryOperator::LessEqual
                | BinaryOperator::Greater
                | BinaryOperator::GreaterEqual => {
                    let left_value = self.allocate_cell();
                    self.evaluate(left, left_value, output)?;
                    let right_value = self.allocate_cell();
                    self.evaluate(right, right_value, output)?;
                    match operator {
                        BinaryOperator::Less => {
                            self.less_than(left_value, right_value, destination, 1, 0, output)
                        }
                        BinaryOperator::LessEqual => {
                            self.less_than(right_value, left_value, destination, 0, 1, output)
                        }
                        BinaryOperator::Greater => {
                            self.less_than(right_value, left_value, destination, 1, 0, output)
                        }
                        BinaryOperator::GreaterEqual => {
                            self.less_than(left_value, right_value, destination, 0, 1, output)
                        }
                        _ => unreachable!(),
                    }
                }
                BinaryOperator::LogicalAnd | BinaryOperator::LogicalOr => {
                    let left_value = self.allocate_cell();
                    self.evaluate(left, left_value, output)?;

                    let right_value = self.allocate_cell();
                    let mut evaluate_right = Vec::new();
                    self.evaluate(right, right_value, &mut evaluate_right)?;
                    self.boolean_from(right_value, destination, 1, 0, &mut evaluate_right);

                    let (then_body, else_body) = if *operator == BinaryOperator::LogicalAnd {
                        (
                            evaluate_right,
                            vec![Instruction::Set {
                                dst: destination,
                                value: 0,
                            }],
                        )
                    } else {
                        (
                            vec![Instruction::Set {
                                dst: destination,
                                value: 1,
                            }],
                            evaluate_right,
                        )
                    };
                    output.push(Instruction::Branch {
                        condition: left_value,
                        then_body,
                        else_body,
                    });
                }
            },
        }
        Ok(())
    }

    fn boolean_from(
        &self,
        value: CellId,
        destination: CellId,
        nonzero: u8,
        zero: u8,
        output: &mut Vec<Instruction>,
    ) {
        output.push(Instruction::Branch {
            condition: value,
            then_body: vec![Instruction::Set {
                dst: destination,
                value: nonzero,
            }],
            else_body: vec![Instruction::Set {
                dst: destination,
                value: zero,
            }],
        });
    }

    fn less_than(
        &mut self,
        left: CellId,
        right: CellId,
        destination: CellId,
        true_value: u8,
        false_value: u8,
        output: &mut Vec<Instruction>,
    ) {
        let right_test = self.allocate_cell();
        let mut body = Vec::new();
        self.copy_value(right, right_test, &mut body);
        body.push(Instruction::Branch {
            condition: right_test,
            then_body: vec![
                Instruction::AddConst {
                    dst: left,
                    value: 255,
                },
                Instruction::AddConst {
                    dst: right,
                    value: 255,
                },
            ],
            else_body: vec![Instruction::Transfer {
                src: left,
                targets: Vec::new(),
            }],
        });
        output.push(Instruction::Loop {
            condition: left,
            body,
        });
        self.boolean_from(right, destination, true_value, false_value, output);
    }

    fn copy_value(&mut self, source: CellId, destination: CellId, output: &mut Vec<Instruction>) {
        let restore = self.allocate_cell();
        output.push(Instruction::Set {
            dst: destination,
            value: 0,
        });
        output.push(Instruction::Transfer {
            src: source,
            targets: vec![
                TransferTarget {
                    dst: destination,
                    factor: 1,
                },
                TransferTarget {
                    dst: restore,
                    factor: 1,
                },
            ],
        });
        self.transfer(restore, source, 1, output);
    }

    fn move_value(&mut self, source: CellId, destination: CellId, output: &mut Vec<Instruction>) {
        output.push(Instruction::Set {
            dst: destination,
            value: 0,
        });
        self.transfer(source, destination, 1, output);
    }

    fn transfer(
        &self,
        source: CellId,
        destination: CellId,
        factor: u8,
        output: &mut Vec<Instruction>,
    ) {
        output.push(Instruction::Transfer {
            src: source,
            targets: vec![TransferTarget {
                dst: destination,
                factor,
            }],
        });
    }

    fn allocate_cell(&mut self) -> CellId {
        let cell = CellId::new(self.next_cell);
        self.next_cell += 1;
        cell
    }

    fn resolve(&self, name: &Name) -> Result<CellId, FrontendError> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name.text).copied())
            .ok_or_else(|| {
                FrontendError::at(name.offset, format!("undefined variable {:?}", name.text))
            })
    }

    fn ir_error(error: IrError) -> FrontendError {
        FrontendError::without_offset(format!("invalid generated IR: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use bf_interpreter::run;

    use super::*;

    fn main_source(body: &str) -> String {
        format!("void main() {{\n{body}\n}}")
    }

    fn execute(source: &str, input: &[u8]) -> Vec<u8> {
        let brainfuck = compile_source(&main_source(source)).unwrap();
        run(brainfuck.as_bytes(), input).unwrap()
    }

    #[test]
    fn compiles_an_echo_loop() {
        let source = r#"
            cell ch;
            ch = input();
            while (ch != 0) {
                output(ch);
                ch = input();
            }
        "#;
        assert_eq!(execute(source, b"hello"), b"hello");
    }

    #[test]
    fn arithmetic_wraps_and_variable_reads_are_non_destructive() {
        let source = r#"
            cell original = 'A';
            cell next = original + 1;
            output(original);
            output(next);
            original -= 66;
            output(original);
        "#;
        assert_eq!(execute(source, b""), vec![b'A', b'B', 255]);
    }

    #[test]
    fn if_else_not_and_nonzero_comparison_work() {
        let source = r#"
            cell value = input();
            if (value != 0) {
                output('T');
            } else {
                output('F');
            }
            if (!value) {
                output('0');
            } else {
                output('1');
            }
            output(value);
        "#;
        assert_eq!(execute(source, &[7]), vec![b'T', b'1', 7]);
        assert_eq!(execute(source, &[0]), vec![b'F', b'0', 0]);
    }

    #[test]
    fn all_unsigned_comparisons_handle_boundaries_and_preserve_operands() {
        let source = r#"
            cell left = input();
            cell right = input();
            output(left == right);
            output(left != right);
            output(left < right);
            output(left <= right);
            output(left > right);
            output(left >= right);
            output(left);
            output(right);
        "#;
        let brainfuck = compile_source(&main_source(source)).unwrap();
        let cases = [
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (1, 2),
            (2, 1),
            (0, 255),
            (255, 0),
            (254, 255),
            (255, 255),
        ];

        for (left, right) in cases {
            let output = run(brainfuck.as_bytes(), &[left, right]).unwrap();
            assert_eq!(
                output,
                vec![
                    u8::from(left == right),
                    u8::from(left != right),
                    u8::from(left < right),
                    u8::from(left <= right),
                    u8::from(left > right),
                    u8::from(left >= right),
                    left,
                    right,
                ],
                "comparison results for {left} and {right}",
            );
        }
    }

    #[test]
    fn logical_operators_short_circuit_and_return_booleans() {
        let source = r#"
            cell zero = 0;
            cell one = 1;
            output(zero && input());
            output(input());
            output(one || input());
            output(input());
            output(one && input());
            output(zero || input());
        "#;
        assert_eq!(
            execute(source, &[b'A', b'B', 2, 0]),
            vec![0, b'A', 1, b'B', 1, 0]
        );
    }

    #[test]
    fn stage_four_operator_precedence_matches_the_specification() {
        let source = r#"
            output(1 || 0 && 0);
            output(2 == 1 < 2);
            output(1 + 2 < 4 == 1);
        "#;
        assert_eq!(execute(source, b""), vec![1, 0, 1]);
    }

    #[test]
    fn blocks_shadow_outer_variables() {
        let source = r#"
            // 外側の値は内側のブロックを抜けても残る。
            cell value = 1;
            {
                cell value = 2;
                /* 日本語の
                   ブロックコメント */
                output(value);
            }
            output(value);
        "#;
        assert_eq!(execute(source, b""), vec![2, 1]);
    }

    #[test]
    fn reports_source_errors_with_offsets() {
        let undefined_source = main_source("output(missing);");
        let undefined = lower_source(&undefined_source).unwrap_err();
        assert_eq!(undefined.offset(), undefined_source.find("missing"));
        assert!(undefined.message().contains("undefined variable"));

        let future_feature = lower_source(&main_source("future_function(1);")).unwrap_err();
        assert!(future_feature.message().contains("function calls"));

        let future_array = lower_source(&main_source("cell[16] values;")).unwrap_err();
        assert!(future_array.message().contains("arrays"));

        let future_definition = lower_source("void helper() {}").unwrap_err();
        assert!(
            future_definition
                .message()
                .contains("only the 'main' function")
        );

        let unbraced_declaration =
            lower_source(&main_source("cell x; if (x) cell y;")).unwrap_err();
        assert!(unbraced_declaration.message().contains("inside a block"));

        let comment = lower_source(&main_source("/* no end")).unwrap_err();
        assert!(comment.message().contains("unterminated block comment"));

        let non_ascii_code = lower_source(&main_source("cell 値;")).unwrap_err();
        assert!(
            non_ascii_code
                .message()
                .contains("only allowed inside comments")
        );
    }

    #[test]
    fn requires_exactly_one_parameterless_void_main() {
        let empty = lower_source("").unwrap_err();
        assert!(empty.message().contains("void main()"));

        let top_level_statement = lower_source("output('x');").unwrap_err();
        assert!(
            top_level_statement
                .message()
                .contains("start with 'void main()'")
        );

        let parameter = lower_source("void main(cell value) {}").unwrap_err();
        assert!(parameter.message().contains("must not have parameters"));

        let second_function = lower_source("void main() {} void helper() {}").unwrap_err();
        assert!(second_function.message().contains("only one top-level"));
    }

    #[test]
    fn rejects_all_non_builtin_function_calls() {
        let statement_call = lower_source(&main_source("helper();")).unwrap_err();
        assert!(statement_call.message().contains("function calls"));

        let expression_call = lower_source(&main_source("cell value = helper();")).unwrap_err();
        assert!(expression_call.message().contains("function calls"));
    }
}
