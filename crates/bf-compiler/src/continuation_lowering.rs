//! Lowering from typed HIR to continuation-oriented ABI IR.

use std::error::Error;
use std::fmt;

use crate::continuation_ir::{
    Address, ArrayRegion, Continuation, ContinuationId, ContinuationIrError, ContinuationProgram,
    FrameArrayDescriptor, FrameArrayId, FrameInstruction, FrameSlot, FrameTransferTarget,
    FunctionDescriptor, FunctionId as ContinuationFunctionId, GlobalDescriptor,
    GlobalId as ContinuationGlobalId, ParameterLocation, Terminator, ValueOperand, ValueType,
};
use crate::hir::{
    self, ArrayIndex, AssignmentOperator, BinaryOperator, HirExpression, HirExpressionKind,
    HirFunction, HirPlace, HirProgram, HirStatement, HirStatementKind, Type, UnaryOperator,
    VariableRef,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContinuationLoweringError {
    ContinuationIdsExhausted,
    InvalidHir {
        function: Option<hir::FunctionId>,
        detail: &'static str,
    },
    InvalidContinuationIr(ContinuationIrError),
}

impl fmt::Display for ContinuationLoweringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContinuationIdsExhausted => {
                write!(
                    f,
                    "continuation program needs more than 65535 dispatcher IDs"
                )
            }
            Self::InvalidHir {
                function: Some(function),
                detail,
            } => {
                write!(f, "invalid HIR for function {}: {detail}", function.index())
            }
            Self::InvalidHir {
                function: None,
                detail,
            } => {
                write!(f, "invalid HIR program: {detail}")
            }
            Self::InvalidContinuationIr(error) => {
                write!(f, "invalid generated continuation IR: {error}")
            }
        }
    }
}

impl Error for ContinuationLoweringError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidContinuationIr(error) => Some(error),
            Self::ContinuationIdsExhausted | Self::InvalidHir { .. } => None,
        }
    }
}

impl From<ContinuationIrError> for ContinuationLoweringError {
    fn from(error: ContinuationIrError) -> Self {
        Self::InvalidContinuationIr(error)
    }
}

pub(crate) fn lower_hir(
    program: &HirProgram,
) -> Result<ContinuationProgram, ContinuationLoweringError> {
    validate_program_shape(program)?;

    let mut ids = IdAllocator::with_reserved_entries(program.functions.len())?;
    let entries = (0..program.functions.len())
        .map(|index| continuation_id(index + 1))
        .collect::<Result<Vec<_>, _>>()?;

    let globals = program
        .globals
        .iter()
        .map(|global| {
            GlobalDescriptor::new(
                ContinuationGlobalId::new(global.id.index()),
                value_type(global.ty),
            )
        })
        .collect();

    let mut functions = Vec::with_capacity(program.functions.len());
    let mut continuations = Vec::new();
    for (index, function) in program.functions.iter().enumerate() {
        let function_id = ContinuationFunctionId::new(function.id.index());
        let mut lowerer =
            FunctionLowerer::new(program, function, function_id, entries[index], &mut ids)?;
        lowerer.lower()?;
        functions.push(lowerer.descriptor(entries[index]));
        continuations.extend(lowerer.continuations);
    }

    ContinuationProgram::new_with_globals(
        ContinuationFunctionId::new(program.entry.index()),
        globals,
        functions,
        continuations,
    )
    .map_err(Into::into)
}

fn continuation_id(value: usize) -> Result<ContinuationId, ContinuationLoweringError> {
    let value =
        u16::try_from(value).map_err(|_| ContinuationLoweringError::ContinuationIdsExhausted)?;
    ContinuationId::new(value).ok_or(ContinuationLoweringError::ContinuationIdsExhausted)
}

const fn value_type(ty: Type) -> ValueType {
    match ty {
        Type::Cell => ValueType::Cell,
        Type::Array(cells) => ValueType::Array(cells),
        Type::Void => ValueType::Void,
    }
}

fn invalid_hir(
    function: Option<hir::FunctionId>,
    detail: &'static str,
) -> ContinuationLoweringError {
    ContinuationLoweringError::InvalidHir { function, detail }
}

fn validate_program_shape(program: &HirProgram) -> Result<(), ContinuationLoweringError> {
    let Some(entry) = program.functions.get(program.entry.index()) else {
        return Err(invalid_hir(None, "entry function is out of bounds"));
    };
    if entry.id != program.entry {
        return Err(invalid_hir(None, "entry ID does not index its function"));
    }
    if entry.signature.return_type != Type::Void || !entry.parameters.is_empty() {
        return Err(invalid_hir(
            Some(entry.id),
            "main must have signature void main()",
        ));
    }

    for (index, global) in program.globals.iter().enumerate() {
        if global.id.index() != index {
            return Err(invalid_hir(None, "global IDs must match vector indices"));
        }
        if !valid_value_type(global.ty) {
            return Err(invalid_hir(None, "global has an invalid value type"));
        }
        if global
            .initializer
            .as_ref()
            .is_some_and(|value| value.ty != Type::Cell)
            || matches!(global.ty, Type::Array(_)) && global.initializer.is_some()
        {
            return Err(invalid_hir(
                None,
                "global initializer does not match its type",
            ));
        }
    }

    for (index, function) in program.functions.iter().enumerate() {
        if function.id.index() != index {
            return Err(invalid_hir(
                Some(function.id),
                "function IDs must match vector indices",
            ));
        }
        if !valid_declared_type(function.signature.return_type) {
            return Err(invalid_hir(
                Some(function.id),
                "invalid function return type",
            ));
        }
        if function.parameters.len() != function.signature.parameter_types.len() {
            return Err(invalid_hir(
                Some(function.id),
                "parameter list and signature have different lengths",
            ));
        }
        for (local_index, local) in function.locals.iter().enumerate() {
            if local.id.index() != local_index || !valid_value_type(local.ty) {
                return Err(invalid_hir(
                    Some(function.id),
                    "local IDs/types are not well formed",
                ));
            }
        }
        let mut seen = vec![false; function.locals.len()];
        for (parameter, expected) in function
            .parameters
            .iter()
            .zip(&function.signature.parameter_types)
        {
            let Some(local) = function.locals.get(parameter.local.index()) else {
                return Err(invalid_hir(
                    Some(function.id),
                    "parameter local is out of bounds",
                ));
            };
            if local.ty != *expected || !valid_value_type(*expected) {
                return Err(invalid_hir(
                    Some(function.id),
                    "parameter local does not match its signature type",
                ));
            }
            if seen[parameter.local.index()] {
                return Err(invalid_hir(
                    Some(function.id),
                    "two parameters use the same local",
                ));
            }
            seen[parameter.local.index()] = true;
        }
    }
    Ok(())
}

const fn valid_declared_type(ty: Type) -> bool {
    matches!(ty, Type::Cell | Type::Void)
        || matches!(ty, Type::Array(cells) if cells >= 1 && cells <= 256)
}

const fn valid_value_type(ty: Type) -> bool {
    !matches!(ty, Type::Void) && valid_declared_type(ty)
}

struct IdAllocator {
    next: usize,
}

impl IdAllocator {
    fn with_reserved_entries(count: usize) -> Result<Self, ContinuationLoweringError> {
        if count > usize::from(u16::MAX) {
            return Err(ContinuationLoweringError::ContinuationIdsExhausted);
        }
        Ok(Self { next: count + 1 })
    }

    fn allocate(&mut self) -> Result<ContinuationId, ContinuationLoweringError> {
        let id = continuation_id(self.next)?;
        self.next += 1;
        Ok(id)
    }
}

struct OpenContinuation {
    id: ContinuationId,
    body: Vec<FrameInstruction>,
}

#[derive(Debug, Clone, Copy)]
enum LocalStorage {
    Cell(FrameSlot),
    Array(FrameArrayId, usize),
}

struct FunctionLowerer<'a, 'ids> {
    program: &'a HirProgram,
    source: &'a HirFunction,
    function: ContinuationFunctionId,
    main: bool,
    ids: &'ids mut IdAllocator,
    current: Option<OpenContinuation>,
    continuations: Vec<Continuation>,
    local_storage: Vec<LocalStorage>,
    frame_arrays: Vec<FrameArrayDescriptor>,
    next_slot: usize,
    outbox_cells: usize,
}

impl<'a, 'ids> FunctionLowerer<'a, 'ids> {
    fn new(
        program: &'a HirProgram,
        source: &'a HirFunction,
        function: ContinuationFunctionId,
        entry: ContinuationId,
        ids: &'ids mut IdAllocator,
    ) -> Result<Self, ContinuationLoweringError> {
        let mut next_slot = 0;
        let mut frame_arrays = Vec::new();
        let mut local_storage = Vec::with_capacity(source.locals.len());
        for local in &source.locals {
            local_storage.push(match local.ty {
                Type::Cell => {
                    let slot = FrameSlot::new(next_slot);
                    next_slot += 1;
                    LocalStorage::Cell(slot)
                }
                Type::Array(cells) => {
                    let id = FrameArrayId::new(frame_arrays.len());
                    frame_arrays.push(FrameArrayDescriptor::new(id, cells));
                    LocalStorage::Array(id, cells)
                }
                Type::Void => {
                    return Err(invalid_hir(Some(source.id), "void local has no storage"));
                }
            });
        }
        Ok(Self {
            program,
            source,
            function,
            main: source.id == program.entry,
            ids,
            current: Some(OpenContinuation {
                id: entry,
                body: Vec::new(),
            }),
            continuations: Vec::new(),
            local_storage,
            frame_arrays,
            next_slot,
            outbox_cells: 0,
        })
    }

    fn descriptor(&self, entry: ContinuationId) -> FunctionDescriptor {
        let parameters = self
            .source
            .parameters
            .iter()
            .map(
                |parameter| match self.local_storage[parameter.local.index()] {
                    LocalStorage::Cell(slot) => ParameterLocation::Cell(slot),
                    LocalStorage::Array(array, _) => ParameterLocation::Array(array),
                },
            )
            .collect();
        FunctionDescriptor::new_typed(
            self.function,
            parameters,
            self.next_slot,
            self.frame_arrays.clone(),
            self.outbox_cells,
            value_type(self.source.signature.return_type),
            entry,
        )
    }

    fn lower(&mut self) -> Result<(), ContinuationLoweringError> {
        if self.main {
            self.lower_global_initializers()?;
        }
        self.lower_statement(&self.source.body)?;
        if self.current.is_some() {
            match (self.main, self.source.signature.return_type) {
                (true, _) => self.finish(Terminator::Halt),
                (false, Type::Void) => self.finish(Terminator::Return { value: None }),
                (false, Type::Cell | Type::Array(_)) => {
                    return Err(invalid_hir(
                        Some(self.source.id),
                        "value function can reach the end without returning",
                    ));
                }
            }
        }
        Ok(())
    }

    fn lower_global_initializers(&mut self) -> Result<(), ContinuationLoweringError> {
        for global in &self.program.globals {
            let id = ContinuationGlobalId::new(global.id.index());
            match global.ty {
                Type::Cell => {
                    let destination = Address::Global(id);
                    self.emit(FrameInstruction::Set {
                        dst: destination,
                        value: 0,
                    });
                    if let Some(initializer) = &global.initializer {
                        let value = self.temporary_cell();
                        self.evaluate_cell(initializer, value)?;
                        self.move_cell(value, destination);
                    }
                }
                Type::Array(cells) => {
                    let region = ArrayRegion::Global(id);
                    self.clear_array(region, cells);
                }
                Type::Void => {
                    return Err(invalid_hir(None, "void global has no storage"));
                }
            }
        }
        Ok(())
    }

    fn lower_statement(
        &mut self,
        statement: &HirStatement,
    ) -> Result<(), ContinuationLoweringError> {
        if self.current.is_none() {
            return Ok(());
        }
        match &statement.kind {
            HirStatementKind::Empty => {}
            HirStatementKind::Block(statements) => {
                for statement in statements {
                    self.lower_statement(statement)?;
                    if self.current.is_none() {
                        break;
                    }
                }
            }
            HirStatementKind::Declaration { local, initializer } => match self.local(*local)? {
                LocalStorage::Cell(slot) => {
                    let destination = Address::Frame(slot);
                    self.emit(FrameInstruction::Set {
                        dst: destination,
                        value: 0,
                    });
                    if let Some(initializer) = initializer {
                        let value = self.temporary_cell();
                        self.evaluate_cell(initializer, value)?;
                        self.move_cell(value, destination);
                    }
                }
                LocalStorage::Array(array, cells) => {
                    if initializer.is_some() {
                        return Err(invalid_hir(
                            Some(self.source.id),
                            "array declaration has an initializer",
                        ));
                    }
                    self.clear_array(ArrayRegion::Frame(array), cells);
                }
            },
            HirStatementKind::Assignment {
                value,
                target,
                operator,
            } => {
                self.lower_assignment(value, target, *operator)?;
            }
            HirStatementKind::Output(expression) => {
                let value = self.temporary_cell();
                self.evaluate_cell(expression, value)?;
                self.emit(FrameInstruction::Output { src: value });
            }
            HirStatementKind::Call {
                function,
                arguments,
            } => {
                self.lower_call(*function, arguments, Type::Void)?;
            }
            HirStatementKind::Return(value) => self.lower_return(value.as_ref())?,
            HirStatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.lower_if(condition, then_branch, else_branch.as_deref())?;
            }
            HirStatementKind::While { condition, body } => {
                self.lower_while(condition, body)?;
            }
        }
        Ok(())
    }

    fn lower_assignment(
        &mut self,
        value: &HirExpression,
        target: &HirPlace,
        operator: AssignmentOperator,
    ) -> Result<(), ContinuationLoweringError> {
        // Source semantics require the complete RHS before a dynamic LHS index.
        match value.ty {
            Type::Cell => {
                let rhs = self.temporary_cell();
                self.evaluate_cell(value, rhs)?;
                match target {
                    HirPlace::Variable {
                        variable,
                        ty: Type::Cell,
                    } => {
                        self.update_cell(rhs, self.cell_variable(*variable)?, operator);
                    }
                    HirPlace::ArrayElement {
                        array,
                        length,
                        index,
                    } => {
                        let region = self.array_variable(*array, *length)?;
                        self.update_array_element(rhs, region, index, operator)?;
                    }
                    _ => {
                        return Err(invalid_hir(
                            Some(self.source.id),
                            "cell assignment target has a non-cell type",
                        ));
                    }
                }
            }
            Type::Array(cells) if operator == AssignmentOperator::Set => {
                let rhs = self.evaluate_array(value)?;
                let HirPlace::Variable {
                    variable,
                    ty: Type::Array(target_cells),
                } = target
                else {
                    return Err(invalid_hir(
                        Some(self.source.id),
                        "aggregate assignment target is not an array variable",
                    ));
                };
                if cells != *target_cells {
                    return Err(invalid_hir(
                        Some(self.source.id),
                        "aggregate assignment has mismatched lengths",
                    ));
                }
                let destination = self.array_variable(*variable, cells)?;
                self.copy_array(rhs, destination, cells);
            }
            Type::Array(_) | Type::Void => {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "assignment value/operator has an invalid type",
                ));
            }
        }
        Ok(())
    }

    fn update_cell(&mut self, rhs: Address, destination: Address, operator: AssignmentOperator) {
        match operator {
            AssignmentOperator::Set => self.move_cell(rhs, destination),
            AssignmentOperator::Add => self.transfer(rhs, destination, 1),
            AssignmentOperator::Subtract => self.transfer(rhs, destination, 255),
        }
    }

    fn update_array_element(
        &mut self,
        rhs: Address,
        array: ArrayRegion,
        index: &ArrayIndex,
        operator: AssignmentOperator,
    ) -> Result<(), ContinuationLoweringError> {
        match index {
            ArrayIndex::Constant(index) => {
                let destination = Address::ArrayElement {
                    array,
                    index: usize::from(*index),
                };
                self.update_cell(rhs, destination, operator);
            }
            ArrayIndex::Dynamic(index) => {
                let index_value = self.temporary_cell();
                self.evaluate_cell(index, index_value)?;
                match operator {
                    AssignmentOperator::Set => {
                        self.array_store(array, index_value, rhs)?;
                    }
                    AssignmentOperator::Add | AssignmentOperator::Subtract => {
                        let store_index = self.temporary_cell();
                        self.copy_cell(index_value, store_index);
                        let current = self.temporary_cell();
                        self.array_load(array, index_value, current)?;
                        self.transfer(
                            rhs,
                            current,
                            if operator == AssignmentOperator::Add {
                                1
                            } else {
                                255
                            },
                        );
                        self.array_store(array, store_index, current)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn lower_return(
        &mut self,
        value: Option<&HirExpression>,
    ) -> Result<(), ContinuationLoweringError> {
        match (self.source.signature.return_type, value) {
            (Type::Void, None) if self.main => self.finish(Terminator::Halt),
            (Type::Void, None) => self.finish(Terminator::Return { value: None }),
            (Type::Cell, Some(value)) if !self.main => {
                let result = self.temporary_cell();
                self.evaluate_cell(value, result)?;
                self.finish(Terminator::Return {
                    value: Some(ValueOperand::Cell(result)),
                });
            }
            (Type::Array(cells), Some(value)) if !self.main && value.ty == Type::Array(cells) => {
                let result = self.evaluate_array(value)?;
                self.finish(Terminator::Return {
                    value: Some(ValueOperand::Array(result)),
                });
            }
            _ => {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "return value does not match the function return type",
                ));
            }
        }
        Ok(())
    }

    fn lower_if(
        &mut self,
        condition: &HirExpression,
        then_branch: &HirStatement,
        else_branch: Option<&HirStatement>,
    ) -> Result<(), ContinuationLoweringError> {
        self.require_cell(condition)?;
        if let Some(condition) = hir::constant_cell_value(condition) {
            if condition != 0 {
                self.lower_statement(then_branch)?;
            } else if let Some(else_branch) = else_branch {
                self.lower_statement(else_branch)?;
            }
            return Ok(());
        }

        let condition_value = self.temporary_cell();
        self.evaluate_cell(condition, condition_value)?;
        let then_id = self.ids.allocate()?;
        let else_id = self.ids.allocate()?;
        self.finish(Terminator::Branch {
            condition: condition_value,
            then_target: then_id,
            else_target: else_id,
        });

        let mut join_id = None;
        self.start(then_id);
        self.lower_statement(then_branch)?;
        if self.current.is_some() {
            let target = self.ids.allocate()?;
            join_id = Some(target);
            self.finish(Terminator::Goto { target });
        }

        self.start(else_id);
        if let Some(else_branch) = else_branch {
            self.lower_statement(else_branch)?;
        }
        if self.current.is_some() {
            let target = match join_id {
                Some(target) => target,
                None => {
                    let target = self.ids.allocate()?;
                    join_id = Some(target);
                    target
                }
            };
            self.finish(Terminator::Goto { target });
        }
        if let Some(join_id) = join_id {
            self.start(join_id);
        }
        Ok(())
    }

    fn lower_while(
        &mut self,
        condition: &HirExpression,
        body: &HirStatement,
    ) -> Result<(), ContinuationLoweringError> {
        self.require_cell(condition)?;
        if let Some(condition) = hir::constant_cell_value(condition) {
            if condition == 0 {
                return Ok(());
            }
            let body_id = self.ids.allocate()?;
            self.finish(Terminator::Goto { target: body_id });
            self.start(body_id);
            self.lower_statement(body)?;
            if self.current.is_some() {
                self.finish(Terminator::Goto { target: body_id });
            }
            return Ok(());
        }

        let condition_id = self.ids.allocate()?;
        let body_id = self.ids.allocate()?;
        let after_id = self.ids.allocate()?;
        self.finish(Terminator::Goto {
            target: condition_id,
        });
        self.start(condition_id);
        let condition_value = self.temporary_cell();
        self.evaluate_cell(condition, condition_value)?;
        self.finish(Terminator::Branch {
            condition: condition_value,
            then_target: body_id,
            else_target: after_id,
        });
        self.start(body_id);
        self.lower_statement(body)?;
        if self.current.is_some() {
            self.finish(Terminator::Goto {
                target: condition_id,
            });
        }
        self.start(after_id);
        Ok(())
    }

    fn evaluate_cell(
        &mut self,
        expression: &HirExpression,
        destination: Address,
    ) -> Result<(), ContinuationLoweringError> {
        self.require_cell(expression)?;
        match &expression.kind {
            HirExpressionKind::Literal(value) => {
                self.emit(FrameInstruction::Set {
                    dst: destination,
                    value: *value,
                });
            }
            HirExpressionKind::Variable(variable) => {
                self.copy_cell(self.cell_variable(*variable)?, destination);
            }
            HirExpressionKind::ArrayElement {
                array,
                length,
                index,
            } => {
                let array = self.array_variable(*array, *length)?;
                match index {
                    ArrayIndex::Constant(index) => self.copy_cell(
                        Address::ArrayElement {
                            array,
                            index: usize::from(*index),
                        },
                        destination,
                    ),
                    ArrayIndex::Dynamic(index) => {
                        let index = self.evaluate_cell_temporary(index)?;
                        self.array_load(array, index, destination)?;
                    }
                }
            }
            HirExpressionKind::Input => self.emit(FrameInstruction::Input { dst: destination }),
            HirExpressionKind::Unary { operator, operand } => match operator {
                UnaryOperator::Plus => self.evaluate_cell(operand, destination)?,
                UnaryOperator::Negate => {
                    let value = self.evaluate_cell_temporary(operand)?;
                    self.emit(FrameInstruction::Set {
                        dst: destination,
                        value: 0,
                    });
                    self.transfer(value, destination, 255);
                }
                UnaryOperator::Not => {
                    let value = self.evaluate_cell_temporary(operand)?;
                    self.boolean_from(value, destination, 0, 1);
                }
            },
            HirExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                self.evaluate_binary(*operator, left, right, destination)?;
            }
            HirExpressionKind::Call {
                function,
                arguments,
            } => {
                self.lower_call(*function, arguments, Type::Cell)?;
                self.move_cell(Address::AbiValue, destination);
            }
        }
        Ok(())
    }

    fn evaluate_array(
        &mut self,
        expression: &HirExpression,
    ) -> Result<ArrayRegion, ContinuationLoweringError> {
        let Type::Array(cells) = expression.ty else {
            return Err(invalid_hir(
                Some(self.source.id),
                "array expression has non-array type",
            ));
        };
        let snapshot = self.temporary_array(cells);
        match &expression.kind {
            HirExpressionKind::Variable(variable) => {
                let source = self.array_variable(*variable, cells)?;
                self.copy_array(source, snapshot, cells);
            }
            HirExpressionKind::Call {
                function,
                arguments,
            } => {
                self.lower_call(*function, arguments, Type::Array(cells))?;
                self.copy_array(ArrayRegion::Outbox, snapshot, cells);
            }
            _ => {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "unsupported aggregate expression kind",
                ));
            }
        }
        Ok(snapshot)
    }

    fn evaluate_binary(
        &mut self,
        operator: BinaryOperator,
        left: &HirExpression,
        right: &HirExpression,
        destination: Address,
    ) -> Result<(), ContinuationLoweringError> {
        self.require_cell(left)?;
        self.require_cell(right)?;
        match operator {
            BinaryOperator::Add | BinaryOperator::Subtract => {
                self.evaluate_cell(left, destination)?;
                let right = self.evaluate_cell_temporary(right)?;
                self.transfer(
                    right,
                    destination,
                    if operator == BinaryOperator::Add {
                        1
                    } else {
                        255
                    },
                );
            }
            BinaryOperator::Equal | BinaryOperator::NotEqual => {
                let left = self.evaluate_cell_temporary(left)?;
                let right = self.evaluate_cell_temporary(right)?;
                self.transfer(right, left, 255);
                let (nonzero, zero) = if operator == BinaryOperator::Equal {
                    (0, 1)
                } else {
                    (1, 0)
                };
                self.boolean_from(left, destination, nonzero, zero);
            }
            BinaryOperator::Less
            | BinaryOperator::LessEqual
            | BinaryOperator::Greater
            | BinaryOperator::GreaterEqual => {
                let left = self.evaluate_cell_temporary(left)?;
                let right = self.evaluate_cell_temporary(right)?;
                match operator {
                    BinaryOperator::Less => self.less_than(left, right, destination, 1, 0),
                    BinaryOperator::LessEqual => self.less_than(right, left, destination, 0, 1),
                    BinaryOperator::Greater => self.less_than(right, left, destination, 1, 0),
                    BinaryOperator::GreaterEqual => self.less_than(left, right, destination, 0, 1),
                    _ => unreachable!(),
                }
            }
            BinaryOperator::LogicalAnd | BinaryOperator::LogicalOr => {
                self.evaluate_logical(operator, left, right, destination)?;
            }
        }
        Ok(())
    }

    fn evaluate_logical(
        &mut self,
        operator: BinaryOperator,
        left: &HirExpression,
        right: &HirExpression,
        destination: Address,
    ) -> Result<(), ContinuationLoweringError> {
        let left_value = self.evaluate_cell_temporary(left)?;
        let right_id = self.ids.allocate()?;
        let short_id = self.ids.allocate()?;
        let join_id = self.ids.allocate()?;
        let (then_target, else_target, short_value) = if operator == BinaryOperator::LogicalAnd {
            (right_id, short_id, 0)
        } else {
            (short_id, right_id, 1)
        };
        self.finish(Terminator::Branch {
            condition: left_value,
            then_target,
            else_target,
        });
        self.start(right_id);
        let right_value = self.evaluate_cell_temporary(right)?;
        self.boolean_from(right_value, destination, 1, 0);
        self.finish(Terminator::Goto { target: join_id });
        self.start(short_id);
        self.emit(FrameInstruction::Set {
            dst: destination,
            value: short_value,
        });
        self.finish(Terminator::Goto { target: join_id });
        self.start(join_id);
        Ok(())
    }

    fn lower_call(
        &mut self,
        function: hir::FunctionId,
        arguments: &[HirExpression],
        expected_return: Type,
    ) -> Result<(), ContinuationLoweringError> {
        let callee = self
            .program
            .functions
            .get(function.index())
            .filter(|callee| callee.id == function)
            .ok_or_else(|| invalid_hir(Some(self.source.id), "call target is out of bounds"))?;
        if callee.signature.return_type != expected_return
            || arguments.len() != callee.signature.parameter_types.len()
        {
            return Err(invalid_hir(
                Some(self.source.id),
                "call use does not match the callee signature",
            ));
        }

        let mut operands = Vec::with_capacity(arguments.len());
        for (argument, parameter_type) in arguments.iter().zip(&callee.signature.parameter_types) {
            if argument.ty != *parameter_type {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "call argument type does not match the callee signature",
                ));
            }
            operands.push(match parameter_type {
                Type::Cell => ValueOperand::Cell(self.evaluate_cell_temporary(argument)?),
                Type::Array(_) => ValueOperand::Array(self.evaluate_array(argument)?),
                Type::Void => {
                    return Err(invalid_hir(Some(self.source.id), "void call parameter"));
                }
            });
        }
        if let Type::Array(cells) = expected_return {
            self.outbox_cells = self.outbox_cells.max(cells);
        }
        let resume = self.ids.allocate()?;
        self.finish(Terminator::Call {
            callee: ContinuationFunctionId::new(function.index()),
            arguments: operands,
            return_to: resume,
        });
        self.start(resume);
        Ok(())
    }

    fn array_load(
        &mut self,
        array: ArrayRegion,
        index: Address,
        destination: Address,
    ) -> Result<(), ContinuationLoweringError> {
        let resume = self.ids.allocate()?;
        self.finish(Terminator::ArrayLoad {
            array,
            index,
            destination,
            return_to: resume,
        });
        self.start(resume);
        Ok(())
    }

    fn array_store(
        &mut self,
        array: ArrayRegion,
        index: Address,
        value: Address,
    ) -> Result<(), ContinuationLoweringError> {
        let resume = self.ids.allocate()?;
        self.finish(Terminator::ArrayStore {
            array,
            index,
            value,
            return_to: resume,
        });
        self.start(resume);
        Ok(())
    }

    fn local(&self, local: hir::LocalId) -> Result<LocalStorage, ContinuationLoweringError> {
        self.local_storage
            .get(local.index())
            .copied()
            .ok_or_else(|| invalid_hir(Some(self.source.id), "local ID is out of bounds"))
    }

    fn cell_variable(&self, variable: VariableRef) -> Result<Address, ContinuationLoweringError> {
        match variable {
            VariableRef::Global(global) => self
                .program
                .globals
                .get(global.index())
                .filter(|item| item.id == global && item.ty == Type::Cell)
                .map(|_| Address::Global(ContinuationGlobalId::new(global.index())))
                .ok_or_else(|| {
                    invalid_hir(Some(self.source.id), "global cell reference is invalid")
                }),
            VariableRef::Local(local) => match self.local(local)? {
                LocalStorage::Cell(slot) => Ok(Address::Frame(slot)),
                LocalStorage::Array(_, _) => Err(invalid_hir(
                    Some(self.source.id),
                    "array local used as a cell",
                )),
            },
        }
    }

    fn array_variable(
        &self,
        variable: VariableRef,
        cells: usize,
    ) -> Result<ArrayRegion, ContinuationLoweringError> {
        match variable {
            VariableRef::Global(global) => self
                .program
                .globals
                .get(global.index())
                .filter(|item| item.id == global && item.ty == Type::Array(cells))
                .map(|_| ArrayRegion::Global(ContinuationGlobalId::new(global.index())))
                .ok_or_else(|| {
                    invalid_hir(Some(self.source.id), "global array reference is invalid")
                }),
            VariableRef::Local(local) => match self.local(local)? {
                LocalStorage::Array(array, actual) if actual == cells => {
                    Ok(ArrayRegion::Frame(array))
                }
                LocalStorage::Cell(_) | LocalStorage::Array(_, _) => Err(invalid_hir(
                    Some(self.source.id),
                    "local array reference has the wrong type",
                )),
            },
        }
    }

    fn require_cell(&self, expression: &HirExpression) -> Result<(), ContinuationLoweringError> {
        if expression.ty != Type::Cell {
            return Err(invalid_hir(
                Some(self.source.id),
                "value expression does not have cell type",
            ));
        }
        Ok(())
    }

    fn temporary_cell(&mut self) -> Address {
        let slot = FrameSlot::new(self.next_slot);
        self.next_slot += 1;
        Address::Frame(slot)
    }

    fn evaluate_cell_temporary(
        &mut self,
        expression: &HirExpression,
    ) -> Result<Address, ContinuationLoweringError> {
        let destination = self.temporary_cell();
        self.evaluate_cell(expression, destination)?;
        Ok(destination)
    }

    fn temporary_array(&mut self, cells: usize) -> ArrayRegion {
        let id = FrameArrayId::new(self.frame_arrays.len());
        self.frame_arrays.push(FrameArrayDescriptor::new(id, cells));
        ArrayRegion::Frame(id)
    }

    fn clear_array(&mut self, array: ArrayRegion, cells: usize) {
        for index in 0..cells {
            self.emit(FrameInstruction::Set {
                dst: Address::ArrayElement { array, index },
                value: 0,
            });
        }
    }

    fn copy_array(&mut self, src: ArrayRegion, dst: ArrayRegion, cells: usize) {
        self.emit(FrameInstruction::AggregateCopy { src, dst, cells });
    }

    fn emit(&mut self, instruction: FrameInstruction) {
        self.current
            .as_mut()
            .expect("instructions require a live continuation")
            .body
            .push(instruction);
    }

    fn start(&mut self, id: ContinuationId) {
        debug_assert!(self.current.is_none());
        self.current = Some(OpenContinuation {
            id,
            body: Vec::new(),
        });
    }

    fn finish(&mut self, terminator: Terminator) {
        let current = self
            .current
            .take()
            .expect("terminators require a live continuation");
        self.continuations.push(Continuation::new(
            current.id,
            self.function,
            current.body,
            terminator,
        ));
    }

    fn transfer(&mut self, source: Address, destination: Address, factor: u8) {
        self.emit(FrameInstruction::Transfer {
            src: source,
            targets: vec![FrameTransferTarget {
                dst: destination,
                factor,
            }],
        });
    }

    fn move_cell(&mut self, source: Address, destination: Address) {
        self.emit(FrameInstruction::Set {
            dst: destination,
            value: 0,
        });
        self.transfer(source, destination, 1);
    }

    fn copy_cell(&mut self, source: Address, destination: Address) {
        let restore = self.temporary_cell();
        self.emit(FrameInstruction::Set {
            dst: destination,
            value: 0,
        });
        self.emit(FrameInstruction::Transfer {
            src: source,
            targets: vec![
                FrameTransferTarget {
                    dst: destination,
                    factor: 1,
                },
                FrameTransferTarget {
                    dst: restore,
                    factor: 1,
                },
            ],
        });
        self.transfer(restore, source, 1);
    }

    fn boolean_from(&mut self, value: Address, destination: Address, nonzero: u8, zero: u8) {
        self.emit(FrameInstruction::Branch {
            condition: value,
            then_body: vec![FrameInstruction::Set {
                dst: destination,
                value: nonzero,
            }],
            else_body: vec![FrameInstruction::Set {
                dst: destination,
                value: zero,
            }],
        });
    }

    fn less_than(
        &mut self,
        left: Address,
        right: Address,
        destination: Address,
        true_value: u8,
        false_value: u8,
    ) {
        let right_test = self.temporary_cell();
        let restore = self.temporary_cell();
        let mut body = vec![
            FrameInstruction::Set {
                dst: right_test,
                value: 0,
            },
            FrameInstruction::Transfer {
                src: right,
                targets: vec![
                    FrameTransferTarget {
                        dst: right_test,
                        factor: 1,
                    },
                    FrameTransferTarget {
                        dst: restore,
                        factor: 1,
                    },
                ],
            },
            FrameInstruction::Transfer {
                src: restore,
                targets: vec![FrameTransferTarget {
                    dst: right,
                    factor: 1,
                }],
            },
        ];
        body.push(FrameInstruction::Branch {
            condition: right_test,
            then_body: vec![
                FrameInstruction::AddConst {
                    dst: left,
                    value: 255,
                },
                FrameInstruction::AddConst {
                    dst: right,
                    value: 255,
                },
            ],
            else_body: vec![FrameInstruction::Transfer {
                src: left,
                targets: Vec::new(),
            }],
        });
        self.emit(FrameInstruction::Loop {
            condition: left,
            body,
        });
        self.boolean_from(right, destination, true_value, false_value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser, semantic};

    fn lower(source: &str) -> ContinuationProgram {
        let ast = parser::parse(lexer::lex(source).unwrap()).unwrap();
        let hir = semantic::analyze(&ast).unwrap();
        lower_hir(&hir).unwrap()
    }

    #[test]
    fn globals_and_general_initializers_are_lowered_before_main_body() {
        let program = lower(
            "cell first = input(); cell second = identity(first); \
             cell identity(cell x) { return x; } void main() { output(second); }",
        );
        assert_eq!(program.globals().len(), 2);
        let main = program.function(ContinuationFunctionId::new(1)).unwrap();
        assert!(main.frame_slots() >= 2);
        let entry = program.continuation(main.entry()).unwrap();
        assert!(
            entry
                .body()
                .iter()
                .any(|instruction| matches!(instruction, FrameInstruction::Input { .. }))
        );
        assert!(matches!(entry.terminator(), Terminator::Call { .. }));
    }

    #[test]
    fn dynamic_load_and_store_split_continuations() {
        let program = lower(
            "cell[4] data; void main() { cell i = input(); data[i] = input(); output(data[i]); }",
        );
        assert!(program.continuations().iter().any(|continuation| matches!(
            continuation.terminator(),
            Terminator::ArrayStore {
                array: ArrayRegion::Global(_),
                ..
            }
        )));
        assert!(program.continuations().iter().any(|continuation| matches!(
            continuation.terminator(),
            Terminator::ArrayLoad {
                array: ArrayRegion::Global(_),
                ..
            }
        )));
    }

    #[test]
    fn dynamic_compound_assignment_evaluates_index_once_then_loads_and_stores() {
        let program = lower("void main() { cell[4] data; data[input()] += input(); }");
        let portal_terms: Vec<_> = program
            .continuations()
            .iter()
            .filter_map(|continuation| match continuation.terminator() {
                Terminator::ArrayLoad { .. } => Some("load"),
                Terminator::ArrayStore { .. } => Some("store"),
                _ => None,
            })
            .collect();
        assert_eq!(portal_terms, ["load", "store"]);
        let input_count = program
            .continuations()
            .iter()
            .flat_map(|continuation| continuation.body())
            .filter(|instruction| matches!(instruction, FrameInstruction::Input { .. }))
            .count();
        assert_eq!(input_count, 2);
    }

    #[test]
    fn rhs_is_lowered_before_dynamic_assignment_index() {
        let program = lower("void main() { cell[4] data; data[input()] = input(); }");
        let main = program.function(ContinuationFunctionId::new(0)).unwrap();
        let entry = program.continuation(main.entry()).unwrap();
        let inputs: Vec<_> = entry
            .body()
            .iter()
            .filter_map(|instruction| match instruction {
                FrameInstruction::Input { dst } => Some(*dst),
                _ => None,
            })
            .collect();
        assert_eq!(inputs.len(), 2);
        let Terminator::ArrayStore { index, value, .. } = entry.terminator() else {
            panic!()
        };
        assert_eq!(inputs[0], *value);
        assert_eq!(inputs[1], *index);
    }

    #[test]
    fn aggregate_calls_use_snapshots_parameters_returns_and_outbox() {
        let program = lower(
            "cell[20] copy(cell[20] value) { cell[20] result; result = value; return result; } \
             void main() { cell[20] source; cell[20] target; target = copy(source); }",
        );
        let main = program.function(ContinuationFunctionId::new(1)).unwrap();
        let copy = program.function(ContinuationFunctionId::new(0)).unwrap();
        assert_eq!(main.outbox_cells(), 20);
        assert!(matches!(
            copy.parameter_locations(),
            [ParameterLocation::Array(_)]
        ));
        assert_eq!(copy.return_type(), ValueType::Array(20));
        assert!(program.continuations().iter().any(|continuation| matches!(
            continuation.terminator(),
            Terminator::Return {
                value: Some(ValueOperand::Array(_))
            }
        )));
    }

    #[test]
    fn aggregate_arguments_are_materialized_left_to_right() {
        let program = lower(
            "cell[2] make(cell x) { cell[2] a; a[0] = x; return a; } \
             void take(cell[2] a, cell[2] b) {} \
             void main() { take(make(input()), make(input())); }",
        );
        let main = program.function(ContinuationFunctionId::new(2)).unwrap();
        assert_eq!(main.outbox_cells(), 2);
        let calls: Vec<_> = program
            .continuations()
            .iter()
            .filter_map(|continuation| match continuation.terminator() {
                Terminator::Call { callee, .. } if continuation.function() == main.id() => {
                    Some(callee.index())
                }
                _ => None,
            })
            .collect();
        assert_eq!(calls, vec![0, 0, 1]);
    }

    #[test]
    fn constant_indices_do_not_use_array_portals() {
        let program = lower("void main() { cell[4] a; a[3] = 7; output(a[3]); }");
        assert!(!program.continuations().iter().any(|continuation| matches!(
            continuation.terminator(),
            Terminator::ArrayLoad { .. } | Terminator::ArrayStore { .. }
        )));
        assert!(
            program
                .continuations()
                .iter()
                .flat_map(|continuation| continuation.body())
                .any(|instruction| matches!(
                    instruction,
                    FrameInstruction::Set {
                        dst: Address::ArrayElement { index: 3, .. },
                        ..
                    }
                ))
        );
    }

    #[test]
    fn array_declarations_clear_every_element_at_their_execution_point() {
        let program = lower(
            "cell[3] global; void main() { cell keep = 1; while (keep) { cell[3] local; local[0] = 9; keep = 0; } }",
        );
        let main = program.function(ContinuationFunctionId::new(0)).unwrap();
        let entry = program.continuation(main.entry()).unwrap();
        let global_clears = entry
            .body()
            .iter()
            .filter(|instruction| {
                matches!(
                    instruction,
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: ArrayRegion::Global(_),
                            ..
                        },
                        value: 0
                    }
                )
            })
            .count();
        assert_eq!(global_clears, 3);

        let local_clear_continuation = program
            .continuations()
            .iter()
            .find(|continuation| {
                continuation
                    .body()
                    .iter()
                    .filter(|instruction| {
                        matches!(
                            instruction,
                            FrameInstruction::Set {
                                dst: Address::ArrayElement {
                                    array: ArrayRegion::Frame(_),
                                    ..
                                },
                                value: 0
                            }
                        )
                    })
                    .count()
                    >= 3
            })
            .expect("loop body must clear the local declaration each time it executes");
        assert!(matches!(
            local_clear_continuation.terminator(),
            Terminator::Goto { .. }
        ));
    }

    #[test]
    fn continuation_id_allocator_checks_the_u16_boundary() {
        let mut last_available = IdAllocator::with_reserved_entries(65_534).unwrap();
        assert_eq!(last_available.allocate().unwrap().get(), u16::MAX);
        assert_eq!(
            last_available.allocate(),
            Err(ContinuationLoweringError::ContinuationIdsExhausted)
        );

        let mut full = IdAllocator::with_reserved_entries(65_535).unwrap();
        assert_eq!(
            full.allocate(),
            Err(ContinuationLoweringError::ContinuationIdsExhausted)
        );
        assert!(matches!(
            IdAllocator::with_reserved_entries(65_536),
            Err(ContinuationLoweringError::ContinuationIdsExhausted)
        ));
    }
}
