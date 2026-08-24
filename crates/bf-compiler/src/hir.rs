//! Typed, name-resolved source IR.
//!
//! This layer preserves aggregate identity and evaluation order. Physical
//! global addresses, frame slots, array portals, and aggregate outboxes are
//! assigned by later lowering passes.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Type {
    Cell,
    Array(usize),
    Void,
}

impl Type {
    pub(crate) const fn is_value(self) -> bool {
        !matches!(self, Self::Void)
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct GlobalId(usize);

impl GlobalId {
    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }
    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum VariableRef {
    Global(GlobalId),
    Local(LocalId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirProgram {
    pub(crate) entry: FunctionId,
    /// Globals in declaration order. Initializers execute in this order.
    pub(crate) globals: Vec<HirGlobal>,
    /// Functions in ID order.
    pub(crate) functions: Vec<HirFunction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirGlobal {
    pub(crate) id: GlobalId,
    pub(crate) name: String,
    pub(crate) offset: usize,
    pub(crate) ty: Type,
    pub(crate) initializer: Option<HirExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionSignature {
    pub(crate) return_type: Type,
    pub(crate) parameter_types: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirParameter {
    pub(crate) local: LocalId,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirLocal {
    pub(crate) id: LocalId,
    pub(crate) name: String,
    pub(crate) offset: usize,
    pub(crate) ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirFunction {
    pub(crate) id: FunctionId,
    pub(crate) name: String,
    pub(crate) offset: usize,
    pub(crate) signature: FunctionSignature,
    pub(crate) parameters: Vec<HirParameter>,
    /// Parameters and declarations in logical-ID order.
    pub(crate) locals: Vec<HirLocal>,
    pub(crate) body: HirStatement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirStatement {
    pub(crate) kind: HirStatementKind,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HirStatementKind {
    Empty,
    Block(Vec<HirStatement>),
    /// Initialize a local at the declaration's execution point.
    Declaration {
        local: LocalId,
        initializer: Option<HirExpression>,
    },
    /// Evaluate `value` completely, then evaluate a dynamic index in `target`,
    /// then perform the update. Both expressions are evaluated exactly once.
    Assignment {
        value: HirExpression,
        target: HirPlace,
        operator: AssignmentOperator,
    },
    Output(HirExpression),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HirPlace {
    Variable {
        variable: VariableRef,
        ty: Type,
    },
    ArrayElement {
        array: VariableRef,
        length: usize,
        index: ArrayIndex,
    },
}

impl HirPlace {
    pub(crate) const fn ty(&self) -> Type {
        match self {
            Self::Variable { ty, .. } => *ty,
            Self::ArrayElement { .. } => Type::Cell,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArrayIndex {
    Constant(u8),
    Dynamic(Box<HirExpression>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirExpression {
    pub(crate) kind: HirExpressionKind,
    pub(crate) ty: Type,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HirExpressionKind {
    Literal(u8),
    Variable(VariableRef),
    ArrayElement {
        array: VariableRef,
        length: usize,
        index: ArrayIndex,
    },
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

/// Evaluate a cell expression without observing runtime state.
pub(crate) fn constant_cell_value(expression: &HirExpression) -> Option<u8> {
    if expression.ty != Type::Cell {
        return None;
    }

    match &expression.kind {
        HirExpressionKind::Literal(value) => Some(*value),
        HirExpressionKind::Variable(_)
        | HirExpressionKind::ArrayElement { .. }
        | HirExpressionKind::Input
        | HirExpressionKind::Call { .. } => None,
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

    fn expression(kind: HirExpressionKind, ty: Type) -> HirExpression {
        HirExpression {
            kind,
            ty,
            offset: 0,
        }
    }

    #[test]
    fn ids_preserve_their_logical_indices() {
        assert_eq!(FunctionId::new(3).index(), 3);
        assert_eq!(GlobalId::new(5).index(), 5);
        assert_eq!(LocalId::new(7).index(), 7);
    }

    #[test]
    fn aggregate_expressions_retain_identity_and_type() {
        let value = expression(
            HirExpressionKind::Variable(VariableRef::Global(GlobalId::new(2))),
            Type::Array(16),
        );
        assert_eq!(value.ty, Type::Array(16));
    }

    #[test]
    fn constant_evaluation_wraps_and_short_circuits() {
        let literal = |value| expression(HirExpressionKind::Literal(value), Type::Cell);
        let wrapped = expression(
            HirExpressionKind::Binary {
                operator: BinaryOperator::Add,
                left: Box::new(literal(255)),
                right: Box::new(literal(1)),
            },
            Type::Cell,
        );
        assert_eq!(constant_cell_value(&wrapped), Some(0));

        let unreachable_call = expression(
            HirExpressionKind::Call {
                function: FunctionId::new(0),
                arguments: vec![],
            },
            Type::Cell,
        );
        let short_circuit = expression(
            HirExpressionKind::Binary {
                operator: BinaryOperator::LogicalOr,
                left: Box::new(literal(1)),
                right: Box::new(unreachable_call),
            },
            Type::Cell,
        );
        assert_eq!(constant_cell_value(&short_circuit), Some(1));
    }
}
