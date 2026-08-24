use crate::ast::{
    AssignmentOperator, AstProgram, BinaryOperator, Expression, ExpressionKind, Name, Statement,
    UnaryOperator,
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
        if self.at(&TokenKind::Eof) {
            return Err(self.error_here("program must define 'void main()'"));
        }

        if !self.at(&TokenKind::Void) {
            return Err(self.error_here("program must start with 'void main()'"));
        }
        self.advance();

        let name = self.parse_name("expected 'main' after 'void'")?;
        if name.text != "main" {
            return Err(FrontendError::at(
                name.offset,
                "only the 'main' function is implemented in this compiler stage",
            ));
        }

        self.expect(TokenKind::LeftParen, "expected '(' after 'main'")?;
        if !self.at(&TokenKind::RightParen) {
            return Err(self.error_here("main must not have parameters"));
        }
        self.advance();

        let Statement::Block(main_body) = self.parse_block()? else {
            unreachable!();
        };
        if !self.at(&TokenKind::Eof) {
            return Err(self.error_here(
                "only one top-level 'void main()' function is implemented in this compiler stage",
            ));
        }

        Ok(AstProgram { main_body })
    }

    fn parse_block_item(&mut self) -> Result<Statement, FrontendError> {
        if self.at(&TokenKind::Cell) {
            self.parse_declaration()
        } else {
            self.parse_statement()
        }
    }

    fn parse_statement(&mut self) -> Result<Statement, FrontendError> {
        match self.current().kind.clone() {
            TokenKind::Semicolon => {
                self.advance();
                Ok(Statement::Empty)
            }
            TokenKind::LeftBrace => self.parse_block(),
            TokenKind::Cell => Err(self.error_here("a declaration here must be inside a block")),
            TokenKind::Void => Err(self.error_here("nested functions are not allowed")),
            TokenKind::Return => {
                Err(self.error_here("return is not implemented in this compiler stage"))
            }
            TokenKind::Output => self.parse_output(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::Identifier(_) => self.parse_assignment(),
            TokenKind::RightBrace => Err(self.error_here("unexpected '}'")),
            TokenKind::Else => Err(self.error_here("'else' without a matching 'if'")),
            _ => Err(self.error_here("expected a statement")),
        }
    }

    fn parse_block(&mut self) -> Result<Statement, FrontendError> {
        self.expect(TokenKind::LeftBrace, "expected '{'")?;
        let mut statements = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.error_here("unterminated block"));
            }
            statements.push(self.parse_block_item()?);
        }
        self.advance();
        Ok(Statement::Block(statements))
    }

    fn parse_declaration(&mut self) -> Result<Statement, FrontendError> {
        self.advance();
        if self.at(&TokenKind::LeftBracket) {
            return Err(self.error_here("arrays are not implemented in this compiler stage"));
        }
        let name = self.parse_name("expected a variable name after 'cell'")?;
        if self.at(&TokenKind::LeftParen) {
            return Err(self.error_here("functions are not implemented in this compiler stage"));
        }
        let initializer = if self.at(&TokenKind::Assign) {
            self.advance();
            Some(self.parse_expression()?)
        } else {
            None
        };
        self.expect(TokenKind::Semicolon, "expected ';' after declaration")?;
        Ok(Statement::Declaration { name, initializer })
    }

    fn parse_assignment(&mut self) -> Result<Statement, FrontendError> {
        let name = self.parse_name("expected assignment target")?;
        if self.at(&TokenKind::LeftParen) {
            return Err(
                self.error_here("function calls are not implemented in this compiler stage")
            );
        }
        if self.at(&TokenKind::LeftBracket) {
            return Err(self.error_here("arrays are not implemented in this compiler stage"));
        }
        let operator = match self.current().kind {
            TokenKind::Assign => AssignmentOperator::Set,
            TokenKind::PlusAssign => AssignmentOperator::Add,
            TokenKind::MinusAssign => AssignmentOperator::Subtract,
            _ => return Err(self.error_here("expected '=', '+=' or '-=' after variable")),
        };
        self.advance();
        let value = self.parse_expression()?;
        self.expect(TokenKind::Semicolon, "expected ';' after assignment")?;
        Ok(Statement::Assignment {
            name,
            operator,
            value,
        })
    }

    fn parse_output(&mut self) -> Result<Statement, FrontendError> {
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'output'")?;
        let value = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')' after output value")?;
        self.expect(TokenKind::Semicolon, "expected ';' after output")?;
        Ok(Statement::Output(value))
    }

    fn parse_if(&mut self) -> Result<Statement, FrontendError> {
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
        Ok(Statement::If {
            condition,
            then_branch,
            else_branch,
        })
    }

    fn parse_while(&mut self) -> Result<Statement, FrontendError> {
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'while'")?;
        let condition = self.parse_expression()?;
        self.expect(TokenKind::RightParen, "expected ')' after while condition")?;
        let body = Box::new(self.parse_statement()?);
        Ok(Statement::While { condition, body })
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
                if self.at(&TokenKind::LeftParen) {
                    return Err(self
                        .error_here("function calls are not implemented in this compiler stage"));
                }
                if self.at(&TokenKind::LeftBracket) {
                    return Err(
                        self.error_here("arrays are not implemented in this compiler stage")
                    );
                }
                let name = Name {
                    text,
                    offset: token.offset,
                };
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
