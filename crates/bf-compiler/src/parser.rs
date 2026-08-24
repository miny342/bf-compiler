use crate::ast::{
    AssignmentOperator, AstProgram, BinaryOperator, Expression, ExpressionKind, Function, Name,
    Parameter, Statement, StatementKind, Type, UnaryOperator,
};
use crate::frontend::FrontendError;
use crate::lexer::{Token, TokenKind};

pub(crate) fn parse(tokens: Vec<Token>) -> Result<AstProgram, FrontendError> {
    Parser {
        tokens,
        position: 0,
    }
    .parse_program()
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn parse_program(mut self) -> Result<AstProgram, FrontendError> {
        let mut functions = Vec::new();
        while !self.at(&TokenKind::Eof) {
            functions.push(self.parse_function()?);
        }
        Ok(AstProgram {
            functions,
            eof_offset: self.current().offset,
        })
    }

    fn parse_function(&mut self) -> Result<Function, FrontendError> {
        let return_type = match self.current().kind {
            TokenKind::Cell => {
                self.advance();
                if self.at(&TokenKind::LeftBracket) {
                    return Err(
                        self.error_here("arrays are not implemented in this compiler stage")
                    );
                }
                Type::Cell
            }
            TokenKind::Void => {
                self.advance();
                Type::Void
            }
            _ => {
                return Err(self.error_here(
                    "top-level items must be function definitions; global variables are not implemented",
                ));
            }
        };

        let name = self.parse_name("expected a function name")?;
        if self.at(&TokenKind::LeftBracket) {
            return Err(self.error_here("arrays are not implemented in this compiler stage"));
        }
        if !self.at(&TokenKind::LeftParen) {
            return Err(
                self.error_here("global variables are not implemented in this compiler stage")
            );
        }
        self.advance();

        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                if self.at(&TokenKind::Void) {
                    return Err(self.error_here("function parameters must have type 'cell'"));
                }
                self.expect(TokenKind::Cell, "expected parameter type 'cell'")?;
                if self.at(&TokenKind::LeftBracket) {
                    return Err(
                        self.error_here("arrays are not implemented in this compiler stage")
                    );
                }
                let name = self.parse_name("expected a parameter name")?;
                if self.at(&TokenKind::LeftBracket) {
                    return Err(
                        self.error_here("arrays are not implemented in this compiler stage")
                    );
                }
                parameters.push(Parameter { name });
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
        }
        self.expect(TokenKind::RightParen, "expected ')' after parameters")?;
        let body = self.parse_block()?;
        Ok(Function {
            return_type,
            name,
            parameters,
            body,
        })
    }

    fn parse_block_item(&mut self) -> Result<Statement, FrontendError> {
        if self.at(&TokenKind::Cell) {
            self.parse_declaration()
        } else {
            self.parse_statement()
        }
    }

    fn parse_statement(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        match self.current().kind.clone() {
            TokenKind::Semicolon => {
                self.advance();
                Ok(Statement {
                    kind: StatementKind::Empty,
                    offset,
                })
            }
            TokenKind::LeftBrace => self.parse_block(),
            TokenKind::Cell => {
                if self.cell_starts_function() {
                    Err(self.error_here("nested functions are not allowed"))
                } else {
                    Err(self.error_here("a declaration here must be inside a block"))
                }
            }
            TokenKind::Void => Err(self.error_here("nested functions are not allowed")),
            TokenKind::Return => self.parse_return(),
            TokenKind::Output => self.parse_output(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::Identifier(_) => self.parse_assignment_or_call(),
            TokenKind::RightBrace => Err(self.error_here("unexpected '}'")),
            TokenKind::Else => Err(self.error_here("'else' without a matching 'if'")),
            _ => Err(self.error_here("expected a statement")),
        }
    }

    fn parse_block(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        self.expect(TokenKind::LeftBrace, "expected '{'")?;
        let mut statements = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.error_here("unterminated block"));
            }
            statements.push(self.parse_block_item()?);
        }
        let closing_offset = self.current().offset;
        self.advance();
        Ok(Statement {
            kind: StatementKind::Block {
                statements,
                closing_offset,
            },
            offset,
        })
    }

    fn parse_declaration(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        self.advance();
        if self.at(&TokenKind::LeftBracket) {
            return Err(self.error_here("arrays are not implemented in this compiler stage"));
        }
        let name = self.parse_name("expected a variable name after 'cell'")?;
        if self.at(&TokenKind::LeftParen) {
            return Err(self.error_here("nested functions are not allowed"));
        }
        if self.at(&TokenKind::LeftBracket) {
            return Err(self.error_here("arrays are not implemented in this compiler stage"));
        }
        let initializer = if self.at(&TokenKind::Assign) {
            self.advance();
            Some(self.parse_expression()?)
        } else {
            None
        };
        self.expect(TokenKind::Semicolon, "expected ';' after declaration")?;
        Ok(Statement {
            kind: StatementKind::Declaration { name, initializer },
            offset,
        })
    }

    fn parse_assignment_or_call(&mut self) -> Result<Statement, FrontendError> {
        let name = self.parse_name("expected assignment target or function name")?;
        let offset = name.offset;
        if self.at(&TokenKind::LeftParen) {
            let arguments = self.parse_call_arguments()?;
            self.expect(TokenKind::Semicolon, "expected ';' after function call")?;
            return Ok(Statement {
                kind: StatementKind::Call { name, arguments },
                offset,
            });
        }
        if self.at(&TokenKind::LeftBracket) {
            return Err(self.error_here("arrays are not implemented in this compiler stage"));
        }
        let operator = match self.current().kind {
            TokenKind::Assign => AssignmentOperator::Set,
            TokenKind::PlusAssign => AssignmentOperator::Add,
            TokenKind::MinusAssign => AssignmentOperator::Subtract,
            _ => return Err(self.error_here("expected '=', '+=', '-=' or '(' after identifier")),
        };
        self.advance();
        let value = self.parse_expression()?;
        self.expect(TokenKind::Semicolon, "expected ';' after assignment")?;
        Ok(Statement {
            kind: StatementKind::Assignment {
                name,
                operator,
                value,
            },
            offset,
        })
    }

    fn parse_return(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        self.advance();
        let value = if self.at(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect(TokenKind::Semicolon, "expected ';' after return")?;
        Ok(Statement {
            kind: StatementKind::Return(value),
            offset,
        })
    }

    fn parse_output(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'output'")?;
        let value = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')' after output value")?;
        self.expect(TokenKind::Semicolon, "expected ';' after output")?;
        Ok(Statement {
            kind: StatementKind::Output(value),
            offset,
        })
    }

    fn parse_if(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'if'")?;
        let condition = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')' after if condition")?;
        let then_branch = Box::new(self.parse_statement()?);
        let else_branch = if self.at(&TokenKind::Else) {
            self.advance();
            Some(Box::new(self.parse_statement()?))
        } else {
            None
        };
        Ok(Statement {
            kind: StatementKind::If {
                condition,
                then_branch,
                else_branch,
            },
            offset,
        })
    }

    fn parse_while(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'while'")?;
        let condition = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')' after while condition")?;
        let body = Box::new(self.parse_statement()?);
        Ok(Statement {
            kind: StatementKind::While { condition, body },
            offset,
        })
    }

    fn parse_expression(&mut self) -> Result<Expression, FrontendError> {
        self.parse_logical_or()
    }

    fn parse_logical_or(&mut self) -> Result<Expression, FrontendError> {
        self.parse_binary_left_associative(
            Self::parse_logical_and,
            &[(TokenKind::PipePipe, BinaryOperator::LogicalOr)],
        )
    }

    fn parse_logical_and(&mut self) -> Result<Expression, FrontendError> {
        self.parse_binary_left_associative(
            Self::parse_equality,
            &[(TokenKind::AmpAmp, BinaryOperator::LogicalAnd)],
        )
    }

    fn parse_equality(&mut self) -> Result<Expression, FrontendError> {
        self.parse_binary_left_associative(
            Self::parse_comparison,
            &[
                (TokenKind::EqualEqual, BinaryOperator::Equal),
                (TokenKind::BangEqual, BinaryOperator::NotEqual),
            ],
        )
    }

    fn parse_comparison(&mut self) -> Result<Expression, FrontendError> {
        self.parse_binary_left_associative(
            Self::parse_additive,
            &[
                (TokenKind::Less, BinaryOperator::Less),
                (TokenKind::LessEqual, BinaryOperator::LessEqual),
                (TokenKind::Greater, BinaryOperator::Greater),
                (TokenKind::GreaterEqual, BinaryOperator::GreaterEqual),
            ],
        )
    }

    fn parse_additive(&mut self) -> Result<Expression, FrontendError> {
        self.parse_binary_left_associative(
            Self::parse_unary,
            &[
                (TokenKind::Plus, BinaryOperator::Add),
                (TokenKind::Minus, BinaryOperator::Subtract),
            ],
        )
    }

    fn parse_binary_left_associative(
        &mut self,
        parse_operand: fn(&mut Self) -> Result<Expression, FrontendError>,
        operators: &[(TokenKind, BinaryOperator)],
    ) -> Result<Expression, FrontendError> {
        let mut expression = parse_operand(self)?;
        while let Some((_, operator)) = operators.iter().find(|(token, _)| self.at(token)) {
            let offset = self.current().offset;
            self.advance();
            let right = parse_operand(self)?;
            expression = Expression {
                kind: ExpressionKind::Binary {
                    operator: *operator,
                    left: Box::new(expression),
                    right: Box::new(right),
                },
                offset,
            };
        }
        Ok(expression)
    }

    fn parse_unary(&mut self) -> Result<Expression, FrontendError> {
        let operator = match self.current().kind {
            TokenKind::Plus => Some(UnaryOperator::Plus),
            TokenKind::Minus => Some(UnaryOperator::Negate),
            TokenKind::Bang => Some(UnaryOperator::Not),
            _ => None,
        };
        if let Some(operator) = operator {
            let offset = self.current().offset;
            self.advance();
            let operand = self.parse_unary()?;
            Ok(Expression {
                kind: ExpressionKind::Unary {
                    operator,
                    operand: Box::new(operand),
                },
                offset,
            })
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Expression, FrontendError> {
        let token = self.current().clone();
        match token.kind {
            TokenKind::Number(value) => {
                self.advance();
                Ok(Expression {
                    kind: ExpressionKind::Literal(value),
                    offset: token.offset,
                })
            }
            TokenKind::Identifier(text) => {
                self.advance();
                let name = Name {
                    text,
                    offset: token.offset,
                };
                if self.at(&TokenKind::LeftParen) {
                    let arguments = self.parse_call_arguments()?;
                    return Ok(Expression {
                        kind: ExpressionKind::Call { name, arguments },
                        offset: token.offset,
                    });
                }
                if self.at(&TokenKind::LeftBracket) {
                    return Err(
                        self.error_here("arrays are not implemented in this compiler stage")
                    );
                }
                Ok(Expression {
                    kind: ExpressionKind::Variable(name),
                    offset: token.offset,
                })
            }
            TokenKind::Input => {
                self.advance();
                self.expect(TokenKind::LeftParen, "expected '(' after 'input'")?;
                self.expect(TokenKind::RightParen, "input takes no arguments")?;
                Ok(Expression {
                    kind: ExpressionKind::Input,
                    offset: token.offset,
                })
            }
            TokenKind::LeftParen => {
                self.advance();
                let expression = self.parse_expression()?;
                self.expect(TokenKind::RightParen, "expected ')' after expression")?;
                Ok(expression)
            }
            _ => Err(FrontendError::at(token.offset, "expected an expression")),
        }
    }

    fn parse_call_arguments(&mut self) -> Result<Vec<Expression>, FrontendError> {
        self.expect(TokenKind::LeftParen, "expected '('")?;
        let mut arguments = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                arguments.push(self.parse_expression()?);
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
        }
        self.expect(TokenKind::RightParen, "expected ')' after arguments")?;
        Ok(arguments)
    }

    fn cell_starts_function(&self) -> bool {
        matches!(
            (
                self.tokens.get(self.position + 1).map(|token| &token.kind),
                self.tokens.get(self.position + 2).map(|token| &token.kind),
            ),
            (Some(TokenKind::Identifier(_)), Some(TokenKind::LeftParen))
        )
    }

    fn parse_name(&mut self, message: &str) -> Result<Name, FrontendError> {
        let token = self.current().clone();
        if let TokenKind::Identifier(text) = token.kind {
            self.advance();
            Ok(Name {
                text,
                offset: token.offset,
            })
        } else {
            Err(FrontendError::at(token.offset, message))
        }
    }

    fn expect(&mut self, expected: TokenKind, message: &str) -> Result<(), FrontendError> {
        if self.at(&expected) {
            self.advance();
            Ok(())
        } else {
            Err(self.error_here(message))
        }
    }

    fn at(&self, expected: &TokenKind) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(expected)
    }

    fn current(&self) -> &Token {
        &self.tokens[self.position]
    }

    fn advance(&mut self) {
        self.position += 1;
    }

    fn error_here(&self, message: impl Into<String>) -> FrontendError {
        FrontendError::at(self.current().offset, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;

    fn parse_source(source: &str) -> Result<AstProgram, FrontendError> {
        parse(lexer::lex(source)?)
    }

    #[test]
    fn parses_multiple_functions_calls_and_returns() {
        let program = parse_source(
            "cell add(cell left, cell right) { return left + right; }\n\
             void main() { output(add(1, 2)); return; }",
        )
        .unwrap();
        assert_eq!(program.functions.len(), 2);
        assert_eq!(program.functions[0].parameters.len(), 2);
    }

    #[test]
    fn rejects_globals_arrays_and_nested_functions_explicitly() {
        assert!(
            parse_source("cell global;")
                .unwrap_err()
                .message()
                .contains("global")
        );
        assert!(
            parse_source("cell[4] values;")
                .unwrap_err()
                .message()
                .contains("arrays")
        );
        assert!(
            parse_source("void main() { cell nested() {} }")
                .unwrap_err()
                .message()
                .contains("nested")
        );
    }
}
