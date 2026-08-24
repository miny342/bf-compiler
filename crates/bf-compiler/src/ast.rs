#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AstProgram {
    pub(crate) items: Vec<TopLevelItem>,
    pub(crate) eof_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Type {
    Cell,
    Named(Name),
    Array {
        element: Box<Type>,
        length: ArrayLength,
    },
    /// `cell[]`; valid only on a directly string-initialized declaration.
    InferredCellArray,
    Void,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArrayLength {
    Literal { value: usize, offset: usize },
    Constant(Name),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TopLevelItem {
    Enum(EnumDefinition),
    Struct(StructDefinition),
    Constant(ConstantDefinition),
    Macro(MacroDefinition),
    Global(Global),
    Function(Function),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumDefinition {
    pub(crate) name: Name,
    pub(crate) variants: Vec<EnumVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumVariant {
    pub(crate) name: Name,
    pub(crate) discriminant: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructDefinition {
    pub(crate) name: Name,
    pub(crate) fields: Vec<StructField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructField {
    pub(crate) ty: Type,
    pub(crate) name: Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConstantDefinition {
    pub(crate) name: Name,
    pub(crate) initializer: Expression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MacroDefinition {
    pub(crate) name: Name,
    pub(crate) parameters: Vec<Name>,
    pub(crate) body: Statement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Global {
    pub(crate) ty: Type,
    pub(crate) name: Name,
    pub(crate) initializer: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Function {
    pub(crate) return_type: Type,
    pub(crate) name: Name,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) body: Statement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parameter {
    pub(crate) ty: Type,
    pub(crate) name: Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Name {
    pub(crate) text: String,
    pub(crate) offset: usize,
    /// Hygiene context assigned after parsing. Parser-produced names are at
    /// the call site; macro expansion distinguishes definition-site free
    /// names from fresh macro-local bindings.
    pub(crate) context: NameContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NameContext {
    CallSite,
    DefinitionSite,
    Synthetic(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Statement {
    pub(crate) kind: StatementKind,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatementKind {
    Empty,
    Block {
        statements: Vec<Statement>,
        closing_offset: usize,
    },
    Declaration {
        ty: Type,
        name: Name,
        initializer: Option<Expression>,
    },
    Assignment {
        target: Expression,
        operator: AssignmentOperator,
        value: Expression,
    },
    Output(Expression),
    Call(Expression),
    MacroInvocation {
        name: Name,
        arguments: Vec<Expression>,
    },
    Abort,
    Return(Option<Expression>),
    If {
        condition: Expression,
        then_branch: Box<Statement>,
        else_branch: Option<Box<Statement>>,
    },
    While {
        condition: Expression,
        body: Box<Statement>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssignmentOperator {
    Set,
    Add,
    Subtract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Expression {
    pub(crate) kind: ExpressionKind,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExpressionKind {
    Literal(u8),
    StringLiteral(Vec<u8>),
    Name(Name),
    EnumVariant {
        enum_name: Name,
        variant: Name,
    },
    Input,
    Call {
        name: Name,
        arguments: Vec<Expression>,
    },
    MethodCall {
        receiver: Box<Expression>,
        name: Name,
        arguments: Vec<Expression>,
    },
    Field {
        base: Box<Expression>,
        field: Name,
    },
    Index {
        base: Box<Expression>,
        index: Box<Expression>,
    },
    Len(Box<Expression>),
    Unary {
        operator: UnaryOperator,
        operand: Box<Expression>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
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
