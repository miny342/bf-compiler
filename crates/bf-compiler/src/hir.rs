//! Typed, name-resolved source IR.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TypeId(usize);

impl TypeId {
    pub(crate) const CELL: Self = Self(0);
    pub(crate) const VOID: Self = Self(1);

    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeTable {
    definitions: Vec<TypeDefinition>,
}

impl TypeTable {
    pub(crate) fn new() -> Self {
        Self {
            definitions: vec![
                TypeDefinition {
                    name: "cell".into(),
                    kind: TypeKind::Cell,
                    cells: 1,
                },
                TypeDefinition {
                    name: "void".into(),
                    kind: TypeKind::Void,
                    cells: 0,
                },
            ],
        }
    }

    pub(crate) fn push(&mut self, definition: TypeDefinition) -> TypeId {
        let id = TypeId::new(self.definitions.len());
        self.definitions.push(definition);
        id
    }

    pub(crate) fn get(&self, ty: TypeId) -> &TypeDefinition {
        &self.definitions[ty.index()]
    }

    pub(crate) fn get_mut(&mut self, ty: TypeId) -> &mut TypeDefinition {
        &mut self.definitions[ty.index()]
    }

    pub(crate) fn cells(&self, ty: TypeId) -> usize {
        self.get(ty).cells
    }

    pub(crate) fn kind(&self, ty: TypeId) -> &TypeKind {
        &self.get(ty).kind
    }

    pub(crate) fn name(&self, ty: TypeId) -> &str {
        &self.get(ty).name
    }

    pub(crate) fn is_scalar(&self, ty: TypeId) -> bool {
        matches!(self.kind(ty), TypeKind::Cell | TypeKind::Enum { .. })
    }

    pub(crate) fn is_aggregate(&self, ty: TypeId) -> bool {
        matches!(
            self.kind(ty),
            TypeKind::Struct { .. } | TypeKind::Array { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeDefinition {
    pub(crate) name: String,
    pub(crate) kind: TypeKind,
    pub(crate) cells: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypeKind {
    Cell,
    Void,
    Enum { variants: Vec<EnumVariant> },
    Struct { fields: Vec<Field> },
    Array { element: TypeId, length: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumVariant {
    pub(crate) name: String,
    pub(crate) value: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    pub(crate) name: String,
    pub(crate) ty: TypeId,
    pub(crate) cell_offset: usize,
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
    pub(crate) types: TypeTable,
    /// Combined-source file ranges used to restore local profile offsets.
    pub(crate) source_files: Vec<HirSourceFile>,
    /// Globals in declaration order. Initializers execute in this order.
    pub(crate) globals: Vec<HirGlobal>,
    /// Functions in ID order.
    pub(crate) functions: Vec<HirFunction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirSourceFile {
    pub(crate) id: u32,
    pub(crate) path: String,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirGlobal {
    pub(crate) id: GlobalId,
    pub(crate) name: String,
    pub(crate) offset: usize,
    pub(crate) ty: TypeId,
    pub(crate) initializer: Option<HirExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionSignature {
    pub(crate) return_type: TypeId,
    pub(crate) parameter_types: Vec<TypeId>,
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
    pub(crate) ty: TypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirFunction {
    pub(crate) id: FunctionId,
    pub(crate) name: String,
    pub(crate) offset: usize,
    pub(crate) signature: FunctionSignature,
    pub(crate) parameters: Vec<HirParameter>,
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
    Declaration {
        local: LocalId,
        initializer: Option<HirExpression>,
    },
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
    Abort,
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
pub(crate) struct HirPlace {
    pub(crate) root: VariableRef,
    pub(crate) projections: Vec<Projection>,
    pub(crate) ty: TypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Projection {
    Field {
        cell_offset: usize,
    },
    Index {
        index: ArrayIndex,
        length: usize,
        element_cells: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArrayIndex {
    Constant(u8),
    Dynamic(Box<HirExpression>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HirExpression {
    pub(crate) kind: HirExpressionKind,
    pub(crate) ty: TypeId,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HirExpressionKind {
    Literal(u8),
    EnumVariant(u8),
    StringLiteral(Vec<u8>),
    Place(HirPlace),
    /// Projection from a non-place aggregate value. `base` is evaluated once.
    Project {
        base: Box<HirExpression>,
        projections: Vec<Projection>,
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
    if expression.ty != TypeId::CELL {
        return None;
    }
    match &expression.kind {
        HirExpressionKind::Literal(value) => Some(*value),
        HirExpressionKind::EnumVariant(_)
        | HirExpressionKind::StringLiteral(_)
        | HirExpressionKind::Place(_)
        | HirExpressionKind::Project { .. }
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
