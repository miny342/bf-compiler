use crate::ast::{
    ArrayLength, AssignmentOperator, AstProgram, BinaryOperator, ConstantDefinition,
    EnumDefinition, EnumVariant, Expression, ExpressionKind, Function, Global, MacroDefinition,
    Name, NameContext, Parameter, Statement, StatementKind, StructDefinition, StructField,
    TopLevelItem, Type, UnaryOperator,
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
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            items.push(self.parse_top_level_item()?);
        }
        Ok(AstProgram {
            items,
            eof_offset: self.current().offset,
        })
    }

    fn parse_top_level_item(&mut self) -> Result<TopLevelItem, FrontendError> {
        match self.current().kind {
            TokenKind::Enum => return self.parse_enum().map(TopLevelItem::Enum),
            TokenKind::Struct => return self.parse_struct().map(TopLevelItem::Struct),
            TokenKind::Const => return self.parse_constant().map(TopLevelItem::Constant),
            TokenKind::Macro => return self.parse_macro().map(TopLevelItem::Macro),
            _ => {}
        }

        let ty = self.parse_type(true, true)?;
        let name = self.parse_name("expected a global variable or function name")?;
        if self.at(&TokenKind::LeftParen) {
            return self
                .parse_function_after_name(ty, name)
                .map(TopLevelItem::Function);
        }
        if matches!(ty, Type::Void) {
            return Err(FrontendError::at(
                name.offset,
                "global variables cannot have type 'void'",
            ));
        }
        let initializer = self.parse_optional_initializer()?;
        self.expect(
            TokenKind::Semicolon,
            "expected ';' after global declaration",
        )?;
        Ok(TopLevelItem::Global(Global {
            ty,
            name,
            initializer,
        }))
    }

    fn parse_enum(&mut self) -> Result<EnumDefinition, FrontendError> {
        self.advance();
        let name = self.parse_name("expected enum name")?;
        self.expect(TokenKind::LeftBrace, "expected '{' after enum name")?;
        let mut variants = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            let variant = self.parse_name("expected enum variant name")?;
            let discriminant = if self.at(&TokenKind::Assign) {
                self.advance();
                Some(self.parse_expression()?)
            } else {
                None
            };
            variants.push(EnumVariant {
                name: variant,
                discriminant,
            });
            if !self.at(&TokenKind::Comma) {
                break;
            }
            self.advance();
            if self.at(&TokenKind::RightBrace) {
                break;
            }
        }
        self.expect(TokenKind::RightBrace, "expected '}' after enum variants")?;
        Ok(EnumDefinition { name, variants })
    }

    fn parse_struct(&mut self) -> Result<StructDefinition, FrontendError> {
        self.advance();
        let name = self.parse_name("expected struct name")?;
        self.expect(TokenKind::LeftBrace, "expected '{' after struct name")?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.error_here("unterminated struct definition"));
            }
            let ty = self.parse_type(false, false)?;
            let field = self.parse_name("expected struct field name")?;
            self.expect(TokenKind::Semicolon, "expected ';' after struct field")?;
            fields.push(StructField { ty, name: field });
        }
        self.advance();
        Ok(StructDefinition { name, fields })
    }

    fn parse_constant(&mut self) -> Result<ConstantDefinition, FrontendError> {
        self.advance();
        self.expect(
            TokenKind::Cell,
            "compile-time constants must have type 'cell'",
        )?;
        let name = self.parse_name("expected constant name")?;
        self.expect(TokenKind::Assign, "expected '=' in constant definition")?;
        let initializer = self.parse_expression()?;
        self.expect(
            TokenKind::Semicolon,
            "expected ';' after constant definition",
        )?;
        Ok(ConstantDefinition { name, initializer })
    }

    fn parse_macro(&mut self) -> Result<MacroDefinition, FrontendError> {
        self.advance();
        let name = self.parse_name("expected macro name")?;
        self.expect(TokenKind::LeftParen, "expected '(' after macro name")?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                parameters.push(self.parse_name("expected macro parameter name")?);
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
        }
        self.expect(TokenKind::RightParen, "expected ')' after macro parameters")?;
        let body = self.parse_block()?;
        Ok(MacroDefinition {
            name,
            parameters,
            body,
        })
    }

    fn parse_function_after_name(
        &mut self,
        return_type: Type,
        name: Name,
    ) -> Result<Function, FrontendError> {
        if matches!(return_type, Type::InferredCellArray) {
            return Err(FrontendError::at(
                name.offset,
                "cell[] cannot be used as a function return type",
            ));
        }
        self.advance();
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                let ty = self.parse_type(false, false)?;
                let name = self.parse_name("expected a parameter name")?;
                parameters.push(Parameter { ty, name });
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
        if self.at(&TokenKind::Cell) || self.named_type_starts_declaration() {
            self.parse_declaration()
        } else {
            self.parse_statement()
        }
    }

    fn parse_statement(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        if self.at(&TokenKind::Cell) || self.named_type_starts_declaration() {
            return Err(self.error_here("a declaration here must be inside a block"));
        }
        match self.current().kind.clone() {
            TokenKind::Semicolon => {
                self.advance();
                Ok(Statement {
                    kind: StatementKind::Empty,
                    offset,
                })
            }
            TokenKind::LeftBrace => self.parse_block(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Output => self.parse_output(),
            TokenKind::Abort => self.parse_abort(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::Identifier(_)
            | TokenKind::Input
            | TokenKind::Len
            | TokenKind::Number(_)
            | TokenKind::Character(_)
            | TokenKind::String(_)
            | TokenKind::LeftParen
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Bang => self.parse_assignment_call_or_macro(),
            TokenKind::Cell | TokenKind::Void => {
                Err(self.error_here("nested functions are not allowed"))
            }
            TokenKind::Enum | TokenKind::Struct | TokenKind::Const | TokenKind::Macro => {
                Err(self.error_here("this definition is only allowed at file scope"))
            }
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
        let ty = self.parse_type(false, true)?;
        let name = self.parse_name("expected a variable name")?;
        if self.at(&TokenKind::LeftParen) {
            return Err(self.error_here("nested functions are not allowed"));
        }
        let initializer = self.parse_optional_initializer()?;
        self.expect(TokenKind::Semicolon, "expected ';' after declaration")?;
        Ok(Statement {
            kind: StatementKind::Declaration {
                ty,
                name,
                initializer,
            },
            offset,
        })
    }

    fn parse_assignment_call_or_macro(&mut self) -> Result<Statement, FrontendError> {
        if matches!(self.peek_kind(1), Some(TokenKind::Bang)) {
            let name = self.parse_name("expected macro name")?;
            self.expect(TokenKind::Bang, "expected '!' after macro name")?;
            let arguments = self.parse_call_arguments()?;
            self.expect(TokenKind::Semicolon, "expected ';' after macro invocation")?;
            return Ok(Statement {
                offset: name.offset,
                kind: StatementKind::MacroInvocation { name, arguments },
            });
        }

        let expression = self.parse_expression()?;
        let offset = expression.offset;
        let operator = match self.current().kind {
            TokenKind::Assign => Some(AssignmentOperator::Set),
            TokenKind::PlusAssign => Some(AssignmentOperator::Add),
            TokenKind::MinusAssign => Some(AssignmentOperator::Subtract),
            _ => None,
        };
        let kind = if let Some(operator) = operator {
            self.advance();
            let value = self.parse_expression()?;
            StatementKind::Assignment {
                target: expression,
                operator,
                value,
            }
        } else if matches!(
            expression.kind,
            ExpressionKind::Call { .. } | ExpressionKind::MethodCall { .. }
        ) {
            StatementKind::Call(expression)
        } else {
            return Err(self.error_here("expected assignment operator or a function call"));
        };
        self.expect(TokenKind::Semicolon, "expected ';' after statement")?;
        Ok(Statement { kind, offset })
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

    fn parse_abort(&mut self) -> Result<Statement, FrontendError> {
        let offset = self.current().offset;
        self.advance();
        self.expect(TokenKind::LeftParen, "expected '(' after 'abort'")?;
        self.expect(TokenKind::RightParen, "abort takes no arguments")?;
        self.expect(TokenKind::Semicolon, "expected ';' after abort")?;
        Ok(Statement {
            kind: StatementKind::Abort,
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
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expression, FrontendError> {
        let mut expression = self.parse_primary()?;
        loop {
            if self.at(&TokenKind::LeftBracket) {
                let offset = self.current().offset;
                self.advance();
                let index = self.parse_expression()?;
                self.expect(TokenKind::RightBracket, "expected ']' after array index")?;
                expression = Expression {
                    kind: ExpressionKind::Index {
                        base: Box::new(expression),
                        index: Box::new(index),
                    },
                    offset,
                };
            } else if self.at(&TokenKind::Dot) {
                let offset = self.current().offset;
                self.advance();
                let name = self.parse_name("expected field or method name after '.'")?;
                if self.at(&TokenKind::LeftParen) {
                    let arguments = self.parse_call_arguments()?;
                    expression = Expression {
                        kind: ExpressionKind::MethodCall {
                            receiver: Box::new(expression),
                            name,
                            arguments,
                        },
                        offset,
                    };
                } else {
                    expression = Expression {
                        kind: ExpressionKind::Field {
                            base: Box::new(expression),
                            field: name,
                        },
                        offset,
                    };
                }
            } else {
                break;
            }
        }
        Ok(expression)
    }

    fn parse_primary(&mut self) -> Result<Expression, FrontendError> {
        let token = self.current().clone();
        match token.kind {
            TokenKind::Number(value) => {
                self.advance();
                let value = u8::try_from(value).map_err(|_| {
                    FrontendError::at(token.offset, "integer literal must be between 0 and 255")
                })?;
                Ok(Expression {
                    kind: ExpressionKind::Literal(value),
                    offset: token.offset,
                })
            }
            TokenKind::Character(value) => {
                self.advance();
                Ok(Expression {
                    kind: ExpressionKind::Literal(value),
                    offset: token.offset,
                })
            }
            TokenKind::String(value) => {
                self.advance();
                Ok(Expression {
                    kind: ExpressionKind::StringLiteral(value),
                    offset: token.offset,
                })
            }
            TokenKind::Identifier(text) => {
                self.advance();
                let name = Name {
                    text,
                    offset: token.offset,
                    context: NameContext::CallSite,
                };
                if self.at(&TokenKind::ColonColon) {
                    self.advance();
                    let variant = self.parse_name("expected enum variant after '::'")?;
                    return Ok(Expression {
                        kind: ExpressionKind::EnumVariant {
                            enum_name: name,
                            variant,
                        },
                        offset: token.offset,
                    });
                }
                if self.at(&TokenKind::LeftParen) {
                    let arguments = self.parse_call_arguments()?;
                    return Ok(Expression {
                        kind: ExpressionKind::Call { name, arguments },
                        offset: token.offset,
                    });
                }
                Ok(Expression {
                    kind: ExpressionKind::Name(name),
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
            TokenKind::Len => {
                self.advance();
                self.expect(TokenKind::LeftParen, "expected '(' after 'len'")?;
                let operand = self.parse_expression()?;
                self.expect(TokenKind::RightParen, "expected ')' after len operand")?;
                Ok(Expression {
                    kind: ExpressionKind::Len(Box::new(operand)),
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

    fn parse_type(
        &mut self,
        allow_void: bool,
        allow_inferred: bool,
    ) -> Result<Type, FrontendError> {
        let mut ty = match self.current().kind.clone() {
            TokenKind::Cell => {
                self.advance();
                Type::Cell
            }
            TokenKind::Identifier(text) => {
                let offset = self.current().offset;
                self.advance();
                Type::Named(Name {
                    text,
                    offset,
                    context: NameContext::CallSite,
                })
            }
            TokenKind::Void if allow_void => {
                self.advance();
                return Ok(Type::Void);
            }
            TokenKind::Void => {
                return Err(self.error_here("variables and parameters cannot have type 'void'"));
            }
            _ if allow_void => {
                return Err(self.error_here("expected 'cell' or 'void' (or a named type)"));
            }
            _ => return Err(self.error_here("expected a value type")),
        };

        let mut lengths = Vec::new();
        while self.at(&TokenKind::LeftBracket) {
            let bracket_offset = self.current().offset;
            self.advance();
            if self.at(&TokenKind::RightBracket) {
                self.advance();
                if allow_inferred && matches!(ty, Type::Cell) && lengths.is_empty() {
                    if self.at(&TokenKind::LeftBracket) {
                        return Err(FrontendError::at(
                            bracket_offset,
                            "cell[] cannot have additional array dimensions",
                        ));
                    }
                    return Ok(Type::InferredCellArray);
                }
                return Err(FrontendError::at(
                    bracket_offset,
                    "an inferred array length is only valid for cell[] string declarations",
                ));
            }
            let token = self.current().clone();
            let length = match token.kind {
                TokenKind::Number(value) if value <= 256 => ArrayLength::Literal {
                    value: usize::from(value),
                    offset: token.offset,
                },
                TokenKind::Number(_) => {
                    return Err(FrontendError::at(
                        token.offset,
                        "array length must be between 0 and 256",
                    ));
                }
                TokenKind::Identifier(text) => ArrayLength::Constant(Name {
                    text,
                    offset: token.offset,
                    context: NameContext::CallSite,
                }),
                _ => {
                    return Err(FrontendError::at(
                        token.offset,
                        "array length must be an integer or const cell name",
                    ));
                }
            };
            self.advance();
            self.expect(TokenKind::RightBracket, "expected ']' after array length")?;
            lengths.push(length);
        }
        for length in lengths.into_iter().rev() {
            ty = Type::Array {
                element: Box::new(ty),
                length,
            };
        }
        Ok(ty)
    }

    fn parse_optional_initializer(&mut self) -> Result<Option<Expression>, FrontendError> {
        if !self.at(&TokenKind::Assign) {
            return Ok(None);
        }
        self.advance();
        self.parse_expression().map(Some)
    }

    fn named_type_starts_declaration(&self) -> bool {
        if !matches!(self.current().kind, TokenKind::Identifier(_)) {
            return false;
        }
        let mut position = self.position + 1;
        while matches!(
            self.tokens.get(position).map(|token| &token.kind),
            Some(TokenKind::LeftBracket)
        ) {
            position += 1;
            if !matches!(
                self.tokens.get(position).map(|token| &token.kind),
                Some(TokenKind::Number(_) | TokenKind::Identifier(_))
            ) {
                return false;
            }
            position += 1;
            if !matches!(
                self.tokens.get(position).map(|token| &token.kind),
                Some(TokenKind::RightBracket)
            ) {
                return false;
            }
            position += 1;
        }
        matches!(
            self.tokens.get(position).map(|token| &token.kind),
            Some(TokenKind::Identifier(_))
        )
    }

    fn parse_name(&mut self, message: &str) -> Result<Name, FrontendError> {
        let token = self.current().clone();
        if let TokenKind::Identifier(text) = token.kind {
            self.advance();
            Ok(Name {
                text,
                offset: token.offset,
                context: NameContext::CallSite,
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

    fn peek_kind(&self, distance: usize) -> Option<&TokenKind> {
        self.tokens
            .get(self.position + distance)
            .map(|token| &token.kind)
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
    fn parses_version_one_surface_syntax() {
        let program = parse_source(
            r#"
            const cell N = 2;
            enum Kind { Empty, Full = 9 }
            struct Item { Kind kind; cell[2] bytes; }
            macro emit(value) { output(value); }
            Item[N] id(Item[N] value) { return value; }
            void main() {
                cell[] text = "a\0";
                Item[2] items;
                items[input()].kind = Kind::Full;
                emit!(len(text));
                items[0] = items[0].id();
            }
            "#,
        )
        .unwrap();
        assert_eq!(program.items.len(), 6);
    }

    #[test]
    fn array_dimensions_are_stored_outermost_first() {
        let program = parse_source("cell[8][255] pages; void main() {}").unwrap();
        let TopLevelItem::Global(global) = &program.items[0] else {
            panic!("expected global")
        };
        let Type::Array { length, element } = &global.ty else {
            panic!("expected outer array")
        };
        assert!(matches!(length, ArrayLength::Literal { value: 8, .. }));
        assert!(matches!(
            element.as_ref(),
            Type::Array {
                length: ArrayLength::Literal { value: 255, .. },
                ..
            }
        ));
    }

    #[test]
    fn accepts_zero_and_256_array_lengths_but_not_257() {
        parse_source("void main() { cell[0] empty; cell[256] full; }").unwrap();
        assert!(
            parse_source("void main() { cell[257] bad; }")
                .unwrap_err()
                .message()
                .contains("between 0 and 256")
        );
    }
}
