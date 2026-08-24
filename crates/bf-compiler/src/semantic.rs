//! Source-level declaration collection, layout, name resolution, and type checking.

use std::collections::{HashMap, HashSet};

use crate::ast;
use crate::ast::NameContext;
use crate::frontend::FrontendError;
use crate::hir::{
    self, ArrayIndex, Field, FunctionId, FunctionSignature, GlobalId, HirExpression,
    HirExpressionKind, HirFunction, HirGlobal, HirLocal, HirParameter, HirPlace, HirProgram,
    HirStatement, HirStatementKind, LocalId, Projection, TypeDefinition, TypeId, TypeKind,
    TypeTable, VariableRef,
};

pub(crate) fn analyze(program: &ast::AstProgram) -> Result<HirProgram, FrontendError> {
    SemanticBuilder::new(program)?.build()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopSymbol {
    Type(TypeId),
    Constant(usize),
    Macro,
    Function(FunctionId),
    Global(GlobalId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredFunction {
    id: FunctionId,
    signature: FunctionSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisteredGlobal {
    id: GlobalId,
    ty: TypeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionState {
    Unvisited,
    Visiting,
    Done,
}

struct SemanticBuilder<'a> {
    program: &'a ast::AstProgram,
    names: HashMap<String, TopSymbol>,
    types: TypeTable,
    nominal_states: Vec<ResolutionState>,
    constants: Vec<Option<u8>>,
    constant_states: Vec<ResolutionState>,
    functions: Vec<Option<RegisteredFunction>>,
    globals: Vec<Option<RegisteredGlobal>>,
    array_types: HashMap<(TypeId, usize), TypeId>,
}

impl<'a> SemanticBuilder<'a> {
    fn new(program: &'a ast::AstProgram) -> Result<Self, FrontendError> {
        let mut builder = Self {
            program,
            names: HashMap::new(),
            types: TypeTable::new(),
            nominal_states: vec![ResolutionState::Done, ResolutionState::Done],
            constants: Vec::new(),
            constant_states: Vec::new(),
            functions: Vec::new(),
            globals: Vec::new(),
            array_types: HashMap::new(),
        };

        for item in &program.items {
            let (name, symbol) = match item {
                ast::TopLevelItem::Enum(definition) => {
                    let id = builder.types.push(TypeDefinition {
                        name: definition.name.text.clone(),
                        kind: TypeKind::Enum { variants: vec![] },
                        cells: 1,
                    });
                    builder.nominal_states.push(ResolutionState::Unvisited);
                    (&definition.name, TopSymbol::Type(id))
                }
                ast::TopLevelItem::Struct(definition) => {
                    let id = builder.types.push(TypeDefinition {
                        name: definition.name.text.clone(),
                        kind: TypeKind::Struct { fields: vec![] },
                        cells: 0,
                    });
                    builder.nominal_states.push(ResolutionState::Unvisited);
                    (&definition.name, TopSymbol::Type(id))
                }
                ast::TopLevelItem::Constant(definition) => {
                    let index = builder.constants.len();
                    builder.constants.push(None);
                    builder.constant_states.push(ResolutionState::Unvisited);
                    (&definition.name, TopSymbol::Constant(index))
                }
                ast::TopLevelItem::Macro(definition) => (&definition.name, TopSymbol::Macro),
                ast::TopLevelItem::Function(function) => {
                    let id = FunctionId::new(builder.functions.len());
                    builder.functions.push(None);
                    (&function.name, TopSymbol::Function(id))
                }
                ast::TopLevelItem::Global(global) => {
                    let id = GlobalId::new(builder.globals.len());
                    builder.globals.push(None);
                    (&global.name, TopSymbol::Global(id))
                }
            };
            if builder.names.insert(name.text.clone(), symbol).is_some() {
                return Err(FrontendError::at(
                    name.offset,
                    format!("file-scope name {:?} is already defined", name.text),
                ));
            }
        }
        Ok(builder)
    }

    fn build(mut self) -> Result<HirProgram, FrontendError> {
        for index in 0..self.constants.len() {
            self.resolve_constant(index)?;
        }
        for index in 2..self.nominal_states.len() {
            self.resolve_nominal(TypeId::new(index))?;
        }
        // String literal types are always available without mutating the type
        // table during expression analysis.
        for length in 0..=256 {
            self.intern_array(TypeId::CELL, length, 0)?;
        }
        self.resolve_runtime_declarations()?;
        let items = self.program.items.clone();
        for item in &items {
            if let ast::TopLevelItem::Function(function) = item {
                self.resolve_statement_types(&function.body)?;
            }
        }

        let functions = self
            .functions
            .iter()
            .map(|function| function.clone().expect("all signatures resolved"))
            .collect::<Vec<_>>();
        let globals = self
            .globals
            .iter()
            .map(|global| global.expect("all global types resolved"))
            .collect::<Vec<_>>();
        let entry = self.validate_main(&functions)?;

        let context = FileContext {
            names: &self.names,
            types: &self.types,
            constants: &self.constants,
            functions: &functions,
            globals: &globals,
            array_types: &self.array_types,
        };

        let mut hir_globals = Vec::new();
        for item in &self.program.items {
            let ast::TopLevelItem::Global(global) = item else {
                continue;
            };
            let TopSymbol::Global(id) = context.symbol(&global.name.text) else {
                unreachable!()
            };
            let registered = globals[id.index()];
            let initializer = global
                .initializer
                .as_ref()
                .map(|value| {
                    let mut analyzer = FunctionAnalyzer::new(&context, TypeId::VOID);
                    let value = analyzer.analyze_expression(value)?;
                    require_type(&context, &value, registered.ty, "global initializer")?;
                    Ok(value)
                })
                .transpose()?;
            if matches!(global.ty, ast::Type::InferredCellArray) && initializer.is_none() {
                return Err(FrontendError::at(
                    global.name.offset,
                    "cell[] requires a direct string literal initializer",
                ));
            }
            hir_globals.push(HirGlobal {
                id,
                name: global.name.text.clone(),
                offset: global.name.offset,
                ty: registered.ty,
                initializer,
            });
        }

        let mut hir_functions = Vec::new();
        for item in &self.program.items {
            let ast::TopLevelItem::Function(function) = item else {
                continue;
            };
            let TopSymbol::Function(id) = context.symbol(&function.name.text) else {
                unreachable!()
            };
            let registered = &functions[id.index()];
            hir_functions.push(
                FunctionAnalyzer::new(&context, registered.signature.return_type)
                    .analyze(id, function)?,
            );
        }

        Ok(HirProgram {
            entry,
            types: self.types,
            globals: hir_globals,
            functions: hir_functions,
        })
    }

    fn resolve_constant(&mut self, index: usize) -> Result<u8, FrontendError> {
        match self.constant_states[index] {
            ResolutionState::Done => return Ok(self.constants[index].unwrap()),
            ResolutionState::Visiting => {
                let definition = self.constant_definition(index);
                return Err(FrontendError::at(
                    definition.name.offset,
                    "compile-time constant definitions form a cycle",
                ));
            }
            ResolutionState::Unvisited => {}
        }
        self.constant_states[index] = ResolutionState::Visiting;
        let definition = self.constant_definition(index).clone();
        let value = self.evaluate_constant_expression(&definition.initializer)?;
        self.constants[index] = Some(value);
        self.constant_states[index] = ResolutionState::Done;
        Ok(value)
    }

    fn constant_definition(&self, target: usize) -> &ast::ConstantDefinition {
        self.program
            .items
            .iter()
            .filter_map(|item| match item {
                ast::TopLevelItem::Constant(definition) => Some(definition),
                _ => None,
            })
            .nth(target)
            .unwrap()
    }

    fn evaluate_constant_expression(
        &mut self,
        expression: &ast::Expression,
    ) -> Result<u8, FrontendError> {
        match &expression.kind {
            ast::ExpressionKind::Literal(value) => Ok(*value),
            ast::ExpressionKind::Name(name) => match self.names.get(&name.text).copied() {
                Some(TopSymbol::Constant(index)) => self.resolve_constant(index),
                _ => Err(FrontendError::at(
                    name.offset,
                    format!("{:?} is not a const cell", name.text),
                )),
            },
            ast::ExpressionKind::Unary { operator, operand } => {
                let operand = self.evaluate_constant_expression(operand)?;
                Ok(match operator {
                    ast::UnaryOperator::Plus => operand,
                    ast::UnaryOperator::Negate => 0_u8.wrapping_sub(operand),
                    ast::UnaryOperator::Not => u8::from(operand == 0),
                })
            }
            ast::ExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.evaluate_constant_expression(left)?;
                match operator {
                    ast::BinaryOperator::LogicalAnd if left == 0 => Ok(0),
                    ast::BinaryOperator::LogicalOr if left != 0 => Ok(1),
                    _ => {
                        let right = self.evaluate_constant_expression(right)?;
                        Ok(evaluate_binary(*operator, left, right))
                    }
                }
            }
            ast::ExpressionKind::Len(operand) => {
                let ty = self.constant_operand_type(operand)?;
                let TypeKind::Array { length, .. } = self.types.kind(ty) else {
                    return Err(FrontendError::at(
                        expression.offset,
                        "len operand must have an array type",
                    ));
                };
                u8::try_from(*length).map_err(|_| {
                    FrontendError::at(
                        expression.offset,
                        "len result 256 cannot be represented as a runtime cell",
                    )
                })
            }
            _ => Err(FrontendError::at(
                expression.offset,
                "expected a compile-time cell constant expression",
            )),
        }
    }

    fn constant_operand_type(
        &mut self,
        expression: &ast::Expression,
    ) -> Result<TypeId, FrontendError> {
        match &expression.kind {
            ast::ExpressionKind::Literal(_) | ast::ExpressionKind::Input => Ok(TypeId::CELL),
            ast::ExpressionKind::Unary { operand, .. } => {
                if self.constant_operand_type(operand)? != TypeId::CELL {
                    return Err(FrontendError::at(
                        operand.offset,
                        "unary operator operand must have type cell",
                    ));
                }
                Ok(TypeId::CELL)
            }
            ast::ExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let left_ty = self.constant_operand_type(left)?;
                let right_ty = self.constant_operand_type(right)?;
                let valid = match operator {
                    ast::BinaryOperator::Equal | ast::BinaryOperator::NotEqual => {
                        left_ty == right_ty
                            && (left_ty == TypeId::CELL
                                || matches!(self.types.kind(left_ty), TypeKind::Enum { .. }))
                    }
                    ast::BinaryOperator::Add
                    | ast::BinaryOperator::Subtract
                    | ast::BinaryOperator::Less
                    | ast::BinaryOperator::LessEqual
                    | ast::BinaryOperator::Greater
                    | ast::BinaryOperator::GreaterEqual
                    | ast::BinaryOperator::LogicalAnd
                    | ast::BinaryOperator::LogicalOr => {
                        left_ty == TypeId::CELL && right_ty == TypeId::CELL
                    }
                };
                if !valid {
                    return Err(FrontendError::at(
                        expression.offset,
                        "binary operator operands have incompatible types",
                    ));
                }
                Ok(TypeId::CELL)
            }
            ast::ExpressionKind::Len(operand) => {
                let ty = self.constant_operand_type(operand)?;
                let TypeKind::Array { length, .. } = self.types.kind(ty) else {
                    return Err(FrontendError::at(
                        expression.offset,
                        "len operand must have an array type",
                    ));
                };
                if *length == 256 {
                    return Err(FrontendError::at(
                        expression.offset,
                        "len result 256 cannot be represented as a runtime cell",
                    ));
                }
                Ok(TypeId::CELL)
            }
            ast::ExpressionKind::StringLiteral(bytes) => {
                self.intern_array(TypeId::CELL, bytes.len(), expression.offset)
            }
            ast::ExpressionKind::Name(name) => match self.names.get(&name.text).copied() {
                Some(TopSymbol::Constant(_)) => Ok(TypeId::CELL),
                Some(TopSymbol::Global(id)) => {
                    let global = self
                        .program
                        .items
                        .iter()
                        .filter_map(|item| match item {
                            ast::TopLevelItem::Global(global) => Some(global),
                            _ => None,
                        })
                        .nth(id.index())
                        .unwrap()
                        .clone();
                    self.resolve_declaration_type(
                        &global.ty,
                        global.initializer.as_ref(),
                        global.name.offset,
                    )
                }
                _ => Err(FrontendError::at(
                    name.offset,
                    format!("{:?} is not a value", name.text),
                )),
            },
            ast::ExpressionKind::EnumVariant { enum_name, variant } => {
                let Some(TopSymbol::Type(ty)) = self.names.get(&enum_name.text).copied() else {
                    return Err(FrontendError::at(enum_name.offset, "unknown enum type"));
                };
                self.resolve_nominal(ty)?;
                let TypeKind::Enum { variants } = self.types.kind(ty) else {
                    return Err(FrontendError::at(enum_name.offset, "expected enum type"));
                };
                if !variants
                    .iter()
                    .any(|candidate| candidate.name == variant.text)
                {
                    return Err(FrontendError::at(variant.offset, "unknown enum variant"));
                }
                Ok(ty)
            }
            ast::ExpressionKind::Call { name, arguments } => {
                self.constant_call_type(name, arguments, None)
            }
            ast::ExpressionKind::MethodCall {
                receiver,
                name,
                arguments,
            } => self.constant_call_type(name, arguments, Some(receiver)),
            ast::ExpressionKind::Field { base, field } => {
                let base = self.constant_operand_type(base)?;
                let TypeKind::Struct { fields } = self.types.kind(base) else {
                    return Err(FrontendError::at(
                        field.offset,
                        "field base is not a struct",
                    ));
                };
                fields
                    .iter()
                    .find(|candidate| candidate.name == field.text)
                    .map(|candidate| candidate.ty)
                    .ok_or_else(|| FrontendError::at(field.offset, "unknown struct field"))
            }
            ast::ExpressionKind::Index { base, index } => {
                let base = self.constant_operand_type(base)?;
                if self.constant_operand_type(index)? != TypeId::CELL {
                    return Err(FrontendError::at(
                        index.offset,
                        "array index must have type cell",
                    ));
                }
                let TypeKind::Array { element, length } = *self.types.kind(base) else {
                    return Err(FrontendError::at(
                        expression.offset,
                        "indexed value is not an array",
                    ));
                };
                if let Some(value) = self.try_evaluate_constant_cell(index)?
                    && usize::from(value) >= length
                {
                    return Err(FrontendError::at(
                        index.offset,
                        format!(
                            "constant array index {value} is out of bounds for length {length}"
                        ),
                    ));
                }
                Ok(element)
            }
        }
    }

    /// Evaluate the compile-time subset of a cell expression when possible.
    ///
    /// `len` is intentionally type-only, so an index occurring below it may
    /// be a runtime expression.  We still diagnose an index that is provably
    /// constant and out of bounds, just as ordinary expression analysis does.
    fn try_evaluate_constant_cell(
        &mut self,
        expression: &ast::Expression,
    ) -> Result<Option<u8>, FrontendError> {
        match &expression.kind {
            ast::ExpressionKind::Literal(value) => Ok(Some(*value)),
            ast::ExpressionKind::Name(name) => match self.names.get(&name.text).copied() {
                Some(TopSymbol::Constant(index)) => self.resolve_constant(index).map(Some),
                _ => Ok(None),
            },
            ast::ExpressionKind::Unary { operator, operand } => {
                let Some(operand) = self.try_evaluate_constant_cell(operand)? else {
                    return Ok(None);
                };
                Ok(Some(match operator {
                    ast::UnaryOperator::Plus => operand,
                    ast::UnaryOperator::Negate => 0_u8.wrapping_sub(operand),
                    ast::UnaryOperator::Not => u8::from(operand == 0),
                }))
            }
            ast::ExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let Some(left) = self.try_evaluate_constant_cell(left)? else {
                    return Ok(None);
                };
                match operator {
                    ast::BinaryOperator::LogicalAnd if left == 0 => Ok(Some(0)),
                    ast::BinaryOperator::LogicalOr if left != 0 => Ok(Some(1)),
                    _ => Ok(self
                        .try_evaluate_constant_cell(right)?
                        .map(|right| evaluate_binary(*operator, left, right))),
                }
            }
            ast::ExpressionKind::Len(_) => self.evaluate_constant_expression(expression).map(Some),
            ast::ExpressionKind::StringLiteral(_)
            | ast::ExpressionKind::EnumVariant { .. }
            | ast::ExpressionKind::Input
            | ast::ExpressionKind::Call { .. }
            | ast::ExpressionKind::MethodCall { .. }
            | ast::ExpressionKind::Field { .. }
            | ast::ExpressionKind::Index { .. } => Ok(None),
        }
    }

    fn constant_call_type(
        &mut self,
        name: &ast::Name,
        arguments: &[ast::Expression],
        receiver: Option<&ast::Expression>,
    ) -> Result<TypeId, FrontendError> {
        let Some(TopSymbol::Function(id)) = self.names.get(&name.text).copied() else {
            return Err(FrontendError::at(name.offset, "unknown function"));
        };
        let function = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                ast::TopLevelItem::Function(function) => Some(function),
                _ => None,
            })
            .nth(id.index())
            .unwrap()
            .clone();
        let actual = arguments.len() + usize::from(receiver.is_some());
        if actual != function.parameters.len() {
            return Err(FrontendError::at(
                name.offset,
                "function argument count mismatch",
            ));
        }
        let mut actual_types = Vec::with_capacity(actual);
        if let Some(receiver) = receiver {
            actual_types.push(self.constant_operand_type(receiver)?);
        }
        for argument in arguments {
            actual_types.push(self.constant_operand_type(argument)?);
        }
        for (actual, parameter) in actual_types.iter().zip(&function.parameters) {
            let expected = self.resolve_type(&parameter.ty)?;
            if *actual != expected {
                return Err(FrontendError::at(
                    parameter.name.offset,
                    "function argument type mismatch",
                ));
            }
        }
        self.resolve_type(&function.return_type)
    }

    fn resolve_nominal(&mut self, ty: TypeId) -> Result<(), FrontendError> {
        match self.nominal_states[ty.index()] {
            ResolutionState::Done => return Ok(()),
            ResolutionState::Visiting => {
                return Err(FrontendError::at(
                    self.nominal_offset(ty),
                    format!("type {:?} recursively contains itself", self.types.name(ty)),
                ));
            }
            ResolutionState::Unvisited => {}
        }
        self.nominal_states[ty.index()] = ResolutionState::Visiting;
        let item = match self.nominal_item(ty) {
            NominalItem::Enum(definition) => NominalOwned::Enum(definition.clone()),
            NominalItem::Struct(definition) => NominalOwned::Struct(definition.clone()),
        };
        match item {
            NominalOwned::Enum(definition) => {
                let mut variants = Vec::new();
                let mut used_names = HashSet::new();
                let mut used_values = HashSet::new();
                let mut previous: Option<u8> = None;
                for variant in definition.variants {
                    if !used_names.insert(variant.name.text.clone()) {
                        return Err(FrontendError::at(
                            variant.name.offset,
                            format!("enum variant {:?} is already defined", variant.name.text),
                        ));
                    }
                    let value = if let Some(expression) = variant.discriminant {
                        self.evaluate_constant_expression(&expression)?
                    } else if let Some(previous) = previous {
                        previous.checked_add(1).ok_or_else(|| {
                            FrontendError::at(
                                variant.name.offset,
                                "implicit enum discriminant exceeds 255",
                            )
                        })?
                    } else {
                        0
                    };
                    if !used_values.insert(value) {
                        return Err(FrontendError::at(
                            variant.name.offset,
                            format!("enum discriminant {value} is duplicated"),
                        ));
                    }
                    previous = Some(value);
                    variants.push(hir::EnumVariant {
                        name: variant.name.text,
                        value,
                    });
                }
                if variants.is_empty() {
                    return Err(FrontendError::at(
                        definition.name.offset,
                        "enum must contain at least one variant",
                    ));
                }
                if variants.iter().filter(|variant| variant.value == 0).count() != 1 {
                    return Err(FrontendError::at(
                        definition.name.offset,
                        "enum must contain exactly one variant with discriminant 0",
                    ));
                }
                self.types.get_mut(ty).kind = TypeKind::Enum { variants };
                self.types.get_mut(ty).cells = 1;
            }
            NominalOwned::Struct(definition) => {
                if definition.fields.is_empty() {
                    return Err(FrontendError::at(
                        definition.name.offset,
                        "struct must contain at least one field",
                    ));
                }
                let mut fields = Vec::new();
                let mut names = HashSet::new();
                let mut cells = 0usize;
                for field in definition.fields {
                    if !names.insert(field.name.text.clone()) {
                        return Err(FrontendError::at(
                            field.name.offset,
                            format!("struct field {:?} is already defined", field.name.text),
                        ));
                    }
                    let field_ty = self.resolve_type(&field.ty)?;
                    let offset = cells;
                    cells = cells
                        .checked_add(self.types.cells(field_ty))
                        .ok_or_else(|| {
                            FrontendError::at(field.name.offset, "struct layout size overflows")
                        })?;
                    fields.push(Field {
                        name: field.name.text,
                        ty: field_ty,
                        cell_offset: offset,
                    });
                }
                self.types.get_mut(ty).kind = TypeKind::Struct { fields };
                self.types.get_mut(ty).cells = cells;
            }
        }
        self.nominal_states[ty.index()] = ResolutionState::Done;
        Ok(())
    }

    fn nominal_offset(&self, ty: TypeId) -> usize {
        match self.nominal_item(ty) {
            NominalItem::Enum(definition) => definition.name.offset,
            NominalItem::Struct(definition) => definition.name.offset,
        }
    }

    fn nominal_item(&self, ty: TypeId) -> NominalItem<'_> {
        let mut index = 2;
        for item in &self.program.items {
            match item {
                ast::TopLevelItem::Enum(definition) => {
                    if index == ty.index() {
                        return NominalItem::Enum(definition);
                    }
                    index += 1;
                }
                ast::TopLevelItem::Struct(definition) => {
                    if index == ty.index() {
                        return NominalItem::Struct(definition);
                    }
                    index += 1;
                }
                _ => {}
            }
        }
        unreachable!()
    }

    fn resolve_type(&mut self, ty: &ast::Type) -> Result<TypeId, FrontendError> {
        match ty {
            ast::Type::Cell => Ok(TypeId::CELL),
            ast::Type::Void => Ok(TypeId::VOID),
            ast::Type::InferredCellArray => Err(FrontendError::at(
                0,
                "cell[] is only valid on a directly string-initialized declaration",
            )),
            ast::Type::Named(name) => match self.names.get(&name.text).copied() {
                Some(TopSymbol::Type(id)) => {
                    self.resolve_nominal(id)?;
                    Ok(id)
                }
                _ => Err(FrontendError::at(
                    name.offset,
                    format!("unknown type {:?}", name.text),
                )),
            },
            ast::Type::Array { element, length } => {
                let element = self.resolve_type(element)?;
                let (length, offset) = match length {
                    ast::ArrayLength::Literal { value, offset } => (*value, *offset),
                    ast::ArrayLength::Constant(name) => {
                        let Some(TopSymbol::Constant(index)) = self.names.get(&name.text).copied()
                        else {
                            return Err(FrontendError::at(
                                name.offset,
                                "array length name must refer to a const cell",
                            ));
                        };
                        (usize::from(self.resolve_constant(index)?), name.offset)
                    }
                };
                self.intern_array(element, length, offset)
            }
        }
    }

    fn intern_array(
        &mut self,
        element: TypeId,
        length: usize,
        offset: usize,
    ) -> Result<TypeId, FrontendError> {
        if let Some(id) = self.array_types.get(&(element, length)) {
            return Ok(*id);
        }
        let cells = self
            .types
            .cells(element)
            .checked_mul(length)
            .ok_or_else(|| FrontendError::at(offset, "array layout size overflows"))?;
        let name = format!("{}[{length}]", self.types.name(element));
        let id = self.types.push(TypeDefinition {
            name,
            kind: TypeKind::Array { element, length },
            cells,
        });
        self.nominal_states.push(ResolutionState::Done);
        self.array_types.insert((element, length), id);
        Ok(id)
    }

    fn resolve_runtime_declarations(&mut self) -> Result<(), FrontendError> {
        let items = self.program.items.clone();
        for item in items {
            match item {
                ast::TopLevelItem::Function(function) => {
                    let TopSymbol::Function(id) = self.names[&function.name.text] else {
                        unreachable!()
                    };
                    let return_type = self.resolve_type(&function.return_type)?;
                    let parameter_types = function
                        .parameters
                        .iter()
                        .map(|parameter| self.resolve_type(&parameter.ty))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.functions[id.index()] = Some(RegisteredFunction {
                        id,
                        signature: FunctionSignature {
                            return_type,
                            parameter_types,
                        },
                    });
                }
                ast::TopLevelItem::Global(global) => {
                    let TopSymbol::Global(id) = self.names[&global.name.text] else {
                        unreachable!()
                    };
                    let ty = self.resolve_declaration_type(
                        &global.ty,
                        global.initializer.as_ref(),
                        global.name.offset,
                    )?;
                    self.globals[id.index()] = Some(RegisteredGlobal { id, ty });
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn resolve_statement_types(&mut self, statement: &ast::Statement) -> Result<(), FrontendError> {
        match &statement.kind {
            ast::StatementKind::Block { statements, .. } => {
                for statement in statements {
                    self.resolve_statement_types(statement)?;
                }
            }
            ast::StatementKind::Declaration {
                ty,
                name,
                initializer,
            } => {
                self.resolve_declaration_type(ty, initializer.as_ref(), name.offset)?;
            }
            ast::StatementKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.resolve_statement_types(then_branch)?;
                if let Some(else_branch) = else_branch {
                    self.resolve_statement_types(else_branch)?;
                }
            }
            ast::StatementKind::While { body, .. } => self.resolve_statement_types(body)?,
            _ => {}
        }
        Ok(())
    }

    fn resolve_declaration_type(
        &mut self,
        ty: &ast::Type,
        initializer: Option<&ast::Expression>,
        offset: usize,
    ) -> Result<TypeId, FrontendError> {
        if !matches!(ty, ast::Type::InferredCellArray) {
            return self.resolve_type(ty);
        }
        let Some(ast::Expression {
            kind: ast::ExpressionKind::StringLiteral(bytes),
            ..
        }) = initializer
        else {
            return Err(FrontendError::at(
                offset,
                "cell[] requires a direct string literal initializer",
            ));
        };
        self.intern_array(TypeId::CELL, bytes.len(), offset)
    }

    fn validate_main(&self, functions: &[RegisteredFunction]) -> Result<FunctionId, FrontendError> {
        let Some(TopSymbol::Function(id)) = self.names.get("main").copied() else {
            return Err(FrontendError::at(
                self.program.eof_offset,
                "program must define exactly one 'void main()' function",
            ));
        };
        let main = &functions[id.index()];
        if main.signature.return_type != TypeId::VOID {
            return Err(FrontendError::at(
                self.function_offset(id),
                "main must have return type 'void'",
            ));
        }
        if !main.signature.parameter_types.is_empty() {
            return Err(FrontendError::at(
                self.function_offset(id),
                "main must not have parameters",
            ));
        }
        Ok(id)
    }

    fn function_offset(&self, id: FunctionId) -> usize {
        self.program
            .items
            .iter()
            .filter_map(|item| match item {
                ast::TopLevelItem::Function(function) => Some(function.name.offset),
                _ => None,
            })
            .nth(id.index())
            .unwrap()
    }
}

#[derive(Clone)]
enum NominalItem<'a> {
    Enum(&'a ast::EnumDefinition),
    Struct(&'a ast::StructDefinition),
}

enum NominalOwned {
    Enum(ast::EnumDefinition),
    Struct(ast::StructDefinition),
}

struct FileContext<'a> {
    names: &'a HashMap<String, TopSymbol>,
    types: &'a TypeTable,
    constants: &'a [Option<u8>],
    functions: &'a [RegisteredFunction],
    globals: &'a [RegisteredGlobal],
    array_types: &'a HashMap<(TypeId, usize), TypeId>,
}

impl FileContext<'_> {
    fn symbol(&self, name: &str) -> TopSymbol {
        self.names[name]
    }

    fn cell_array(&self, length: usize) -> TypeId {
        self.array_types[&(TypeId::CELL, length)]
    }
}

struct FunctionAnalyzer<'a> {
    context: &'a FileContext<'a>,
    return_type: TypeId,
    scopes: Vec<HashMap<(String, NameContext), LocalId>>,
    locals: Vec<HirLocal>,
}

impl<'a> FunctionAnalyzer<'a> {
    fn new(context: &'a FileContext<'a>, return_type: TypeId) -> Self {
        Self {
            context,
            return_type,
            scopes: vec![HashMap::new()],
            locals: Vec::new(),
        }
    }

    fn analyze(
        mut self,
        id: FunctionId,
        function: &ast::Function,
    ) -> Result<HirFunction, FrontendError> {
        let registered = &self.context.functions[id.index()];
        let mut parameters = Vec::with_capacity(function.parameters.len());
        for (parameter, ty) in function
            .parameters
            .iter()
            .zip(&registered.signature.parameter_types)
        {
            let local = self.declare_local(&parameter.name, *ty)?;
            parameters.push(HirParameter {
                local,
                offset: parameter.name.offset,
            });
        }
        let (mut body, flow, closing_offset) = self.analyze_function_body(&function.body)?;
        if self.return_type != TypeId::VOID && flow == Flow::FallsThrough {
            return Err(FrontendError::at(
                closing_offset,
                format!(
                    "{} function {:?} does not return a value on every path",
                    self.context.types.name(self.return_type),
                    function.name.text
                ),
            ));
        }
        if self.return_type == TypeId::VOID && flow == Flow::FallsThrough {
            let HirStatementKind::Block(statements) = &mut body.kind else {
                unreachable!()
            };
            statements.push(HirStatement {
                kind: HirStatementKind::Return(None),
                offset: closing_offset,
            });
        }
        Ok(HirFunction {
            id,
            name: function.name.text.clone(),
            offset: function.name.offset,
            signature: registered.signature.clone(),
            parameters,
            locals: self.locals,
            body,
        })
    }

    fn analyze_function_body(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<(HirStatement, Flow, usize), FrontendError> {
        let ast::StatementKind::Block {
            statements,
            closing_offset,
        } = &statement.kind
        else {
            unreachable!("function bodies are blocks")
        };
        let (statements, flow) = self.analyze_statements(statements)?;
        Ok((
            HirStatement {
                kind: HirStatementKind::Block(statements),
                offset: statement.offset,
            },
            flow,
            *closing_offset,
        ))
    }

    fn analyze_statements(
        &mut self,
        statements: &[ast::Statement],
    ) -> Result<(Vec<HirStatement>, Flow), FrontendError> {
        let mut output = Vec::with_capacity(statements.len());
        let mut flow = Flow::FallsThrough;
        for statement in statements {
            let (statement, statement_flow) = self.analyze_statement(statement)?;
            if flow == Flow::FallsThrough {
                flow = statement_flow;
            }
            output.push(statement);
        }
        Ok((output, flow))
    }

    fn analyze_statement(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<(HirStatement, Flow), FrontendError> {
        let (kind, flow) = match &statement.kind {
            ast::StatementKind::Empty => (HirStatementKind::Empty, Flow::FallsThrough),
            ast::StatementKind::Block { statements, .. } => {
                self.scopes.push(HashMap::new());
                let result = self.analyze_statements(statements);
                self.scopes.pop();
                let (statements, flow) = result?;
                (HirStatementKind::Block(statements), flow)
            }
            ast::StatementKind::Declaration {
                ty,
                name,
                initializer,
            } => {
                if self
                    .scopes
                    .last()
                    .unwrap()
                    .contains_key(&(name.text.clone(), name.context))
                {
                    return Err(already_declared(name));
                }
                let declared_ty = self.resolve_local_type(ty, initializer.as_ref(), name.offset)?;
                let initializer = initializer
                    .as_ref()
                    .map(|value| {
                        let value = self.analyze_expression(value)?;
                        require_type(self.context, &value, declared_ty, "variable initializer")?;
                        Ok(value)
                    })
                    .transpose()?;
                let local = self.declare_local(name, declared_ty)?;
                (
                    HirStatementKind::Declaration { local, initializer },
                    Flow::FallsThrough,
                )
            }
            ast::StatementKind::Assignment {
                target,
                operator,
                value,
            } => {
                let value = self.analyze_expression(value)?;
                let target_expression = self.analyze_expression(target)?;
                let HirExpressionKind::Place(target) = target_expression.kind else {
                    return Err(FrontendError::at(
                        target.offset,
                        "assignment target must be a variable, field, or array element",
                    ));
                };
                require_type(self.context, &value, target.ty, "assignment")?;
                let operator = match operator {
                    ast::AssignmentOperator::Set => hir::AssignmentOperator::Set,
                    ast::AssignmentOperator::Add => hir::AssignmentOperator::Add,
                    ast::AssignmentOperator::Subtract => hir::AssignmentOperator::Subtract,
                };
                if operator != hir::AssignmentOperator::Set && target.ty != TypeId::CELL {
                    return Err(FrontendError::at(
                        statement.offset,
                        "compound assignment target and value must have type cell",
                    ));
                }
                (
                    HirStatementKind::Assignment {
                        value,
                        target,
                        operator,
                    },
                    Flow::FallsThrough,
                )
            }
            ast::StatementKind::Output(value) => {
                let value = self.analyze_expression(value)?;
                require_type(self.context, &value, TypeId::CELL, "output argument")?;
                (HirStatementKind::Output(value), Flow::FallsThrough)
            }
            ast::StatementKind::Call(expression) => {
                let expression = self.analyze_expression(expression)?;
                if expression.ty != TypeId::VOID {
                    return Err(FrontendError::at(
                        expression.offset,
                        "a value-returning function cannot be used as a statement",
                    ));
                }
                let HirExpressionKind::Call {
                    function,
                    arguments,
                } = expression.kind
                else {
                    unreachable!()
                };
                (
                    HirStatementKind::Call {
                        function,
                        arguments,
                    },
                    Flow::FallsThrough,
                )
            }
            ast::StatementKind::MacroInvocation { name, .. } => {
                return Err(FrontendError::at(
                    name.offset,
                    "macro invocation was not expanded before type checking",
                ));
            }
            ast::StatementKind::Abort => (HirStatementKind::Abort, Flow::Stops),
            ast::StatementKind::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|value| self.analyze_expression(value))
                    .transpose()?;
                match (&value, self.return_type) {
                    (None, TypeId::VOID) => {}
                    (None, _) => {
                        return Err(FrontendError::at(
                            statement.offset,
                            "non-void function must return a value",
                        ));
                    }
                    (Some(_), TypeId::VOID) => {
                        return Err(FrontendError::at(
                            statement.offset,
                            "void function cannot return a value",
                        ));
                    }
                    (Some(value), expected) => {
                        require_type(self.context, value, expected, "return value")?;
                    }
                }
                (HirStatementKind::Return(value), Flow::Stops)
            }
            ast::StatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.analyze_expression(condition)?;
                require_type(self.context, &condition, TypeId::CELL, "if condition")?;
                let (then_branch, then_flow) = self.analyze_statement(then_branch)?;
                let (else_branch, else_flow) = else_branch
                    .as_ref()
                    .map(|branch| self.analyze_statement(branch))
                    .transpose()?
                    .map_or((None, Flow::FallsThrough), |(branch, flow)| {
                        (Some(Box::new(branch)), flow)
                    });
                let flow = match hir::constant_cell_value(&condition) {
                    Some(0) => else_flow,
                    Some(_) => then_flow,
                    None if then_flow == Flow::Stops && else_flow == Flow::Stops => Flow::Stops,
                    None => Flow::FallsThrough,
                };
                (
                    HirStatementKind::If {
                        condition,
                        then_branch: Box::new(then_branch),
                        else_branch,
                    },
                    flow,
                )
            }
            ast::StatementKind::While { condition, body } => {
                let condition = self.analyze_expression(condition)?;
                require_type(self.context, &condition, TypeId::CELL, "while condition")?;
                let (body, _) = self.analyze_statement(body)?;
                let flow = if hir::constant_cell_value(&condition).is_some_and(|value| value != 0) {
                    Flow::Stops
                } else {
                    Flow::FallsThrough
                };
                (
                    HirStatementKind::While {
                        condition,
                        body: Box::new(body),
                    },
                    flow,
                )
            }
        };
        Ok((
            HirStatement {
                kind,
                offset: statement.offset,
            },
            flow,
        ))
    }

    fn resolve_local_type(
        &self,
        ty: &ast::Type,
        initializer: Option<&ast::Expression>,
        offset: usize,
    ) -> Result<TypeId, FrontendError> {
        if matches!(ty, ast::Type::InferredCellArray) {
            let Some(ast::Expression {
                kind: ast::ExpressionKind::StringLiteral(bytes),
                ..
            }) = initializer
            else {
                return Err(FrontendError::at(
                    offset,
                    "cell[] requires a direct string literal initializer",
                ));
            };
            return Ok(self.context.cell_array(bytes.len()));
        }
        self.resolve_ast_type(ty)
    }

    fn resolve_ast_type(&self, ty: &ast::Type) -> Result<TypeId, FrontendError> {
        match ty {
            ast::Type::Cell => Ok(TypeId::CELL),
            ast::Type::Named(name) => match self.context.names.get(&name.text) {
                Some(TopSymbol::Type(id)) => Ok(*id),
                _ => Err(FrontendError::at(
                    name.offset,
                    format!("unknown type {:?}", name.text),
                )),
            },
            ast::Type::Array { element, length } => {
                let element = self.resolve_ast_type(element)?;
                let length = match length {
                    ast::ArrayLength::Literal { value, .. } => *value,
                    ast::ArrayLength::Constant(name) => {
                        let Some(TopSymbol::Constant(index)) =
                            self.context.names.get(&name.text).copied()
                        else {
                            return Err(FrontendError::at(
                                name.offset,
                                "array length name must refer to a const cell",
                            ));
                        };
                        usize::from(self.context.constants[index].unwrap())
                    }
                };
                self.context
                    .array_types
                    .get(&(element, length))
                    .copied()
                    .ok_or_else(|| FrontendError::at(0, "unresolved array type"))
            }
            ast::Type::InferredCellArray => Err(FrontendError::at(
                0,
                "cell[] requires a direct string literal initializer",
            )),
            ast::Type::Void => Ok(TypeId::VOID),
        }
    }

    fn analyze_expression(
        &mut self,
        expression: &ast::Expression,
    ) -> Result<HirExpression, FrontendError> {
        let (kind, ty) = match &expression.kind {
            ast::ExpressionKind::Literal(value) => {
                (HirExpressionKind::Literal(*value), TypeId::CELL)
            }
            ast::ExpressionKind::StringLiteral(bytes) => (
                HirExpressionKind::StringLiteral(bytes.clone()),
                self.context.cell_array(bytes.len()),
            ),
            ast::ExpressionKind::Name(name) => return self.resolve_name_expression(name),
            ast::ExpressionKind::EnumVariant { enum_name, variant } => {
                let Some(TopSymbol::Type(ty)) = self.context.names.get(&enum_name.text).copied()
                else {
                    return Err(FrontendError::at(
                        enum_name.offset,
                        format!("unknown enum type {:?}", enum_name.text),
                    ));
                };
                let TypeKind::Enum { variants } = self.context.types.kind(ty) else {
                    return Err(FrontendError::at(
                        enum_name.offset,
                        format!("{:?} is not an enum type", enum_name.text),
                    ));
                };
                let Some(value) = variants
                    .iter()
                    .find(|candidate| candidate.name == variant.text)
                    .map(|candidate| candidate.value)
                else {
                    return Err(FrontendError::at(
                        variant.offset,
                        format!(
                            "enum {:?} has no variant {:?}",
                            enum_name.text, variant.text
                        ),
                    ));
                };
                (HirExpressionKind::EnumVariant(value), ty)
            }
            ast::ExpressionKind::Input => (HirExpressionKind::Input, TypeId::CELL),
            ast::ExpressionKind::Call { name, arguments } => {
                return self.analyze_call(name, arguments, None, expression.offset);
            }
            ast::ExpressionKind::MethodCall {
                receiver,
                name,
                arguments,
            } => {
                return self.analyze_call(name, arguments, Some(receiver), expression.offset);
            }
            ast::ExpressionKind::Field { base, field } => {
                let base = self.analyze_expression(base)?;
                let TypeKind::Struct { fields } = self.context.types.kind(base.ty) else {
                    return Err(FrontendError::at(
                        field.offset,
                        format!("type {} has no fields", self.context.types.name(base.ty)),
                    ));
                };
                let Some(field_definition) = fields.iter().find(|item| item.name == field.text)
                else {
                    return Err(FrontendError::at(
                        field.offset,
                        format!(
                            "struct {} has no field {:?}",
                            self.context.types.name(base.ty),
                            field.text
                        ),
                    ));
                };
                return Ok(project_expression(
                    base,
                    Projection::Field {
                        cell_offset: field_definition.cell_offset,
                    },
                    field_definition.ty,
                    expression.offset,
                ));
            }
            ast::ExpressionKind::Index { base, index } => {
                let base = self.analyze_expression(base)?;
                let TypeKind::Array { element, length } = *self.context.types.kind(base.ty) else {
                    return Err(FrontendError::at(
                        expression.offset,
                        format!(
                            "type {} cannot be indexed",
                            self.context.types.name(base.ty)
                        ),
                    ));
                };
                let index = self.analyze_expression(index)?;
                require_type(self.context, &index, TypeId::CELL, "array index")?;
                let index = if let Some(value) = hir::constant_cell_value(&index) {
                    if usize::from(value) >= length {
                        return Err(FrontendError::at(
                            index.offset,
                            format!(
                                "constant array index {value} is out of bounds for length {length}"
                            ),
                        ));
                    }
                    ArrayIndex::Constant(value)
                } else {
                    ArrayIndex::Dynamic(Box::new(index))
                };
                return Ok(project_expression(
                    base,
                    Projection::Index {
                        index,
                        length,
                        element_cells: self.context.types.cells(element),
                    },
                    element,
                    expression.offset,
                ));
            }
            ast::ExpressionKind::Len(operand) => {
                let operand = self.analyze_expression(operand)?;
                let TypeKind::Array { length, .. } = self.context.types.kind(operand.ty) else {
                    return Err(FrontendError::at(
                        expression.offset,
                        "len operand must have an array type",
                    ));
                };
                let value = u8::try_from(*length).map_err(|_| {
                    FrontendError::at(
                        expression.offset,
                        "len result 256 cannot be represented as a runtime cell",
                    )
                })?;
                (HirExpressionKind::Literal(value), TypeId::CELL)
            }
            ast::ExpressionKind::Unary { operator, operand } => {
                let operand = self.analyze_expression(operand)?;
                require_type(self.context, &operand, TypeId::CELL, "unary operand")?;
                (
                    HirExpressionKind::Unary {
                        operator: match operator {
                            ast::UnaryOperator::Plus => hir::UnaryOperator::Plus,
                            ast::UnaryOperator::Negate => hir::UnaryOperator::Negate,
                            ast::UnaryOperator::Not => hir::UnaryOperator::Not,
                        },
                        operand: Box::new(operand),
                    },
                    TypeId::CELL,
                )
            }
            ast::ExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.analyze_expression(left)?;
                let right = self.analyze_expression(right)?;
                let operator = match operator {
                    ast::BinaryOperator::Add => hir::BinaryOperator::Add,
                    ast::BinaryOperator::Subtract => hir::BinaryOperator::Subtract,
                    ast::BinaryOperator::Less => hir::BinaryOperator::Less,
                    ast::BinaryOperator::LessEqual => hir::BinaryOperator::LessEqual,
                    ast::BinaryOperator::Greater => hir::BinaryOperator::Greater,
                    ast::BinaryOperator::GreaterEqual => hir::BinaryOperator::GreaterEqual,
                    ast::BinaryOperator::Equal => hir::BinaryOperator::Equal,
                    ast::BinaryOperator::NotEqual => hir::BinaryOperator::NotEqual,
                    ast::BinaryOperator::LogicalAnd => hir::BinaryOperator::LogicalAnd,
                    ast::BinaryOperator::LogicalOr => hir::BinaryOperator::LogicalOr,
                };
                match operator {
                    hir::BinaryOperator::Equal | hir::BinaryOperator::NotEqual
                        if left.ty == right.ty
                            && (left.ty == TypeId::CELL
                                || matches!(
                                    self.context.types.kind(left.ty),
                                    TypeKind::Enum { .. }
                                )) => {}
                    hir::BinaryOperator::Equal | hir::BinaryOperator::NotEqual => {
                        return Err(FrontendError::at(
                            expression.offset,
                            "equality operands must both be cell or the same enum type",
                        ));
                    }
                    _ => {
                        require_type(self.context, &left, TypeId::CELL, "binary left operand")?;
                        require_type(self.context, &right, TypeId::CELL, "binary right operand")?;
                    }
                }
                (
                    HirExpressionKind::Binary {
                        operator,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    TypeId::CELL,
                )
            }
        };
        Ok(HirExpression {
            kind,
            ty,
            offset: expression.offset,
        })
    }

    fn resolve_name_expression(&self, name: &ast::Name) -> Result<HirExpression, FrontendError> {
        if name.context != NameContext::DefinitionSite {
            for scope in self.scopes.iter().rev() {
                if let Some(local) = scope.get(&(name.text.clone(), name.context)) {
                    let local_definition = &self.locals[local.index()];
                    return Ok(HirExpression {
                        kind: HirExpressionKind::Place(HirPlace {
                            root: VariableRef::Local(*local),
                            projections: vec![],
                            ty: local_definition.ty,
                        }),
                        ty: local_definition.ty,
                        offset: name.offset,
                    });
                }
            }
        }
        match self.context.names.get(&name.text).copied() {
            Some(TopSymbol::Global(id)) => {
                let global = self.context.globals[id.index()];
                Ok(HirExpression {
                    kind: HirExpressionKind::Place(HirPlace {
                        root: VariableRef::Global(id),
                        projections: vec![],
                        ty: global.ty,
                    }),
                    ty: global.ty,
                    offset: name.offset,
                })
            }
            Some(TopSymbol::Constant(index)) => Ok(HirExpression {
                kind: HirExpressionKind::Literal(self.context.constants[index].unwrap()),
                ty: TypeId::CELL,
                offset: name.offset,
            }),
            Some(_) => Err(FrontendError::at(
                name.offset,
                format!("{:?} is not a value", name.text),
            )),
            None => Err(FrontendError::at(
                name.offset,
                format!("undefined variable {:?}", name.text),
            )),
        }
    }

    fn analyze_call(
        &mut self,
        name: &ast::Name,
        arguments: &[ast::Expression],
        receiver: Option<&ast::Expression>,
        offset: usize,
    ) -> Result<HirExpression, FrontendError> {
        let Some(TopSymbol::Function(id)) = self.context.names.get(&name.text).copied() else {
            return Err(FrontendError::at(
                name.offset,
                format!("undefined function {:?}", name.text),
            ));
        };
        if name.text == "main" {
            return Err(FrontendError::at(
                name.offset,
                "main cannot be called explicitly",
            ));
        }
        let function = &self.context.functions[id.index()];
        let actual_count = arguments.len() + usize::from(receiver.is_some());
        if actual_count != function.signature.parameter_types.len() {
            return Err(FrontendError::at(
                name.offset,
                format!(
                    "function {:?} expects {} argument(s), but {actual_count} were provided",
                    name.text,
                    function.signature.parameter_types.len()
                ),
            ));
        }
        let mut hir_arguments = Vec::with_capacity(actual_count);
        if let Some(receiver) = receiver {
            hir_arguments.push(self.analyze_expression(receiver)?);
        }
        for argument in arguments {
            hir_arguments.push(self.analyze_expression(argument)?);
        }
        for (argument, expected) in hir_arguments
            .iter()
            .zip(&function.signature.parameter_types)
        {
            require_type(self.context, argument, *expected, "function argument")?;
        }
        Ok(HirExpression {
            kind: HirExpressionKind::Call {
                function: id,
                arguments: hir_arguments,
            },
            ty: function.signature.return_type,
            offset,
        })
    }

    fn declare_local(&mut self, name: &ast::Name, ty: TypeId) -> Result<LocalId, FrontendError> {
        let scope = self.scopes.last_mut().unwrap();
        let key = (name.text.clone(), name.context);
        if scope.contains_key(&key) {
            return Err(already_declared(name));
        }
        let id = LocalId::new(self.locals.len());
        scope.insert(key, id);
        self.locals.push(HirLocal {
            id,
            name: name.text.clone(),
            offset: name.offset,
            ty,
        });
        Ok(id)
    }
}

fn project_expression(
    base: HirExpression,
    projection: Projection,
    ty: TypeId,
    offset: usize,
) -> HirExpression {
    let kind = match base.kind {
        HirExpressionKind::Place(mut place) => {
            place.projections.push(projection);
            place.ty = ty;
            HirExpressionKind::Place(place)
        }
        HirExpressionKind::Project {
            base,
            mut projections,
        } => {
            projections.push(projection);
            HirExpressionKind::Project { base, projections }
        }
        kind => HirExpressionKind::Project {
            base: Box::new(HirExpression {
                kind,
                ty: base.ty,
                offset: base.offset,
            }),
            projections: vec![projection],
        },
    };
    HirExpression { kind, ty, offset }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    FallsThrough,
    Stops,
}

fn require_type(
    context: &FileContext<'_>,
    expression: &HirExpression,
    expected: TypeId,
    context_name: &str,
) -> Result<(), FrontendError> {
    if expression.ty == expected {
        Ok(())
    } else if expression.ty == TypeId::VOID {
        Err(FrontendError::at(
            expression.offset,
            format!("void function call cannot be used as a value for {context_name}"),
        ))
    } else {
        Err(FrontendError::at(
            expression.offset,
            format!(
                "{context_name} expected {}, found {}",
                context.types.name(expected),
                context.types.name(expression.ty)
            ),
        ))
    }
}

fn already_declared(name: &ast::Name) -> FrontendError {
    FrontendError::at(
        name.offset,
        format!("name {:?} is already declared in this scope", name.text),
    )
}

fn evaluate_binary(operator: ast::BinaryOperator, left: u8, right: u8) -> u8 {
    match operator {
        ast::BinaryOperator::Add => left.wrapping_add(right),
        ast::BinaryOperator::Subtract => left.wrapping_sub(right),
        ast::BinaryOperator::Less => u8::from(left < right),
        ast::BinaryOperator::LessEqual => u8::from(left <= right),
        ast::BinaryOperator::Greater => u8::from(left > right),
        ast::BinaryOperator::GreaterEqual => u8::from(left >= right),
        ast::BinaryOperator::Equal => u8::from(left == right),
        ast::BinaryOperator::NotEqual => u8::from(left != right),
        ast::BinaryOperator::LogicalAnd => u8::from(left != 0 && right != 0),
        ast::BinaryOperator::LogicalOr => u8::from(left != 0 || right != 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser};

    fn analyze_source(source: &str) -> Result<HirProgram, FrontendError> {
        let tokens = lexer::lex(source)?;
        let ast = parser::parse(tokens)?;
        analyze(&ast)
    }

    #[test]
    fn lays_out_nominal_types_and_multidimensional_arrays() {
        let program = analyze_source(
            "enum K { Zero, One } struct Pair { K kind; cell[2] bytes; } \
             Pair[3][4] values; void main() { values[1][2].bytes[0] = 9; }",
        )
        .unwrap();
        assert_eq!(program.types.cells(program.globals[0].ty), 36);
    }

    #[test]
    fn validates_enum_and_struct_definitions() {
        assert!(
            analyze_source("enum Bad { A = 1 } void main() {}")
                .unwrap_err()
                .message()
                .contains("discriminant 0")
        );
        assert!(
            analyze_source("struct Bad { Bad value; } void main() {}")
                .unwrap_err()
                .message()
                .contains("recursively")
        );
    }

    #[test]
    fn strings_len_constants_and_methods_are_typed() {
        analyze_source(
            r#"
            const cell N = 2 + 1;
            cell id(cell value) { return value; }
            void main() {
                cell[] text = "a\0b";
                cell[N] copy = text;
                output(copy[0].id() + len(text));
            }
            "#,
        )
        .unwrap();
    }

    #[test]
    fn abort_satisfies_nonvoid_return_flow() {
        analyze_source("cell fail() { abort(); } void main() { fail(); }").unwrap_err();
        analyze_source("cell fail() { abort(); } void main() {}").unwrap();
    }

    #[test]
    fn len_is_allowed_in_constant_expressions_without_evaluating_its_operand() {
        analyze_source(
            "cell[3] values; const cell N = len(values); \
             cell[3] identity(cell[3] value) { return value; } \
             const cell M = len(identity(values)); \
             void main() { output(N + M); }",
        )
        .unwrap();
    }

    #[test]
    fn len_type_checks_unevaluated_nested_expressions_and_constant_indices() {
        for source in [
            "struct Z { cell q; } Z z; cell[2][3] xs; \
             const cell N = len(xs[+z]); void main() {}",
            "cell[2][3] xs; const cell N = len(xs[2]); void main() {}",
        ] {
            assert!(
                analyze_source(source).is_err(),
                "invalid unevaluated expression was accepted: {source}"
            );
        }
        analyze_source(
            "cell[2][3] xs; cell runtime; \
             const cell N = len(xs[runtime]); void main() { output(N); }",
        )
        .unwrap();
    }

    #[test]
    fn mixed_len_and_array_length_cycles_are_diagnosed() {
        let error = analyze_source("const cell N = len(values); cell[N] values; void main() {}")
            .unwrap_err();
        assert!(error.message().contains("cycle"));
    }

    #[test]
    fn method_call_statements_accept_non_identifier_receivers() {
        analyze_source(
            "void discard(cell value) {} void main() { input().discard(); ('x').discard(); }",
        )
        .unwrap();
    }
}
