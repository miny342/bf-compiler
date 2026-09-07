//! Lowering from typed HIR to continuation-oriented ABI IR.

use std::error::Error;
use std::fmt;

use crate::continuation_ir::{
    Address, AggregateRegion, Continuation, ContinuationId, ContinuationIrError,
    ContinuationProgram, FrameAggregateDescriptor, FrameAggregateId, FrameInstruction, FrameSlot,
    FrameTransferTarget, FunctionDescriptor, FunctionId as ContinuationFunctionId,
    GlobalDescriptor, GlobalId as ContinuationGlobalId, LogicalOffset, ParameterLocation,
    Terminator, ValueOperand, ValueType,
};
use crate::hir::{
    self, ArrayIndex, AssignmentOperator, BinaryOperator, HirExpression, HirExpressionKind,
    HirFunction, HirPlace, HirProgram, HirStatement, HirStatementKind, Projection, TypeId,
    TypeKind, UnaryOperator, VariableRef,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContinuationLoweringError {
    ContinuationIdsExhausted,
    InvalidHir {
        function: Option<hir::FunctionId>,
        detail: String,
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
            } => write!(f, "invalid HIR for function {}: {detail}", function.index()),
            Self::InvalidHir {
                function: None,
                detail,
            } => write!(f, "invalid HIR program: {detail}"),
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

#[derive(Debug, Clone, Copy)]
struct LoweredFunction {
    id: ContinuationFunctionId,
    entry: ContinuationId,
}

pub(crate) fn lower_hir(
    program: &HirProgram,
    graph: &hir::CallGraph,
) -> Result<ContinuationProgram, ContinuationLoweringError> {
    validate_program_shape(program)?;

    if graph.reachable.len() != program.functions.len() {
        return Err(invalid_hir(None, "call graph does not match HIR"));
    }

    let live_count = graph.reachable.iter().filter(|&&live| live).count();

    let mut ids = IdAllocator::with_reserved_entries(live_count)?;

    let mut function_map = vec![None; program.functions.len()];
    let mut next_function = 0;

    for f in program.functions.iter() {
        if !graph.reachable[f.id.index()] {
            continue;
        }

        let id = ContinuationFunctionId::new(next_function);
        let entry = continuation_id(next_function + 1)?;

        function_map[f.id.index()] = Some(LoweredFunction { id, entry });

        next_function += 1;
    }

    let globals = program
        .globals
        .iter()
        .map(|global| {
            Ok(GlobalDescriptor::new(
                ContinuationGlobalId::new(global.id.index()),
                value_type(program, global.ty)?,
            ))
        })
        .collect::<Result<Vec<_>, ContinuationLoweringError>>()?;

    let mut functions = Vec::with_capacity(live_count);
    let mut continuations = Vec::new();
    for function in program.functions.iter() {
        let Some(lowered) = function_map[function.id.index()] else {
            continue;
        };

        let mut lowerer =
            FunctionLowerer::new(program, function, lowered.id, lowered.entry, &function_map, &mut ids)?;
        lowerer.lower()?;
        functions.push(lowerer.descriptor(lowered.entry)?);
        continuations.extend(lowerer.continuations);
    }
    let entry = function_map[program.entry.index()].expect("main must be reachable");
    ContinuationProgram::new_with_globals(
        entry.id,
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

fn value_type(program: &HirProgram, ty: TypeId) -> Result<ValueType, ContinuationLoweringError> {
    Ok(match program.types.kind(ty) {
        TypeKind::Cell | TypeKind::Enum { .. } => ValueType::Cell,
        TypeKind::Struct { .. } | TypeKind::Array { .. } => ValueType::Aggregate {
            cells: program.types.cells(ty),
        },
        TypeKind::Void => ValueType::Void,
    })
}

fn invalid_hir(
    function: Option<hir::FunctionId>,
    detail: impl Into<String>,
) -> ContinuationLoweringError {
    ContinuationLoweringError::InvalidHir {
        function,
        detail: detail.into(),
    }
}

fn validate_program_shape(program: &HirProgram) -> Result<(), ContinuationLoweringError> {
    let Some(entry) = program.functions.get(program.entry.index()) else {
        return Err(invalid_hir(None, "entry function is out of bounds"));
    };
    if entry.id != program.entry
        || entry.signature.return_type != TypeId::VOID
        || !entry.parameters.is_empty()
    {
        return Err(invalid_hir(
            Some(entry.id),
            "main must have signature void main()",
        ));
    }
    for (index, global) in program.globals.iter().enumerate() {
        if global.id.index() != index
            || global.ty == TypeId::VOID
            || global
                .initializer
                .as_ref()
                .is_some_and(|value| value.ty != global.ty)
        {
            return Err(invalid_hir(
                None,
                "global descriptor or initializer is invalid",
            ));
        }
    }
    for (index, function) in program.functions.iter().enumerate() {
        if function.id.index() != index
            || function.parameters.len() != function.signature.parameter_types.len()
        {
            return Err(invalid_hir(
                Some(function.id),
                "function descriptor is invalid",
            ));
        }
        for (local_index, local) in function.locals.iter().enumerate() {
            if local.id.index() != local_index || local.ty == TypeId::VOID {
                return Err(invalid_hir(
                    Some(function.id),
                    "local descriptor is invalid",
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
            if local.ty != *expected || seen[parameter.local.index()] {
                return Err(invalid_hir(
                    Some(function.id),
                    "parameter type/identity mismatch",
                ));
            }
            seen[parameter.local.index()] = true;
        }
    }
    Ok(())
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
    Scalar(FrameSlot),
    Aggregate(FrameAggregateId, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AggregateRef {
    region: AggregateRegion,
    offset: usize,
    cells: usize,
}

impl AggregateRef {
    fn operand(self) -> ValueOperand {
        ValueOperand::Aggregate {
            region: self.region,
            offset: self.offset,
            cells: self.cells,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ValueLocation {
    Scalar(Address),
    Aggregate(AggregateRef),
    Dynamic {
        region: AggregateRegion,
        offset: LogicalOffset,
        cells: usize,
    },
    /// Runtime access through a zero-length aggregate. Source behavior is
    /// undefined, but accepting it must not create invalid continuation IR.
    Undefined {
        cells: usize,
    },
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
    frame_aggregates: Vec<FrameAggregateDescriptor>,
    next_slot: usize,
    outbox_cells: usize,
    function_map: &'a [Option<LoweredFunction>],
}

impl<'a, 'ids> FunctionLowerer<'a, 'ids> {
    fn new(
        program: &'a HirProgram,
        source: &'a HirFunction,
        function: ContinuationFunctionId,
        entry: ContinuationId,
        function_map: &'a [Option<LoweredFunction>],
        ids: &'ids mut IdAllocator,
    ) -> Result<Self, ContinuationLoweringError> {
        let mut next_slot = 0;
        let mut frame_aggregates = Vec::new();
        let mut local_storage = Vec::with_capacity(source.locals.len());
        for local in &source.locals {
            if program.types.is_scalar(local.ty) {
                let slot = FrameSlot::new(next_slot);
                next_slot += 1;
                local_storage.push(LocalStorage::Scalar(slot));
            } else if program.types.is_aggregate(local.ty) {
                let id = FrameAggregateId::new(frame_aggregates.len());
                let cells = program.types.cells(local.ty);
                frame_aggregates.push(FrameAggregateDescriptor::new(id, cells));
                local_storage.push(LocalStorage::Aggregate(id, cells));
            } else {
                return Err(invalid_hir(Some(source.id), "void local has no storage"));
            }
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
            frame_aggregates,
            next_slot,
            outbox_cells: 0,
            function_map,
        })
    }

    fn descriptor(
        &self,
        entry: ContinuationId,
    ) -> Result<FunctionDescriptor, ContinuationLoweringError> {
        let parameters = self
            .source
            .parameters
            .iter()
            .map(
                |parameter| match self.local_storage[parameter.local.index()] {
                    LocalStorage::Scalar(slot) => ParameterLocation::Cell(slot),
                    LocalStorage::Aggregate(region, _) => ParameterLocation::Aggregate(region),
                },
            )
            .collect();
        Ok(FunctionDescriptor::new_aggregates(
            self.function,
            parameters,
            self.next_slot,
            self.frame_aggregates.clone(),
            self.outbox_cells,
            value_type(self.program, self.source.signature.return_type)?,
            entry,
        ))
    }

    fn lower(&mut self) -> Result<(), ContinuationLoweringError> {
        if self.main {
            self.lower_global_initializers()?;
        }
        self.lower_statement(&self.source.body)?;
        if self.current.is_some() {
            if self.main {
                self.finish(Terminator::Halt);
            } else if self.source.signature.return_type == TypeId::VOID {
                self.finish(Terminator::Return { value: None });
            } else {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "value function can reach the end without returning",
                ));
            }
        }
        Ok(())
    }

    fn lower_global_initializers(&mut self) -> Result<(), ContinuationLoweringError> {
        for global in &self.program.globals {
            let id = ContinuationGlobalId::new(global.id.index());
            if self.program.types.is_scalar(global.ty) {
                let destination = Address::Global(id);
                self.emit(FrameInstruction::Set {
                    dst: destination,
                    value: 0,
                });
                if let Some(initializer) = &global.initializer {
                    let value = self.evaluate_scalar_temporary(initializer)?;
                    self.move_cell(value, destination);
                }
            } else {
                let cells = self.program.types.cells(global.ty);
                let destination = AggregateRef {
                    region: AggregateRegion::Global(id),
                    offset: 0,
                    cells,
                };
                self.clear_aggregate(destination);
                if let Some(initializer) = &global.initializer {
                    let value = self.evaluate_aggregate(initializer)?;
                    self.copy_aggregate(value, destination);
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
                LocalStorage::Scalar(slot) => {
                    let destination = Address::Frame(slot);
                    self.emit(FrameInstruction::Set {
                        dst: destination,
                        value: 0,
                    });
                    if let Some(initializer) = initializer {
                        let value = self.evaluate_scalar_temporary(initializer)?;
                        self.move_cell(value, destination);
                    }
                }
                LocalStorage::Aggregate(region, cells) => {
                    let destination = AggregateRef {
                        region: AggregateRegion::Frame(region),
                        offset: 0,
                        cells,
                    };
                    self.clear_aggregate(destination);
                    if let Some(initializer) = initializer {
                        let value = self.evaluate_aggregate(initializer)?;
                        self.copy_aggregate(value, destination);
                    }
                }
            },
            HirStatementKind::Assignment {
                value,
                target,
                operator,
            } => self.lower_assignment(value, target, *operator)?,
            HirStatementKind::Output(expression) => {
                let value = self.evaluate_scalar_temporary(expression)?;
                self.emit(FrameInstruction::Output { src: value });
            }
            HirStatementKind::Call {
                function,
                arguments,
            } => self.lower_call(*function, arguments, TypeId::VOID)?,
            HirStatementKind::Abort => self.finish(Terminator::Abort),
            HirStatementKind::Return(value) => self.lower_return(value.as_ref())?,
            HirStatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(condition, then_branch, else_branch.as_deref())?,
            HirStatementKind::While { condition, body } => self.lower_while(condition, body)?,
        }
        Ok(())
    }

    fn lower_assignment(
        &mut self,
        value: &HirExpression,
        target: &HirPlace,
        operator: AssignmentOperator,
    ) -> Result<(), ContinuationLoweringError> {
        // Snapshot the complete RHS before evaluating any dynamic LHS index.
        if self.program.types.is_scalar(value.ty) {
            let rhs = self.evaluate_scalar_temporary(value)?;
            let destination = self.resolve_place(target)?;
            match destination {
                ValueLocation::Scalar(destination) => self.update_cell(rhs, destination, operator),
                ValueLocation::Aggregate(aggregate) if aggregate.cells == 1 => {
                    let destination = self.aggregate_address(aggregate, 0)?;
                    self.update_cell(rhs, destination, operator);
                }
                ValueLocation::Dynamic {
                    region,
                    offset,
                    cells: 1,
                } => {
                    if operator == AssignmentOperator::Set {
                        self.aggregate_store(region, offset, ValueOperand::Cell(rhs), 1)?;
                    } else {
                        let current = self.temporary_cell();
                        self.aggregate_load(region, offset, ValueOperand::Cell(current), 1)?;
                        self.transfer(
                            rhs,
                            current,
                            if operator == AssignmentOperator::Add {
                                1
                            } else {
                                255
                            },
                        );
                        self.aggregate_store(region, offset, ValueOperand::Cell(current), 1)?;
                    }
                }
                ValueLocation::Undefined { .. } => {}
                _ => {
                    return Err(invalid_hir(
                        Some(self.source.id),
                        "invalid scalar assignment target",
                    ));
                }
            }
        } else if self.program.types.is_aggregate(value.ty) && operator == AssignmentOperator::Set {
            let rhs = self.evaluate_aggregate(value)?;
            match self.resolve_place(target)? {
                ValueLocation::Aggregate(destination) => self.copy_aggregate(rhs, destination),
                ValueLocation::Dynamic {
                    region,
                    offset,
                    cells,
                } => self.aggregate_store(region, offset, rhs.operand(), cells)?,
                ValueLocation::Undefined { .. } => {}
                ValueLocation::Scalar(_) => {
                    return Err(invalid_hir(
                        Some(self.source.id),
                        "invalid aggregate assignment target",
                    ));
                }
            }
        } else {
            return Err(invalid_hir(
                Some(self.source.id),
                "assignment type/operator is invalid",
            ));
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

    fn lower_return(
        &mut self,
        value: Option<&HirExpression>,
    ) -> Result<(), ContinuationLoweringError> {
        let return_type = self.source.signature.return_type;
        if return_type == TypeId::VOID && value.is_none() {
            self.finish(if self.main {
                Terminator::Halt
            } else {
                Terminator::Return { value: None }
            });
        } else if !self.main && self.program.types.is_scalar(return_type) {
            let value =
                value.ok_or_else(|| invalid_hir(Some(self.source.id), "missing return value"))?;
            let result = self.evaluate_scalar_temporary(value)?;
            self.finish(Terminator::Return {
                value: Some(ValueOperand::Cell(result)),
            });
        } else if !self.main && self.program.types.is_aggregate(return_type) {
            let value =
                value.ok_or_else(|| invalid_hir(Some(self.source.id), "missing return value"))?;
            let result = self.evaluate_aggregate(value)?;
            self.finish(Terminator::Return {
                value: Some(result.operand()),
            });
        } else {
            return Err(invalid_hir(
                Some(self.source.id),
                "return value/type mismatch",
            ));
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
        let condition_value = self.evaluate_scalar_temporary(condition)?;
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
        let condition_value = self.evaluate_scalar_temporary(condition)?;
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

    fn evaluate_scalar(
        &mut self,
        expression: &HirExpression,
        destination: Address,
    ) -> Result<(), ContinuationLoweringError> {
        if !self.program.types.is_scalar(expression.ty) {
            return Err(invalid_hir(
                Some(self.source.id),
                "expected scalar expression",
            ));
        }
        match &expression.kind {
            HirExpressionKind::Literal(value) | HirExpressionKind::EnumVariant(value) => {
                self.emit(FrameInstruction::Set {
                    dst: destination,
                    value: *value,
                });
            }
            HirExpressionKind::Place(place) => {
                let location = self.resolve_place(place)?;
                self.load_scalar_location(location, destination)?;
            }
            HirExpressionKind::Project { base, projections } => {
                let base_type = base.ty;
                let base = self.evaluate_aggregate(base)?;
                let location =
                    self.resolve_region_projections(base.region, base_type, projections, 1)?;
                self.load_scalar_location(location, destination)?;
            }
            HirExpressionKind::Input => self.emit(FrameInstruction::Input { dst: destination }),
            HirExpressionKind::Unary { operator, operand } => match operator {
                UnaryOperator::Plus => self.evaluate_scalar(operand, destination)?,
                UnaryOperator::Negate => {
                    let value = self.evaluate_scalar_temporary(operand)?;
                    self.emit(FrameInstruction::Set {
                        dst: destination,
                        value: 0,
                    });
                    self.transfer(value, destination, 255);
                }
                UnaryOperator::Not => {
                    let value = self.evaluate_scalar_temporary(operand)?;
                    self.boolean_from(value, destination, 0, 1);
                }
            },
            HirExpressionKind::Binary {
                operator,
                left,
                right,
            } => self.evaluate_binary(*operator, left, right, destination)?,
            HirExpressionKind::Call {
                function,
                arguments,
            } => {
                self.lower_call(*function, arguments, expression.ty)?;
                self.move_cell(Address::AbiValue, destination);
            }
            HirExpressionKind::StringLiteral(_) => {
                return Err(invalid_hir(Some(self.source.id), "string is not scalar"));
            }
        }
        Ok(())
    }

    fn load_scalar_location(
        &mut self,
        location: ValueLocation,
        destination: Address,
    ) -> Result<(), ContinuationLoweringError> {
        match location {
            ValueLocation::Scalar(source) => self.copy_cell(source, destination),
            ValueLocation::Aggregate(source) if source.cells == 1 => {
                let source = self.aggregate_address(source, 0)?;
                self.copy_cell(source, destination);
            }
            ValueLocation::Dynamic {
                region,
                offset,
                cells: 1,
            } => self.aggregate_load(region, offset, ValueOperand::Cell(destination), 1)?,
            ValueLocation::Undefined { cells: 1 } => self.emit(FrameInstruction::Set {
                dst: destination,
                value: 0,
            }),
            _ => {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "scalar projection has invalid size",
                ));
            }
        }
        Ok(())
    }

    fn evaluate_aggregate(
        &mut self,
        expression: &HirExpression,
    ) -> Result<AggregateRef, ContinuationLoweringError> {
        if !self.program.types.is_aggregate(expression.ty) {
            return Err(invalid_hir(
                Some(self.source.id),
                "expected aggregate expression",
            ));
        }
        let cells = self.program.types.cells(expression.ty);
        let snapshot = self.temporary_aggregate(cells);
        match &expression.kind {
            HirExpressionKind::StringLiteral(bytes) => {
                self.clear_aggregate(snapshot);
                for (index, value) in bytes.iter().copied().enumerate() {
                    self.emit(FrameInstruction::Set {
                        dst: self.aggregate_address(snapshot, index)?,
                        value,
                    });
                }
            }
            HirExpressionKind::Place(place) => match self.resolve_place(place)? {
                ValueLocation::Aggregate(source) => self.copy_aggregate(source, snapshot),
                ValueLocation::Dynamic {
                    region,
                    offset,
                    cells,
                } => self.aggregate_load(region, offset, snapshot.operand(), cells)?,
                ValueLocation::Undefined { .. } => self.clear_aggregate(snapshot),
                ValueLocation::Scalar(_) => {
                    return Err(invalid_hir(
                        Some(self.source.id),
                        "aggregate place resolved to scalar",
                    ));
                }
            },
            HirExpressionKind::Project { base, projections } => {
                let base_type = base.ty;
                let base = self.evaluate_aggregate(base)?;
                match self.resolve_region_projections(base.region, base_type, projections, cells)? {
                    ValueLocation::Aggregate(source) => self.copy_aggregate(source, snapshot),
                    ValueLocation::Dynamic {
                        region,
                        offset,
                        cells,
                    } => self.aggregate_load(region, offset, snapshot.operand(), cells)?,
                    ValueLocation::Undefined { .. } => self.clear_aggregate(snapshot),
                    ValueLocation::Scalar(_) => {
                        return Err(invalid_hir(
                            Some(self.source.id),
                            "aggregate projection resolved to scalar",
                        ));
                    }
                }
            }
            HirExpressionKind::Call {
                function,
                arguments,
            } => {
                self.lower_call(*function, arguments, expression.ty)?;
                self.copy_aggregate(
                    AggregateRef {
                        region: AggregateRegion::Outbox,
                        offset: 0,
                        cells,
                    },
                    snapshot,
                );
            }
            _ => {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "unsupported aggregate expression",
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
        match operator {
            BinaryOperator::Add | BinaryOperator::Subtract => {
                self.evaluate_scalar(left, destination)?;
                let right = self.evaluate_scalar_temporary(right)?;
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
                let left = self.evaluate_scalar_temporary(left)?;
                let right = self.evaluate_scalar_temporary(right)?;
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
                let left = self.evaluate_scalar_temporary(left)?;
                let right = self.evaluate_scalar_temporary(right)?;
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
        let left_value = self.evaluate_scalar_temporary(left)?;
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
        let right_value = self.evaluate_scalar_temporary(right)?;
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
        expected_return: TypeId,
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
            return Err(invalid_hir(Some(self.source.id), "call signature mismatch"));
        }
        let mut operands = Vec::with_capacity(arguments.len());
        for (argument, parameter_type) in arguments.iter().zip(&callee.signature.parameter_types) {
            if argument.ty != *parameter_type {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "call argument type mismatch",
                ));
            }
            if self.program.types.is_scalar(*parameter_type) {
                operands.push(ValueOperand::Cell(
                    self.evaluate_scalar_temporary(argument)?,
                ));
            } else {
                operands.push(self.evaluate_aggregate(argument)?.operand());
            }
        }
        if self.program.types.is_aggregate(expected_return) {
            self.outbox_cells = self
                .outbox_cells
                .max(self.program.types.cells(expected_return));
        }
        let resume = self.ids.allocate()?;
        let lowered_callee = self
            .function_map
            .get(function.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                invalid_hir(
                    Some(self.source.id),
                    "reachable function calls eliminated function",
                )
            })?;
        self.finish(Terminator::Call {
            callee: lowered_callee.id,
            arguments: operands,
            return_to: resume,
        });
        self.start(resume);
        Ok(())
    }

    fn resolve_place(
        &mut self,
        place: &HirPlace,
    ) -> Result<ValueLocation, ContinuationLoweringError> {
        let root_type = self.variable_type(place.root)?;
        if self.program.types.is_scalar(root_type) {
            if !place.projections.is_empty() {
                return Err(invalid_hir(
                    Some(self.source.id),
                    "scalar root has projections",
                ));
            }
            return Ok(ValueLocation::Scalar(self.scalar_variable(place.root)?));
        }
        let region = self.aggregate_variable(place.root, self.program.types.cells(root_type))?;
        self.resolve_region_projections(
            region,
            root_type,
            &place.projections,
            self.program.types.cells(place.ty),
        )
    }

    fn resolve_region_projections(
        &mut self,
        region: AggregateRegion,
        _root_type: TypeId,
        projections: &[Projection],
        result_cells: usize,
    ) -> Result<ValueLocation, ContinuationLoweringError> {
        let root_cells = self.program.types.cells(_root_type);
        let mut constant_offset = 0usize;
        let mut dynamic = Vec::new();
        for projection in projections {
            match projection {
                Projection::Field { cell_offset } => {
                    constant_offset =
                        constant_offset.checked_add(*cell_offset).ok_or_else(|| {
                            invalid_hir(Some(self.source.id), "projection offset overflows")
                        })?;
                }
                Projection::Index {
                    index,
                    length,
                    element_cells,
                } => match index {
                    ArrayIndex::Constant(index) => {
                        let contribution = usize::from(*index)
                            .checked_mul(*element_cells)
                            .ok_or_else(|| {
                                invalid_hir(Some(self.source.id), "projection stride overflows")
                            })?;
                        constant_offset =
                            constant_offset.checked_add(contribution).ok_or_else(|| {
                                invalid_hir(Some(self.source.id), "projection offset overflows")
                            })?;
                    }
                    ArrayIndex::Dynamic(index) => {
                        dynamic.push((index.as_ref(), *length, *element_cells));
                    }
                },
            }
        }
        if dynamic.is_empty() {
            return Ok(ValueLocation::Aggregate(AggregateRef {
                region,
                offset: constant_offset,
                cells: result_cells,
            }));
        }
        let low = self.temporary_cell();
        let high = self.temporary_cell();
        self.emit(FrameInstruction::Set {
            dst: low,
            value: constant_offset as u8,
        });
        self.emit(FrameInstruction::Set {
            dst: high,
            value: ((constant_offset >> 8) & 0xff) as u8,
        });
        let mut maximum_low = constant_offset & 0xff;
        for (index, length, stride) in dynamic {
            let index = self.evaluate_scalar_temporary(index)?;
            let low_add = stride as u8;
            // Out-of-bounds indexing is source-level undefined behavior, so a
            // valid index is bounded by the declared length.  If that range
            // cannot wrap the low offset byte, one transfer can multiply and
            // add both stride bytes without a runtime carry branch.
            let maximum_index = length.saturating_sub(1);
            let maximum_add = maximum_index.checked_mul(usize::from(low_add));
            let next_maximum_low = maximum_add
                .and_then(|maximum_add| maximum_low.checked_add(maximum_add))
                .filter(|maximum| *maximum <= usize::from(u8::MAX));
            if let Some(next_maximum_low) = next_maximum_low {
                self.add_scaled_offset_without_low_carry(index, stride, low, high);
                maximum_low = next_maximum_low;
            } else {
                self.add_scaled_offset(index, stride, low, high);
                maximum_low = usize::from(u8::MAX);
            }
        }
        if root_cells == 0 {
            return Ok(ValueLocation::Undefined {
                cells: result_cells,
            });
        }
        Ok(ValueLocation::Dynamic {
            region,
            offset: LogicalOffset::new(low, high),
            cells: result_cells,
        })
    }

    fn add_scaled_offset(&mut self, counter: Address, stride: usize, low: Address, high: Address) {
        if stride == 0 {
            self.emit(FrameInstruction::Transfer {
                src: counter,
                targets: vec![],
            });
            return;
        }
        let low_add = stride as u8;
        let high_add = ((stride >> 8) & 0xff) as u8;
        let mut body = Vec::new();
        if low_add != 0 {
            let compare_left = self.temporary_cell();
            let compare_right = self.temporary_cell();
            let right_test = self.temporary_cell();
            let restore = self.temporary_cell();
            let carry = self.temporary_cell();
            body.extend(copy_instructions(low, compare_left, restore));
            body.push(FrameInstruction::Set {
                dst: compare_right,
                value: 0_u8.wrapping_sub(low_add),
            });
            body.extend(less_than_instructions(
                compare_left,
                compare_right,
                carry,
                right_test,
                restore,
                0,
                1,
            ));
            body.push(FrameInstruction::AddConst {
                dst: low,
                value: low_add,
            });
            body.push(FrameInstruction::Branch {
                condition: carry,
                then_body: vec![FrameInstruction::AddConst {
                    dst: high,
                    value: 1,
                }],
                else_body: vec![],
            });
        }
        if high_add != 0 {
            body.push(FrameInstruction::AddConst {
                dst: high,
                value: high_add,
            });
        }
        body.push(FrameInstruction::AddConst {
            dst: counter,
            value: 255,
        });
        self.emit(FrameInstruction::Loop {
            condition: counter,
            body,
        });
    }

    fn add_scaled_offset_without_low_carry(
        &mut self,
        counter: Address,
        stride: usize,
        low: Address,
        high: Address,
    ) {
        let mut targets = Vec::with_capacity(2);
        let low_add = stride as u8;
        if low_add != 0 {
            targets.push(FrameTransferTarget {
                dst: low,
                factor: low_add,
            });
        }
        let high_add = ((stride >> 8) & 0xff) as u8;
        if high_add != 0 {
            targets.push(FrameTransferTarget {
                dst: high,
                factor: high_add,
            });
        }
        self.emit(FrameInstruction::Transfer {
            src: counter,
            targets,
        });
    }

    fn aggregate_load(
        &mut self,
        source: AggregateRegion,
        offset: LogicalOffset,
        destination: ValueOperand,
        cells: usize,
    ) -> Result<(), ContinuationLoweringError> {
        if cells == 0 {
            return Ok(());
        }
        let resume = self.ids.allocate()?;
        self.finish(Terminator::AggregateLoad {
            source,
            offset,
            destination,
            cells,
            return_to: resume,
        });
        self.start(resume);
        Ok(())
    }

    fn aggregate_store(
        &mut self,
        destination: AggregateRegion,
        offset: LogicalOffset,
        source: ValueOperand,
        cells: usize,
    ) -> Result<(), ContinuationLoweringError> {
        if cells == 0 {
            return Ok(());
        }
        let resume = self.ids.allocate()?;
        self.finish(Terminator::AggregateStore {
            destination,
            offset,
            source,
            cells,
            return_to: resume,
        });
        self.start(resume);
        Ok(())
    }

    fn variable_type(&self, variable: VariableRef) -> Result<TypeId, ContinuationLoweringError> {
        match variable {
            VariableRef::Global(global) => self
                .program
                .globals
                .get(global.index())
                .filter(|item| item.id == global)
                .map(|item| item.ty)
                .ok_or_else(|| invalid_hir(Some(self.source.id), "global reference is invalid")),
            VariableRef::Local(local) => self
                .source
                .locals
                .get(local.index())
                .filter(|item| item.id == local)
                .map(|item| item.ty)
                .ok_or_else(|| invalid_hir(Some(self.source.id), "local reference is invalid")),
        }
    }

    fn scalar_variable(&self, variable: VariableRef) -> Result<Address, ContinuationLoweringError> {
        match variable {
            VariableRef::Global(global)
                if self.program.types.is_scalar(self.variable_type(variable)?) =>
            {
                Ok(Address::Global(ContinuationGlobalId::new(global.index())))
            }
            VariableRef::Local(local) => match self.local(local)? {
                LocalStorage::Scalar(slot) => Ok(Address::Frame(slot)),
                LocalStorage::Aggregate(_, _) => Err(invalid_hir(
                    Some(self.source.id),
                    "aggregate local used as scalar",
                )),
            },
            _ => Err(invalid_hir(
                Some(self.source.id),
                "aggregate global used as scalar",
            )),
        }
    }

    fn aggregate_variable(
        &self,
        variable: VariableRef,
        cells: usize,
    ) -> Result<AggregateRegion, ContinuationLoweringError> {
        match variable {
            VariableRef::Global(global)
                if self
                    .program
                    .types
                    .is_aggregate(self.variable_type(variable)?)
                    && self.program.types.cells(self.variable_type(variable)?) == cells =>
            {
                Ok(AggregateRegion::Global(ContinuationGlobalId::new(
                    global.index(),
                )))
            }
            VariableRef::Local(local) => match self.local(local)? {
                LocalStorage::Aggregate(region, actual) if actual == cells => {
                    Ok(AggregateRegion::Frame(region))
                }
                _ => Err(invalid_hir(
                    Some(self.source.id),
                    "local aggregate size mismatch",
                )),
            },
            _ => Err(invalid_hir(
                Some(self.source.id),
                "global aggregate size mismatch",
            )),
        }
    }

    fn local(&self, local: hir::LocalId) -> Result<LocalStorage, ContinuationLoweringError> {
        self.local_storage
            .get(local.index())
            .copied()
            .ok_or_else(|| invalid_hir(Some(self.source.id), "local ID is out of bounds"))
    }

    fn require_cell(&self, expression: &HirExpression) -> Result<(), ContinuationLoweringError> {
        if expression.ty == TypeId::CELL {
            Ok(())
        } else {
            Err(invalid_hir(
                Some(self.source.id),
                "expression does not have cell type",
            ))
        }
    }

    fn temporary_cell(&mut self) -> Address {
        let slot = FrameSlot::new(self.next_slot);
        self.next_slot += 1;
        Address::Frame(slot)
    }

    fn evaluate_scalar_temporary(
        &mut self,
        expression: &HirExpression,
    ) -> Result<Address, ContinuationLoweringError> {
        let destination = self.temporary_cell();
        self.evaluate_scalar(expression, destination)?;
        Ok(destination)
    }

    fn temporary_aggregate(&mut self, cells: usize) -> AggregateRef {
        let id = FrameAggregateId::new(self.frame_aggregates.len());
        self.frame_aggregates
            .push(FrameAggregateDescriptor::new(id, cells));
        AggregateRef {
            region: AggregateRegion::Frame(id),
            offset: 0,
            cells,
        }
    }

    fn aggregate_address(
        &self,
        aggregate: AggregateRef,
        relative: usize,
    ) -> Result<Address, ContinuationLoweringError> {
        if relative >= aggregate.cells {
            return Err(invalid_hir(
                Some(self.source.id),
                "aggregate address is out of bounds",
            ));
        }
        Ok(Address::ArrayElement {
            array: aggregate.region,
            index: aggregate.offset + relative,
        })
    }

    fn clear_aggregate(&mut self, aggregate: AggregateRef) {
        for index in 0..aggregate.cells {
            self.emit(FrameInstruction::Set {
                dst: Address::ArrayElement {
                    array: aggregate.region,
                    index: aggregate.offset + index,
                },
                value: 0,
            });
        }
    }

    fn copy_aggregate(&mut self, source: AggregateRef, destination: AggregateRef) {
        debug_assert_eq!(source.cells, destination.cells);
        if source.cells == 0 || source == destination {
            return;
        }
        if source.offset == 0 && destination.offset == 0 {
            self.emit(FrameInstruction::AggregateCopy {
                src: source.region,
                dst: destination.region,
                cells: source.cells,
            });
            return;
        }
        // Callers snapshot before an observable store, so leaf copies cannot
        // alias even when the source-language roots overlap.
        for index in 0..source.cells {
            let source_address = Address::ArrayElement {
                array: source.region,
                index: source.offset + index,
            };
            let destination_address = Address::ArrayElement {
                array: destination.region,
                index: destination.offset + index,
            };
            self.copy_cell(source_address, destination_address);
        }
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
        self.current = Some(OpenContinuation { id, body: vec![] });
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
        self.emit(FrameInstruction::Copy {
            src: source,
            dst: destination,
        });
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
        for instruction in less_than_instructions(
            left,
            right,
            destination,
            right_test,
            restore,
            true_value,
            false_value,
        ) {
            self.emit(instruction);
        }
    }
}

fn copy_instructions(
    source: Address,
    destination: Address,
    restore: Address,
) -> Vec<FrameInstruction> {
    vec![
        FrameInstruction::Set {
            dst: destination,
            value: 0,
        },
        FrameInstruction::Transfer {
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
        },
        FrameInstruction::Transfer {
            src: restore,
            targets: vec![FrameTransferTarget {
                dst: source,
                factor: 1,
            }],
        },
    ]
}

#[allow(clippy::too_many_arguments)]
fn less_than_instructions(
    left: Address,
    right: Address,
    destination: Address,
    right_test: Address,
    restore: Address,
    true_value: u8,
    false_value: u8,
) -> Vec<FrameInstruction> {
    let body = vec![
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
        FrameInstruction::Branch {
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
                targets: vec![],
            }],
        },
    ];
    vec![
        FrameInstruction::Loop {
            condition: left,
            body,
        },
        FrameInstruction::Branch {
            condition: right,
            then_body: vec![FrameInstruction::Set {
                dst: destination,
                value: true_value,
            }],
            else_body: vec![FrameInstruction::Set {
                dst: destination,
                value: false_value,
            }],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser, semantic, hir};

    fn lower(source: &str) -> ContinuationProgram {
        let ast = parser::parse(lexer::lex(source).unwrap()).unwrap();
        let hir = semantic::analyze(&ast).unwrap();
        let graph = hir::CallGraph::build(&hir);
        lower_hir(&hir, &graph).unwrap()
    }

    fn contains_branch(instructions: &[FrameInstruction]) -> bool {
        instructions.iter().any(|instruction| match instruction {
            FrameInstruction::Loop { body, .. } => contains_branch(body),
            FrameInstruction::Branch { .. } => true,
            _ => false,
        })
    }

    #[test]
    fn scalar_copies_remain_nondestructive_copy_instructions() {
        let program = lower("cell source; void main() { cell destination = source; }");
        assert!(program.continuations().iter().any(|continuation| {
            continuation.body().iter().any(|instruction| {
                matches!(
                    instruction,
                    FrameInstruction::Copy {
                        src: Address::Global(_),
                        dst: Address::Frame(_),
                    }
                )
            })
        }));
    }

    #[test]
    fn lowers_nominal_aggregates_and_dynamic_flat_offsets() {
        let program = lower(
            "struct Node { cell kind; cell[2] data; } Node[100] nodes; \
             void main() { nodes[input()].data[input()] = 9; }",
        );
        assert!(program.continuations().iter().any(|continuation| matches!(
            continuation.terminator(),
            Terminator::AggregateStore { cells: 1, .. }
        )));
        assert_eq!(
            program.globals()[0].value_type(),
            ValueType::Aggregate { cells: 300 }
        );
    }

    #[test]
    fn dynamic_flat_offsets_skip_carry_branches_when_layout_proves_they_cannot_wrap() {
        let no_carry = lower(
            "cell[16][256] arena; \
             void main() { arena[input()][input()] = 9; }",
        );
        let store = no_carry
            .continuations()
            .iter()
            .find(|continuation| {
                matches!(continuation.terminator(), Terminator::AggregateStore { .. })
            })
            .unwrap();
        assert!(!contains_branch(store.body()));

        let carry = lower(
            "struct Padded { cell prefix; cell[256] data; } Padded value; \
             void main() { value.data[input()] = 9; }",
        );
        let store = carry
            .continuations()
            .iter()
            .find(|continuation| {
                matches!(continuation.terminator(), Terminator::AggregateStore { .. })
            })
            .unwrap();
        assert!(contains_branch(store.body()));
    }

    #[test]
    fn strings_become_exact_aggregate_initializers() {
        let program = lower("void main() { cell[] value = \"a\\0b\"; output(value[1]); }");
        let main = program.function(ContinuationFunctionId::new(0)).unwrap();
        assert!(
            main.frame_aggregates()
                .iter()
                .any(|aggregate| aggregate.cells() == 3)
        );
    }

    #[test]
    fn abort_is_a_distinct_terminator() {
        let program = lower("void stop() { abort(); } void main() { stop(); }");
        assert!(
            program
                .continuations()
                .iter()
                .any(|continuation| matches!(continuation.terminator(), Terminator::Abort))
        );
    }

    #[test]
    fn continuation_id_allocator_checks_the_u16_boundary() {
        let mut last_available = IdAllocator::with_reserved_entries(65_534).unwrap();
        assert_eq!(last_available.allocate().unwrap().get(), u16::MAX);
        assert_eq!(
            last_available.allocate(),
            Err(ContinuationLoweringError::ContinuationIdsExhausted)
        );
    }
}
