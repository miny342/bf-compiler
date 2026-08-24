//! Typed, name-resolved source IR.
//!
//! This IR deliberately stops before frame layout. [`LocalId`] values identify
//! logical storage within one function; a later lowering pass decides where
//! those values live in an activation frame.

/// A scalar source-language type supported by the function milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Type {
    Cell,
    Void,
}

/// The identity of a function within a [`HirProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct FunctionId(usize);

impl FunctionId {
    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

/// The identity of a parameter or local variable within one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct LocalId(usize);

impl LocalId {
    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

/// A fully resolved collection of source functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirProgram {
    /// The `void main()` function selected by the frontend.
    pub(crate) entry: FunctionId,
    /// Functions in ID order: a function's ID indexes this vector.
    pub(crate) functions: Vec<HirFunction>,
}

/// The scalar portion of a function's source-level signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionSignature {
    pub(crate) return_type: Type,
    pub(crate) parameter_types: Vec<Type>,
}

/// A parameter and the logical local initialized by its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirParameter {
    pub(crate) local: LocalId,
    /// Byte offset of the parameter name, retained for diagnostics.
    pub(crate) offset: usize,
}

/// One typed, name-resolved function body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirFunction {
    pub(crate) id: FunctionId,
    /// Retained for diagnostics and debug output; calls use `FunctionId`.
    pub(crate) name: String,
    /// Byte offset of the function name.
    pub(crate) offset: usize,
    pub(crate) signature: FunctionSignature,
    pub(crate) parameters: Vec<HirParameter>,
    /// Number of logical locals, including parameters.
    ///
    /// Every `LocalId` in this function must be less than `local_count`.
    pub(crate) local_count: usize,
    pub(crate) body: HirStatement,
}

/// A statement paired with the byte offset of its leading source token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirStatement {
    pub(crate) kind: HirStatementKind,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HirStatementKind {
    Empty,
    Block(Vec<HirStatement>),
    Declaration {
        local: LocalId,
        initializer: Option<HirExpression>,
    },
    Assignment {
        local: LocalId,
        operator: AssignmentOperator,
        value: HirExpression,
    },
    Output(HirExpression),
    /// A resolved call whose result type is `void`.
    Call {
        function: FunctionId,
        arguments: Vec<HirExpression>,
    },
    Return(Option<HirExpression>),
    If {
        condition: HirExpression,
        then_branch: Box<HirStatement>,
        else_branch: Option<Box<HirStatement>>,
    },
    While {
        condition: HirExpression,
        body: Box<HirStatement>,
    },
}

/// A typed expression paired with the byte offset of its leading source token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirExpression {
    pub(crate) kind: HirExpressionKind,
    pub(crate) ty: Type,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HirExpressionKind {
    Literal(u8),
    Local(LocalId),
    Input,
    Unary {
        operator: UnaryOperator,
        operand: Box<HirExpression>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<HirExpression>,
        right: Box<HirExpression>,
    },
    /// A resolved call used as a value expression.
    Call {
        function: FunctionId,
        arguments: Vec<HirExpression>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssignmentOperator {
    Set,
    Add,
    Subtract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOperator {
    Plus,
    Negate,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryOperator {
    Add,
    Subtract,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    LogicalAnd,
    LogicalOr,
}

/// Evaluate an expression whose result is known without observing runtime
/// state. Logical operators retain their source short-circuit behavior, so a
/// runtime-dependent right operand need not prevent a constant result when it
/// is unreachable.
pub(crate) fn constant_cell_value(expression: &HirExpression) -> Option<u8> {
    if expression.ty != Type::Cell {
        return None;
    }

    match &expression.kind {
        HirExpressionKind::Literal(value) => Some(*value),
        HirExpressionKind::Local(_) | HirExpressionKind::Input | HirExpressionKind::Call { .. } => {
            None
        }
        HirExpressionKind::Unary { operator, operand } => {
            let operand = constant_cell_value(operand)?;
            Some(match operator {
                UnaryOperator::Plus => operand,
                UnaryOperator::Negate => 0_u8.wrapping_sub(operand),
                UnaryOperator::Not => u8::from(operand == 0),
            })
        }
        HirExpressionKind::Binary {
            operator,
            left,
            right,
        } => {
            let left = constant_cell_value(left)?;
            match operator {
                BinaryOperator::LogicalAnd if left == 0 => Some(0),
                BinaryOperator::LogicalOr if left != 0 => Some(1),
                BinaryOperator::LogicalAnd | BinaryOperator::LogicalOr => {
                    Some(u8::from(constant_cell_value(right)? != 0))
                }
                _ => {
                    let right = constant_cell_value(right)?;
                    Some(match operator {
                        BinaryOperator::Add => left.wrapping_add(right),
                        BinaryOperator::Subtract => left.wrapping_sub(right),
                        BinaryOperator::Less => u8::from(left < right),
                        BinaryOperator::LessEqual => u8::from(left <= right),
                        BinaryOperator::Greater => u8::from(left > right),
                        BinaryOperator::GreaterEqual => u8::from(left >= right),
                        BinaryOperator::Equal => u8::from(left == right),
                        BinaryOperator::NotEqual => u8::from(left != right),
                        BinaryOperator::LogicalAnd | BinaryOperator::LogicalOr => unreachable!(),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_preserve_their_logical_indices() {
        assert_eq!(FunctionId::new(3).index(), 3);
        assert_eq!(LocalId::new(7).index(), 7);
    }

    #[test]
    fn call_keeps_resolved_target_type_and_offset() {
        let expression = HirExpression {
            kind: HirExpressionKind::Call {
                function: FunctionId::new(1),
                arguments: vec![],
            },
            ty: Type::Cell,
            offset: 42,
        };

        assert_eq!(expression.ty, Type::Cell);
        assert_eq!(expression.offset, 42);
        let HirExpressionKind::Call { function, .. } = expression.kind else {
            panic!("expected a call");
        };
        assert_eq!(function, FunctionId::new(1));
    }

    #[test]
    fn constant_evaluation_wraps_and_short_circuits() {
        let expression = |kind| HirExpression {
            kind,
            ty: Type::Cell,
            offset: 0,
        };
        let literal = |value| expression(HirExpressionKind::Literal(value));

        let wrapped = expression(HirExpressionKind::Binary {
            operator: BinaryOperator::Add,
            left: Box::new(literal(255)),
            right: Box::new(literal(1)),
        });
        assert_eq!(constant_cell_value(&wrapped), Some(0));

        let unreachable_call = expression(HirExpressionKind::Call {
            function: FunctionId::new(0),
            arguments: vec![],
        });
        let short_circuit = expression(HirExpressionKind::Binary {
            operator: BinaryOperator::LogicalOr,
            left: Box::new(literal(1)),
            right: Box::new(unreachable_call),
        });
        assert_eq!(constant_cell_value(&short_circuit), Some(1));
    }
}
