//! Lowering from typed HIR to the continuation-oriented scalar ABI IR.

use std::error::Error;
use std::fmt;

use crate::continuation_ir::{
    Address, Continuation, ContinuationId, ContinuationIrError, ContinuationProgram,
    FrameInstruction, FrameSlot, FrameTransferTarget, FunctionDescriptor,
    FunctionId as ContinuationFunctionId, Terminator, ValueType,
};
use crate::hir::{
    self, AssignmentOperator, BinaryOperator, HirExpression, HirExpressionKind, HirFunction,
    HirProgram, HirStatement, HirStatementKind, Type, UnaryOperator,
};

/// Failure to turn typed HIR into validated continuation IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContinuationLoweringError {
    /// The dispatcher has no more nonzero 16-bit identities available.
    ContinuationIdsExhausted,
    /// The input violated an invariant promised by the typed HIR frontend.
    InvalidHir {
        function: Option<hir::FunctionId>,
        detail: &'static str,
    },
    /// The generated program failed the continuation IR validator.
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

/// Lower a typed, name-resolved scalar program to validated continuation IR.
pub(crate) fn lower_hir(
    program: &HirProgram,
) -> Result<ContinuationProgram, ContinuationLoweringError> {
    validate_program_shape(program)?;

    let mut ids = IdAllocator::with_reserved_entries(program.functions.len())?;
    let entries = (0..program.functions.len())
        .map(|index| continuation_id(index + 1))
        .collect::<Result<Vec<_>, _>>()?;

    let mut functions = Vec::with_capacity(program.functions.len());
    let mut continuations = Vec::new();
    for (function_index, function) in program.functions.iter().enumerate() {
        let function_id = ContinuationFunctionId::new(function.id.index());
        let mut lowerer = FunctionLowerer::new(
            program,
            function,
            function_id,
            entries[function_index],
            &mut ids,
        );
        lowerer.lower()?;

        functions.push(FunctionDescriptor::new(
            function_id,
            function
                .parameters
                .iter()
                .map(|parameter| FrameSlot::new(parameter.local.index()))
                .collect(),
            lowerer.next_slot,
            value_type(function.signature.return_type),
            entries[function_index],
        ));
        continuations.extend(lowerer.continuations);
    }

    ContinuationProgram::new(
        ContinuationFunctionId::new(program.entry.index()),
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

fn value_type(ty: Type) -> ValueType {
    match ty {
        Type::Cell => ValueType::Cell,
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

    for (index, function) in program.functions.iter().enumerate() {
        if function.id.index() != index {
            return Err(invalid_hir(
                Some(function.id),
                "function IDs must match vector indices",
            ));
        }
        if function.parameters.len() != function.signature.parameter_types.len() {
            return Err(invalid_hir(
                Some(function.id),
                "parameter list and signature have different lengths",
            ));
        }
        if function
            .signature
            .parameter_types
            .iter()
            .any(|ty| *ty != Type::Cell)
        {
            return Err(invalid_hir(
                Some(function.id),
                "scalar parameters must have cell type",
            ));
        }
        let mut seen = vec![false; function.local_count];
        for parameter in &function.parameters {
            let Some(slot) = seen.get_mut(parameter.local.index()) else {
                return Err(invalid_hir(
                    Some(function.id),
                    "parameter local is out of bounds",
                ));
            };
            if *slot {
                return Err(invalid_hir(
                    Some(function.id),
                    "two parameters use the same local",
                ));
            }
            *slot = true;
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

struct FunctionLowerer<'a, 'ids> {
    program: &'a HirProgram,
    source: &'a HirFunction,
    function: ContinuationFunctionId,
    main: bool,
    ids: &'ids mut IdAllocator,
    current: Option<OpenContinuation>,
    continuations: Vec<Continuation>,
    next_slot: usize,
}

impl<'a, 'ids> FunctionLowerer<'a, 'ids> {
    fn new(
        program: &'a HirProgram,
        source: &'a HirFunction,
        function: ContinuationFunctionId,
        entry: ContinuationId,
        ids: &'ids mut IdAllocator,
    ) -> Self {
        Self {
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
            next_slot: source.local_count,
        }
    }

    fn lower(&mut self) -> Result<(), ContinuationLoweringError> {
        self.lower_statement(&self.source.body)?;
        if self.current.is_some() {
            match (self.main, self.source.signature.return_type) {
                (true, _) => self.finish(Terminator::Halt),
                (false, Type::Void) => self.finish(Terminator::Return { value: None }),
                (false, Type::Cell) => {
                    return Err(invalid_hir(
                        Some(self.source.id),
                        "cell function can reach the end without returning",
                    ));
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
            HirStatementKind::Declaration { local, initializer } => {
                self.validate_local(*local)?;
                let destination = frame(*local);
                if let Some(initializer) = initializer {
                    self.require_cell(initializer)?;
                    let value = self.temporary();
                    self.evaluate(initializer, value)?;
                    self.move_value(value, destination);
                } else {
                    self.emit(FrameInstruction::Set {
                        dst: destination,
                        value: 0,
                    });
                }
            }
            HirStatementKind::Assignment {
                local,
                operator,
                value,
            } => {
                self.validate_local(*local)?;
                self.require_cell(value)?;
                let temporary = self.temporary();
                self.evaluate(value, temporary)?;
                match operator {
                    AssignmentOperator::Set => self.move_value(temporary, frame(*local)),
                    AssignmentOperator::Add => self.transfer(temporary, frame(*local), 1),
                    AssignmentOperator::Subtract => self.transfer(temporary, frame(*local), 255),
                }
            }
            HirStatementKind::Output(expression) => {
                self.require_cell(expression)?;
                let value = self.temporary();
                self.evaluate(expression, value)?;
                self.emit(FrameInstruction::Output { src: value });
            }
            HirStatementKind::Call {
                function,
                arguments,
            } => self.lower_call(*function, arguments, None)?,
            HirStatementKind::Return(value) => {
                self.lower_return(value.as_ref())?;
            }
            HirStatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(condition, then_branch, else_branch.as_deref())?,
            HirStatementKind::While { condition, body } => self.lower_while(condition, body)?,
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
                self.require_cell(value)?;
                let result = self.temporary();
                self.evaluate(value, result)?;
                self.finish(Terminator::Return {
                    value: Some(result),
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

        let condition_value = self.temporary();
        self.evaluate(condition, condition_value)?;

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
        let condition_value = self.temporary();
        self.evaluate(condition, condition_value)?;
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

    fn evaluate(
        &mut self,
        expression: &HirExpression,
        destination: Address,
    ) -> Result<(), ContinuationLoweringError> {
        self.require_cell(expression)?;
        match &expression.kind {
            HirExpressionKind::Literal(value) => self.emit(FrameInstruction::Set {
                dst: destination,
                value: *value,
            }),
            HirExpressionKind::Local(local) => {
                self.validate_local(*local)?;
                self.copy_value(frame(*local), destination);
            }
            HirExpressionKind::Input => self.emit(FrameInstruction::Input { dst: destination }),
            HirExpressionKind::Unary { operator, operand } => match operator {
                UnaryOperator::Plus => self.evaluate(operand, destination)?,
                UnaryOperator::Negate => {
                    let value = self.temporary();
                    self.evaluate(operand, value)?;
                    self.emit(FrameInstruction::Set {
                        dst: destination,
                        value: 0,
                    });
                    self.transfer(value, destination, 255);
                }
                UnaryOperator::Not => {
                    let value = self.temporary();
                    self.evaluate(operand, value)?;
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
            } => self.lower_call(*function, arguments, Some(destination))?,
        }
        Ok(())
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
                self.evaluate(left, destination)?;
                let right_value = self.temporary();
                self.evaluate(right, right_value)?;
                self.transfer(
                    right_value,
                    destination,
                    if operator == BinaryOperator::Add {
                        1
                    } else {
                        255
                    },
                );
            }
            BinaryOperator::Equal | BinaryOperator::NotEqual => {
                let left_value = self.temporary();
                let right_value = self.temporary();
                self.evaluate(left, left_value)?;
                self.evaluate(right, right_value)?;
                self.transfer(right_value, left_value, 255);
                let (nonzero, zero) = if operator == BinaryOperator::Equal {
                    (0, 1)
                } else {
                    (1, 0)
                };
                self.boolean_from(left_value, destination, nonzero, zero);
            }
            BinaryOperator::Less
            | BinaryOperator::LessEqual
            | BinaryOperator::Greater
            | BinaryOperator::GreaterEqual => {
                let left_value = self.temporary();
                let right_value = self.temporary();
                self.evaluate(left, left_value)?;
                self.evaluate(right, right_value)?;
                match operator {
                    BinaryOperator::Less => {
                        self.less_than(left_value, right_value, destination, 1, 0)
                    }
                    BinaryOperator::LessEqual => {
                        self.less_than(right_value, left_value, destination, 0, 1)
                    }
                    BinaryOperator::Greater => {
                        self.less_than(right_value, left_value, destination, 1, 0)
                    }
                    BinaryOperator::GreaterEqual => {
                        self.less_than(left_value, right_value, destination, 0, 1)
                    }
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
        let left_value = self.temporary();
        self.evaluate(left, left_value)?;
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
        let right_value = self.temporary();
        self.evaluate(right, right_value)?;
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
        destination: Option<Address>,
    ) -> Result<(), ContinuationLoweringError> {
        let Some(callee) = self.source_program_function(function) else {
            return Err(invalid_hir(
                Some(self.source.id),
                "call target is out of bounds",
            ));
        };
        let parameter_count = callee.signature.parameter_types.len();
        let return_type = callee.signature.return_type;
        if parameter_count != arguments.len() {
            return Err(invalid_hir(
                Some(self.source.id),
                "call argument count does not match the callee signature",
            ));
        }
        let expects_value = destination.is_some();
        if expects_value != (return_type == Type::Cell) {
            return Err(invalid_hir(
                Some(self.source.id),
                "call use does not match the callee return type",
            ));
        }

        let mut argument_slots = Vec::with_capacity(arguments.len());
        for argument in arguments {
            self.require_cell(argument)?;
            let slot = self.temporary();
            self.evaluate(argument, slot)?;
            argument_slots.push(slot);
        }
        let resume = self.ids.allocate()?;
        self.finish(Terminator::Call {
            callee: ContinuationFunctionId::new(function.index()),
            arguments: argument_slots,
            return_to: resume,
        });
        self.start(resume);
        if let Some(destination) = destination {
            self.move_value(Address::AbiValue, destination);
        }
        Ok(())
    }

    // Set for the duration of `lower_hir`; stored indirectly to keep this
    // builder focused on a single function. This method is replaced by the
    // call-signature table populated in `SIGNATURES` below.
    fn source_program_function(&self, id: hir::FunctionId) -> Option<&HirFunction> {
        self.program
            .functions
            .get(id.index())
            .filter(|function| function.id == id)
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

    fn validate_local(&self, local: hir::LocalId) -> Result<(), ContinuationLoweringError> {
        if local.index() >= self.source.local_count {
            return Err(invalid_hir(
                Some(self.source.id),
                "local ID is out of bounds",
            ));
        }
        Ok(())
    }

    fn temporary(&mut self) -> Address {
        let slot = FrameSlot::new(self.next_slot);
        self.next_slot += 1;
        Address::Frame(slot)
    }

    fn emit(&mut self, instruction: FrameInstruction) {
        self.current
            .as_mut()
            .expect("instructions are only emitted into a live continuation")
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
            .expect("a live continuation is required before a terminator");
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

    fn move_value(&mut self, source: Address, destination: Address) {
        self.emit(FrameInstruction::Set {
            dst: destination,
            value: 0,
        });
        self.transfer(source, destination, 1);
    }

    fn copy_value(&mut self, source: Address, destination: Address) {
        let restore = self.temporary();
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
        let right_test = self.temporary();
        let restore = self.temporary();
        let copy_right = vec![
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
        let mut body = copy_right;
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

fn frame(local: hir::LocalId) -> Address {
    Address::Frame(FrameSlot::new(local.index()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{FunctionSignature, HirParameter, LocalId};

    fn expression(kind: HirExpressionKind) -> HirExpression {
        HirExpression {
            kind,
            ty: Type::Cell,
            offset: 0,
        }
    }

    fn literal(value: u8) -> HirExpression {
        expression(HirExpressionKind::Literal(value))
    }

    fn local(index: usize) -> HirExpression {
        expression(HirExpressionKind::Local(LocalId::new(index)))
    }

    fn call(id: usize, arguments: Vec<HirExpression>) -> HirExpression {
        expression(HirExpressionKind::Call {
            function: hir::FunctionId::new(id),
            arguments,
        })
    }

    fn statement(kind: HirStatementKind) -> HirStatement {
        HirStatement { kind, offset: 0 }
    }

    fn block(statements: Vec<HirStatement>) -> HirStatement {
        statement(HirStatementKind::Block(statements))
    }

    fn function(
        id: usize,
        name: &str,
        parameter_count: usize,
        local_count: usize,
        return_type: Type,
        body: HirStatement,
    ) -> HirFunction {
        HirFunction {
            id: hir::FunctionId::new(id),
            name: name.into(),
            offset: 0,
            signature: FunctionSignature {
                return_type,
                parameter_types: vec![Type::Cell; parameter_count],
            },
            parameters: (0..parameter_count)
                .map(|index| HirParameter {
                    local: LocalId::new(index),
                    offset: 0,
                })
                .collect(),
            local_count,
            body,
        }
    }

    fn main(body: Vec<HirStatement>) -> HirFunction {
        function(0, "main", 0, 0, Type::Void, block(body))
    }

    fn program(functions: Vec<HirFunction>) -> HirProgram {
        HirProgram {
            entry: hir::FunctionId::new(0),
            functions,
        }
    }

    #[test]
    fn scalar_call_splits_and_resume_consumes_abi_value() {
        let source = program(vec![
            main(vec![statement(HirStatementKind::Output(call(
                1,
                vec![literal(7)],
            )))]),
            function(
                1,
                "identity",
                1,
                1,
                Type::Cell,
                statement(HirStatementKind::Return(Some(local(0)))),
            ),
        ]);

        let lowered = lower_hir(&source).unwrap();
        assert_eq!(lowered.functions()[0].entry().get(), 1);
        assert_eq!(lowered.functions()[1].entry().get(), 2);
        assert_eq!(lowered.functions()[0].frame_slots(), 2);

        let entry = lowered
            .continuation(ContinuationId::new(1).unwrap())
            .unwrap();
        let Terminator::Call {
            callee,
            arguments,
            return_to,
        } = entry.terminator()
        else {
            panic!("main entry should end in a call");
        };
        assert_eq!(*callee, ContinuationFunctionId::new(1));
        assert_eq!(arguments, &[Address::Frame(FrameSlot::new(1))]);

        let resume = lowered.continuation(*return_to).unwrap();
        assert!(matches!(
            resume.body(),
            [
                FrameInstruction::Set {
                    dst: Address::Frame(_),
                    value: 0,
                },
                FrameInstruction::Transfer {
                    src: Address::AbiValue,
                    ..
                },
                FrameInstruction::Output { .. }
            ]
        ));
        assert_eq!(resume.terminator(), &Terminator::Halt);
    }

    #[test]
    fn nested_call_arguments_are_emitted_in_left_to_right_order() {
        let constant = |id, name: &str, value| {
            function(
                id,
                name,
                0,
                0,
                Type::Cell,
                statement(HirStatementKind::Return(Some(literal(value)))),
            )
        };
        let source = program(vec![
            main(vec![statement(HirStatementKind::Output(call(
                3,
                vec![call(1, vec![]), call(2, vec![])],
            )))]),
            constant(1, "left", 1),
            constant(2, "right", 2),
            function(
                3,
                "pick",
                2,
                2,
                Type::Cell,
                statement(HirStatementKind::Return(Some(local(0)))),
            ),
        ]);

        let lowered = lower_hir(&source).unwrap();
        let calls = lowered
            .continuations()
            .iter()
            .filter(|continuation| continuation.function() == ContinuationFunctionId::new(0))
            .filter_map(|continuation| match continuation.terminator() {
                Terminator::Call { callee, .. } => Some(callee.index()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls, vec![1, 2, 3]);
    }

    #[test]
    fn if_with_early_returns_needs_no_join_continuation() {
        let choose = function(
            1,
            "choose",
            1,
            1,
            Type::Cell,
            statement(HirStatementKind::If {
                condition: local(0),
                then_branch: Box::new(statement(HirStatementKind::Return(Some(literal(1))))),
                else_branch: Some(Box::new(statement(HirStatementKind::Return(Some(
                    literal(2),
                ))))),
            }),
        );
        let probe = function(
            2,
            "probe",
            1,
            1,
            Type::Cell,
            block(vec![
                statement(HirStatementKind::If {
                    condition: local(0),
                    then_branch: Box::new(statement(HirStatementKind::Empty)),
                    else_branch: None,
                }),
                statement(HirStatementKind::Return(Some(literal(0)))),
            ]),
        );
        let lowered = lower_hir(&program(vec![main(vec![]), choose, probe])).unwrap();
        let choose_continuations = lowered
            .continuations()
            .iter()
            .filter(|continuation| continuation.function() == ContinuationFunctionId::new(1))
            .collect::<Vec<_>>();

        assert_eq!(choose_continuations.len(), 3);
        assert!(matches!(
            choose_continuations[0].terminator(),
            Terminator::Branch { .. }
        ));
        assert!(
            choose_continuations[1..]
                .iter()
                .all(|continuation| matches!(
                    continuation.terminator(),
                    Terminator::Return { value: Some(_) }
                ))
        );

        let probe_entry = lowered
            .continuation(ContinuationId::new(3).unwrap())
            .unwrap();
        assert!(matches!(
            probe_entry.terminator(),
            Terminator::Branch {
                then_target,
                else_target,
                ..
            } if then_target.get() == 6 && else_target.get() == 7
        ));
        assert_eq!(
            lowered
                .continuations()
                .iter()
                .map(|continuation| continuation.id().get())
                .max(),
            Some(8),
            "the both-returning if must not consume an unused join ID",
        );
    }

    #[test]
    fn logical_or_places_rhs_call_only_on_the_zero_branch() {
        let rhs = function(
            1,
            "rhs",
            0,
            0,
            Type::Cell,
            statement(HirStatementKind::Return(Some(literal(1)))),
        );
        let logical = expression(HirExpressionKind::Binary {
            operator: BinaryOperator::LogicalOr,
            left: Box::new(expression(HirExpressionKind::Input)),
            right: Box::new(call(1, vec![])),
        });
        let lowered = lower_hir(&program(vec![
            main(vec![statement(HirStatementKind::Output(logical))]),
            rhs,
        ]))
        .unwrap();
        let main_entry = lowered
            .continuation(ContinuationId::new(1).unwrap())
            .unwrap();
        let Terminator::Branch {
            then_target,
            else_target,
            ..
        } = main_entry.terminator()
        else {
            panic!("logical or must branch after evaluating its left operand");
        };
        assert!(matches!(
            lowered.continuation(*then_target).unwrap().terminator(),
            Terminator::Goto { .. }
        ));
        assert!(matches!(
            lowered.continuation(*else_target).unwrap().terminator(),
            Terminator::Call {
                callee,
                ..
            } if *callee == ContinuationFunctionId::new(1)
        ));
    }

    #[test]
    fn local_reads_copy_and_restore_the_source() {
        let source = program(vec![HirFunction {
            local_count: 1,
            ..main(vec![
                statement(HirStatementKind::Declaration {
                    local: LocalId::new(0),
                    initializer: Some(literal(9)),
                }),
                statement(HirStatementKind::Output(local(0))),
            ])
        }]);
        let lowered = lower_hir(&source).unwrap();
        assert!(lowered.continuations()[0].body().iter().any(|instruction| {
            matches!(
                instruction,
                FrameInstruction::Transfer {
                    src: Address::Frame(slot),
                    targets,
                } if *slot == FrameSlot::new(0) && targets.len() == 2
            )
        }));
    }

    #[test]
    fn rejects_a_cell_function_that_falls_through() {
        let source = program(vec![
            main(vec![]),
            function(
                1,
                "bad",
                0,
                0,
                Type::Cell,
                statement(HirStatementKind::Empty),
            ),
        ]);
        assert!(matches!(
            lower_hir(&source),
            Err(ContinuationLoweringError::InvalidHir {
                function: Some(id),
                ..
            }) if id == hir::FunctionId::new(1)
        ));
    }
}
