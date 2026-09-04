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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CallSite {
    caller: FunctionId,
    callee: FunctionId,
    offset: usize,
}

fn collect_callsite_from_projection(visited: &mut Vec<bool>, calls: &mut Vec<CallSite>, hir: &HirProgram, caller: FunctionId, proj: &Projection) {
    match proj {
        Projection::Field { cell_offset: _ } => {},
        Projection::Index { index, length: _, element_cells: _ } => {
            match index {
                ArrayIndex::Constant(_) => {},
                ArrayIndex::Dynamic(hir_expression) => {
                    collect_callsite_from_expr(visited, calls, hir, caller, hir_expression);
                },
            }
        },
    }
}

fn collect_callsite_from_expr(visited: &mut Vec<bool>, calls: &mut Vec<CallSite>, hir: &HirProgram, caller: FunctionId, expr: &HirExpression) {
    match &expr.kind {
        HirExpressionKind::Literal(_) => {},
        HirExpressionKind::EnumVariant(_) => {},
        HirExpressionKind::StringLiteral(_) => {},
        HirExpressionKind::Place(hir_place) => {
            for p in hir_place.projections.iter() {
                collect_callsite_from_projection(visited, calls, hir, caller, p);
            }
        },
        HirExpressionKind::Project { base, projections } => {
            collect_callsite_from_expr(visited, calls, hir, caller, &base);
            for p in projections.iter() {
                collect_callsite_from_projection(visited, calls, hir, caller, p);
            }
        }
        HirExpressionKind::Input => {},
        HirExpressionKind::Unary { operator: _, operand } => collect_callsite_from_expr(visited, calls, hir, caller, &operand),
        HirExpressionKind::Binary { operator: _, left, right } => {
            collect_callsite_from_expr(visited, calls, hir, caller, &left);
            collect_callsite_from_expr(visited, calls, hir, caller, &right);
        },
        HirExpressionKind::Call { function, arguments } => {
            let callsite = CallSite { caller, callee: *function, offset: expr.offset };

            calls.push(callsite);
            for arg in arguments {
                collect_callsite_from_expr(visited, calls, hir, caller, &arg);
            }
            collect_callsite_from_function(visited, calls, hir, *function);
        },
    }

}

fn collect_callsite_from_stmt(visited: &mut Vec<bool>, calls: &mut Vec<CallSite>, hir: &HirProgram, caller: FunctionId, stmt: &HirStatement) {
    match &stmt.kind {
        HirStatementKind::Empty => {},
        HirStatementKind::Block(hir_statements) => {
            for s in hir_statements.iter() {
                collect_callsite_from_stmt(visited, calls, hir, caller, &s);
            }
        }
        HirStatementKind::Declaration { local: _, initializer } => {
            let Some(expr) = initializer else {
                return;
            };
            collect_callsite_from_expr(visited, calls, hir, caller, &expr);
        },
        HirStatementKind::Assignment { value, target, operator: _ } => {
            collect_callsite_from_expr(visited, calls, hir, caller, &value);
            for p in target.projections.iter() {
                collect_callsite_from_projection(visited, calls, hir, caller, p);
            }
        },
        HirStatementKind::Output(hir_expression) => {
            collect_callsite_from_expr(visited, calls, hir, caller, &hir_expression);
        },
        HirStatementKind::Call { function, arguments } => {
            let callsite = CallSite { caller, callee: function.clone(), offset: stmt.offset };

            calls.push(callsite);
            for arg in arguments {
                collect_callsite_from_expr(visited, calls, hir, caller, &arg);
            }
            collect_callsite_from_function(visited, calls, hir, function.clone());
        },
        HirStatementKind::Abort => {},
        HirStatementKind::Return(hir_expression) => {
            let Some(expr) = hir_expression else {
                return;
            };
            collect_callsite_from_expr(visited, calls, hir, caller, &expr);
        },
        HirStatementKind::If { condition, then_branch, else_branch } => {
            collect_callsite_from_expr(visited, calls, hir, caller, &condition);
            collect_callsite_from_stmt(visited, calls, hir, caller, &then_branch);
            let Some(else_stmt) = else_branch else {
                return;
            };
            collect_callsite_from_stmt(visited, calls, hir, caller, &else_stmt);
        },
        HirStatementKind::While { condition, body } => {
            collect_callsite_from_expr(visited, calls, hir, caller, condition);
            collect_callsite_from_stmt(visited, calls, hir, caller, body);
        },
    }
}

fn collect_callsite_from_function(visited: &mut Vec<bool>, calls: &mut Vec<CallSite>, hir: &HirProgram, caller: FunctionId) {
    let idx = caller.index();
    if visited[idx] {
        return;
    }
    visited[idx] = true;
    let func = &hir.functions[idx];
    let stmt = &func.body;
    collect_callsite_from_stmt(visited, calls, hir, caller, stmt);
}

fn collect_callsite_from_globals(visited: &mut Vec<bool>, calls: &mut Vec<CallSite>, hir: &HirProgram) {
    for g in hir.globals.iter() {
        let Some(initializer) = &g.initializer else {
            continue;
        };
        collect_callsite_from_expr(visited, calls, hir, hir.entry, initializer);
    }
}

#[derive(Debug, Clone)]
struct CallGraph {
    calls: Vec<CallSite>,
    incoming: Vec<Vec<usize>>,
    outgoing: Vec<Vec<usize>>,
    reachable: Vec<bool>,
}

impl CallGraph {
    fn build(hir: &HirProgram) -> Self {
        let mut calls = Vec::new();
        let mut visited = vec![false; hir.functions.len()];
        collect_callsite_from_function(&mut visited, &mut calls, hir, hir.entry);
        collect_callsite_from_globals(&mut visited, &mut calls, hir);
        for call in calls.iter() {
            // eprintln!("{} {}", &hir.functions[call.caller.index()].name, &hir.functions[call.callee.index()].name);
        }

        let mut incoming = vec![vec![]; hir.functions.len()];
        let mut outgoing = vec![vec![]; hir.functions.len()];

        for (site_idx, call) in calls.iter().enumerate() {
            incoming[call.callee.index()].push(site_idx);
            outgoing[call.caller.index()].push(site_idx);
        }

        CallGraph { calls, incoming, outgoing, reachable: visited }
    }
}

pub(crate) fn optimize_hir(hir: &HirProgram) -> HirProgram {
    let graph = CallGraph::build(hir);

    // eprintln!("{:?}", graph);


    // new_hir
    hir.clone()
}

#[cfg(test)]
mod test {
    use super::CallGraph;
    use super::HirProgram;

    fn compile(source: &str) -> HirProgram {
        let tokens = crate::lexer::lex(source).unwrap();
        let ast = crate::parser::parse(tokens).unwrap();
        let ast = crate::macro_expansion::expand(ast).unwrap();
        return crate::semantic::analyze(&ast).unwrap();
    }

    #[test]
    fn check_call() {
        let s = compile(r"
            cell glob = fn10();

            void main() { // fn0
                if (fn1()) {};
                {
                    fn2();
                }
                fn5();
                if (0) {} else { fn7(); }
                cell[3] data;
                cell tmp = data[fn9()];
            }
            cell fn1() {
                return 0;
            }
            void fn2() {}
            void fn3() {
                fn4();
            }
            void fn4() {
                fn3();
            }
            void fn5() {
                fn5();
                fn5();
            }
            void fn6() {
                fn5();
            }
            void fn7() {
                fn8();
            }
            void fn8() {
                fn7();
            }
            cell fn9() {
                return 1;
            }
            cell fn10() {
                return 2;
            }");

        let g = CallGraph::build(&s);

        // eprintln!("{:?}", g);

        assert!(g.reachable[0]);
        assert!(g.reachable[1]);
        assert!(g.reachable[2]);
        assert!(!g.reachable[3]);
        assert!(!g.reachable[4]);
        assert!(g.reachable[5]);
        assert!(!g.reachable[6]);
        assert!(g.reachable[7]);
        assert!(g.reachable[8]);
        assert!(g.reachable[9]);
        assert_eq!(g.outgoing[5].len(), 2);
    }
}
