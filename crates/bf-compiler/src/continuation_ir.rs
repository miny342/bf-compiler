//! Continuation-oriented IR for functions with frame-relative storage.
//!
//! A [`ContinuationProgram`] separates intra-continuation cell operations from
//! control flow. Calls are terminators, so a backend can switch frames before
//! dispatching the continuation named by the callee's descriptor.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::num::NonZeroU16;

/// The stable identity of a function in a continuation program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(usize);

impl FunctionId {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

/// One cell in the current function's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameSlot(usize);

impl FrameSlot {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

/// A nonzero dispatcher identity.
///
/// Zero is reserved by the ABI for the stopped dispatcher state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContinuationId(NonZeroU16);

impl ContinuationId {
    /// Construct an identity, returning `None` for the reserved value zero.
    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// A source-language value category supported by the scalar ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    Cell,
    Void,
}

/// An address visible to frame-relative instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Address {
    /// A cell owned by the currently executing function frame.
    Frame(FrameSlot),
    /// The scalar value cell in the ABI context.
    AbiValue,
}

/// One destination of a destructive [`FrameInstruction::Transfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTransferTarget {
    pub dst: Address,
    /// The source value is multiplied by this value modulo 256 before being
    /// added to `dst`.
    pub factor: u8,
}

/// A structured operation whose frame addresses are resolved at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameInstruction {
    Set {
        dst: Address,
        value: u8,
    },
    AddConst {
        dst: Address,
        value: u8,
    },
    /// Add `src * factor` to every target, then clear `src`.
    Transfer {
        src: Address,
        targets: Vec<FrameTransferTarget>,
    },
    Input {
        dst: Address,
    },
    Output {
        src: Address,
    },
    /// Execute `body` while `condition` is nonzero.
    Loop {
        condition: Address,
        body: Vec<FrameInstruction>,
    },
    /// Execute one branch and consume `condition`.
    ///
    /// A nonzero condition selects `then_body`; zero selects `else_body`.
    /// The condition is zero before the selected body begins and remains zero
    /// after the instruction completes.
    Branch {
        condition: Address,
        then_body: Vec<FrameInstruction>,
        else_body: Vec<FrameInstruction>,
    },
}

/// Static metadata needed to enter and validate a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDescriptor {
    id: FunctionId,
    parameters: Vec<FrameSlot>,
    frame_slots: usize,
    return_type: ValueType,
    entry: ContinuationId,
}

impl FunctionDescriptor {
    pub fn new(
        id: FunctionId,
        parameters: Vec<FrameSlot>,
        frame_slots: usize,
        return_type: ValueType,
        entry: ContinuationId,
    ) -> Self {
        Self {
            id,
            parameters,
            frame_slots,
            return_type,
            entry,
        }
    }

    pub const fn id(&self) -> FunctionId {
        self.id
    }

    pub fn parameters(&self) -> &[FrameSlot] {
        &self.parameters
    }

    pub const fn frame_slots(&self) -> usize {
        self.frame_slots
    }

    pub const fn return_type(&self) -> ValueType {
        self.return_type
    }

    pub const fn entry(&self) -> ContinuationId {
        self.entry
    }
}

/// Control flow performed after a continuation's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Goto {
        target: ContinuationId,
    },
    /// Select a successor and consume `condition`.
    Branch {
        condition: Address,
        then_target: ContinuationId,
        else_target: ContinuationId,
    },
    /// Call `callee` with already evaluated scalar arguments.
    ///
    /// Arguments retain source evaluation order. A scalar result is returned
    /// through [`Address::AbiValue`] before `return_to` begins.
    Call {
        callee: FunctionId,
        arguments: Vec<Address>,
        return_to: ContinuationId,
    },
    Return {
        value: Option<Address>,
    },
    Halt,
}

/// A straight-line instruction body and its single control-flow terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continuation {
    id: ContinuationId,
    function: FunctionId,
    body: Vec<FrameInstruction>,
    terminator: Terminator,
}

impl Continuation {
    pub fn new(
        id: ContinuationId,
        function: FunctionId,
        body: Vec<FrameInstruction>,
        terminator: Terminator,
    ) -> Self {
        Self {
            id,
            function,
            body,
            terminator,
        }
    }

    pub const fn id(&self) -> ContinuationId {
        self.id
    }

    pub const fn function(&self) -> FunctionId {
        self.function
    }

    pub fn body(&self) -> &[FrameInstruction] {
        &self.body
    }

    pub const fn terminator(&self) -> &Terminator {
        &self.terminator
    }
}

/// A validated collection of functions and dispatcher continuations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationProgram {
    main: FunctionId,
    functions: Vec<FunctionDescriptor>,
    continuations: Vec<Continuation>,
}

impl ContinuationProgram {
    pub fn new(
        main: FunctionId,
        functions: Vec<FunctionDescriptor>,
        continuations: Vec<Continuation>,
    ) -> Result<Self, ContinuationIrError> {
        validate_program(main, &functions, &continuations)?;
        Ok(Self {
            main,
            functions,
            continuations,
        })
    }

    pub const fn main(&self) -> FunctionId {
        self.main
    }

    pub fn functions(&self) -> &[FunctionDescriptor] {
        &self.functions
    }

    pub fn continuations(&self) -> &[Continuation] {
        &self.continuations
    }

    pub fn function(&self, id: FunctionId) -> Option<&FunctionDescriptor> {
        self.functions.iter().find(|function| function.id == id)
    }

    pub fn continuation(&self, id: ContinuationId) -> Option<&Continuation> {
        self.continuations
            .iter()
            .find(|continuation| continuation.id == id)
    }
}

/// A structural error in continuation IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationIrError {
    UnknownMainFunction {
        function: FunctionId,
    },
    MainHasParameters {
        function: FunctionId,
        parameters: usize,
    },
    MainMustReturnVoid {
        function: FunctionId,
        actual: ValueType,
    },
    DuplicateFunctionId {
        function: FunctionId,
    },
    DuplicateContinuationId {
        continuation: ContinuationId,
    },
    ParameterSlotOutOfBounds {
        function: FunctionId,
        slot: FrameSlot,
        frame_slots: usize,
    },
    DuplicateParameterSlot {
        function: FunctionId,
        slot: FrameSlot,
    },
    UnknownFunctionEntry {
        function: FunctionId,
        entry: ContinuationId,
    },
    FunctionEntryOwnedByAnotherFunction {
        function: FunctionId,
        entry: ContinuationId,
        owner: FunctionId,
    },
    UnknownContinuationOwner {
        continuation: ContinuationId,
        function: FunctionId,
    },
    FrameSlotOutOfBounds {
        continuation: ContinuationId,
        slot: FrameSlot,
        frame_slots: usize,
    },
    TransferSourceIsTarget {
        continuation: ContinuationId,
        address: Address,
    },
    DuplicateTransferTarget {
        continuation: ContinuationId,
        address: Address,
    },
    ZeroTransferFactor {
        continuation: ContinuationId,
        address: Address,
    },
    UnknownSuccessor {
        continuation: ContinuationId,
        target: ContinuationId,
    },
    SuccessorOwnedByAnotherFunction {
        continuation: ContinuationId,
        target: ContinuationId,
        owner: FunctionId,
    },
    UnknownCallee {
        continuation: ContinuationId,
        callee: FunctionId,
    },
    CallToMain {
        continuation: ContinuationId,
    },
    CallArgumentCountMismatch {
        continuation: ContinuationId,
        callee: FunctionId,
        expected: usize,
        actual: usize,
    },
    ReturnFromMain {
        continuation: ContinuationId,
    },
    ReturnTypeMismatch {
        continuation: ContinuationId,
        function: FunctionId,
        expected: ValueType,
        has_value: bool,
    },
    HaltOutsideMain {
        continuation: ContinuationId,
        function: FunctionId,
    },
}

impl fmt::Display for ContinuationIrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMainFunction { function } => {
                write!(f, "main function {} does not exist", function.index())
            }
            Self::MainHasParameters {
                function,
                parameters,
            } => write!(
                f,
                "main function {} has {parameters} parameters; expected none",
                function.index()
            ),
            Self::MainMustReturnVoid { function, actual } => write!(
                f,
                "main function {} returns {actual:?}; expected Void",
                function.index()
            ),
            Self::DuplicateFunctionId { function } => {
                write!(f, "function ID {} occurs more than once", function.index())
            }
            Self::DuplicateContinuationId { continuation } => write!(
                f,
                "continuation ID {} occurs more than once",
                continuation.get()
            ),
            Self::ParameterSlotOutOfBounds {
                function,
                slot,
                frame_slots,
            } => write!(
                f,
                "parameter slot {} is outside function {}'s {frame_slots}-slot frame",
                slot.index(),
                function.index()
            ),
            Self::DuplicateParameterSlot { function, slot } => write!(
                f,
                "parameter slot {} occurs more than once in function {}",
                slot.index(),
                function.index()
            ),
            Self::UnknownFunctionEntry { function, entry } => write!(
                f,
                "function {} has unknown entry continuation {}",
                function.index(),
                entry.get()
            ),
            Self::FunctionEntryOwnedByAnotherFunction {
                function,
                entry,
                owner,
            } => write!(
                f,
                "function {} entry continuation {} is owned by function {}",
                function.index(),
                entry.get(),
                owner.index()
            ),
            Self::UnknownContinuationOwner {
                continuation,
                function,
            } => write!(
                f,
                "continuation {} is owned by unknown function {}",
                continuation.get(),
                function.index()
            ),
            Self::FrameSlotOutOfBounds {
                continuation,
                slot,
                frame_slots,
            } => write!(
                f,
                "frame slot {} in continuation {} is outside its {frame_slots}-slot frame",
                slot.index(),
                continuation.get()
            ),
            Self::TransferSourceIsTarget {
                continuation,
                address,
            } => write!(
                f,
                "transfer source {address:?} in continuation {} is also a target",
                continuation.get()
            ),
            Self::DuplicateTransferTarget {
                continuation,
                address,
            } => write!(
                f,
                "transfer target {address:?} occurs more than once in continuation {}",
                continuation.get()
            ),
            Self::ZeroTransferFactor {
                continuation,
                address,
            } => write!(
                f,
                "transfer target {address:?} has a zero factor in continuation {}",
                continuation.get()
            ),
            Self::UnknownSuccessor {
                continuation,
                target,
            } => write!(
                f,
                "continuation {} has unknown successor {}",
                continuation.get(),
                target.get()
            ),
            Self::SuccessorOwnedByAnotherFunction {
                continuation,
                target,
                owner,
            } => write!(
                f,
                "continuation {} has successor {} owned by function {}",
                continuation.get(),
                target.get(),
                owner.index()
            ),
            Self::UnknownCallee {
                continuation,
                callee,
            } => write!(
                f,
                "continuation {} calls unknown function {}",
                continuation.get(),
                callee.index()
            ),
            Self::CallToMain { continuation } => write!(
                f,
                "continuation {} calls main, which is not callable",
                continuation.get()
            ),
            Self::CallArgumentCountMismatch {
                continuation,
                callee,
                expected,
                actual,
            } => write!(
                f,
                "continuation {} calls function {} with {actual} arguments; expected {expected}",
                continuation.get(),
                callee.index()
            ),
            Self::ReturnFromMain { continuation } => write!(
                f,
                "main continuation {} uses return instead of halt",
                continuation.get()
            ),
            Self::ReturnTypeMismatch {
                continuation,
                function,
                expected,
                has_value,
            } => write!(
                f,
                "continuation {} returns {} from function {} with return type {expected:?}",
                continuation.get(),
                if *has_value { "a value" } else { "no value" },
                function.index()
            ),
            Self::HaltOutsideMain {
                continuation,
                function,
            } => write!(
                f,
                "continuation {} halts outside main function {}",
                continuation.get(),
                function.index()
            ),
        }
    }
}

impl Error for ContinuationIrError {}

fn validate_program(
    main: FunctionId,
    functions: &[FunctionDescriptor],
    continuations: &[Continuation],
) -> Result<(), ContinuationIrError> {
    let mut functions_by_id = HashMap::with_capacity(functions.len());
    for function in functions {
        if functions_by_id.insert(function.id, function).is_some() {
            return Err(ContinuationIrError::DuplicateFunctionId {
                function: function.id,
            });
        }

        let mut parameters = HashSet::with_capacity(function.parameters.len());
        for &slot in &function.parameters {
            if slot.index() >= function.frame_slots {
                return Err(ContinuationIrError::ParameterSlotOutOfBounds {
                    function: function.id,
                    slot,
                    frame_slots: function.frame_slots,
                });
            }
            if !parameters.insert(slot) {
                return Err(ContinuationIrError::DuplicateParameterSlot {
                    function: function.id,
                    slot,
                });
            }
        }
    }
    let Some(main_descriptor) = functions_by_id.get(&main).copied() else {
        return Err(ContinuationIrError::UnknownMainFunction { function: main });
    };
    if !main_descriptor.parameters.is_empty() {
        return Err(ContinuationIrError::MainHasParameters {
            function: main,
            parameters: main_descriptor.parameters.len(),
        });
    }
    if main_descriptor.return_type != ValueType::Void {
        return Err(ContinuationIrError::MainMustReturnVoid {
            function: main,
            actual: main_descriptor.return_type,
        });
    }

    let mut continuations_by_id = HashMap::with_capacity(continuations.len());
    for continuation in continuations {
        if continuations_by_id
            .insert(continuation.id, continuation)
            .is_some()
        {
            return Err(ContinuationIrError::DuplicateContinuationId {
                continuation: continuation.id,
            });
        }
    }

    for function in functions {
        let Some(entry) = continuations_by_id.get(&function.entry) else {
            return Err(ContinuationIrError::UnknownFunctionEntry {
                function: function.id,
                entry: function.entry,
            });
        };
        if entry.function != function.id {
            return Err(ContinuationIrError::FunctionEntryOwnedByAnotherFunction {
                function: function.id,
                entry: function.entry,
                owner: entry.function,
            });
        }
    }

    for continuation in continuations {
        let Some(function) = functions_by_id.get(&continuation.function).copied() else {
            return Err(ContinuationIrError::UnknownContinuationOwner {
                continuation: continuation.id,
                function: continuation.function,
            });
        };
        validate_instructions(&continuation.body, continuation.id, function.frame_slots)?;
        validate_terminator(
            continuation,
            function,
            main,
            &functions_by_id,
            &continuations_by_id,
        )?;
    }

    Ok(())
}

fn validate_instructions(
    instructions: &[FrameInstruction],
    continuation: ContinuationId,
    frame_slots: usize,
) -> Result<(), ContinuationIrError> {
    for instruction in instructions {
        match instruction {
            FrameInstruction::Set { dst, .. }
            | FrameInstruction::AddConst { dst, .. }
            | FrameInstruction::Input { dst } => {
                validate_address(*dst, continuation, frame_slots)?;
            }
            FrameInstruction::Output { src } => {
                validate_address(*src, continuation, frame_slots)?;
            }
            FrameInstruction::Transfer { src, targets } => {
                validate_address(*src, continuation, frame_slots)?;
                let mut seen = HashSet::with_capacity(targets.len());
                for target in targets {
                    validate_address(target.dst, continuation, frame_slots)?;
                    if target.dst == *src {
                        return Err(ContinuationIrError::TransferSourceIsTarget {
                            continuation,
                            address: *src,
                        });
                    }
                    if !seen.insert(target.dst) {
                        return Err(ContinuationIrError::DuplicateTransferTarget {
                            continuation,
                            address: target.dst,
                        });
                    }
                    if target.factor == 0 {
                        return Err(ContinuationIrError::ZeroTransferFactor {
                            continuation,
                            address: target.dst,
                        });
                    }
                }
            }
            FrameInstruction::Loop { condition, body } => {
                validate_address(*condition, continuation, frame_slots)?;
                validate_instructions(body, continuation, frame_slots)?;
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                validate_address(*condition, continuation, frame_slots)?;
                validate_instructions(then_body, continuation, frame_slots)?;
                validate_instructions(else_body, continuation, frame_slots)?;
            }
        }
    }
    Ok(())
}

fn validate_terminator(
    continuation: &Continuation,
    function: &FunctionDescriptor,
    main: FunctionId,
    functions: &HashMap<FunctionId, &FunctionDescriptor>,
    continuations: &HashMap<ContinuationId, &Continuation>,
) -> Result<(), ContinuationIrError> {
    match &continuation.terminator {
        Terminator::Goto { target } => validate_successor(continuation, *target, continuations),
        Terminator::Branch {
            condition,
            then_target,
            else_target,
        } => {
            validate_address(*condition, continuation.id, function.frame_slots)?;
            validate_successor(continuation, *then_target, continuations)?;
            validate_successor(continuation, *else_target, continuations)
        }
        Terminator::Call {
            callee,
            arguments,
            return_to,
        } => {
            for &argument in arguments {
                validate_address(argument, continuation.id, function.frame_slots)?;
            }
            validate_successor(continuation, *return_to, continuations)?;
            if *callee == main {
                return Err(ContinuationIrError::CallToMain {
                    continuation: continuation.id,
                });
            }
            let Some(callee_descriptor) = functions.get(callee) else {
                return Err(ContinuationIrError::UnknownCallee {
                    continuation: continuation.id,
                    callee: *callee,
                });
            };
            if arguments.len() != callee_descriptor.parameters.len() {
                return Err(ContinuationIrError::CallArgumentCountMismatch {
                    continuation: continuation.id,
                    callee: *callee,
                    expected: callee_descriptor.parameters.len(),
                    actual: arguments.len(),
                });
            }
            Ok(())
        }
        Terminator::Return { value } => {
            if continuation.function == main {
                return Err(ContinuationIrError::ReturnFromMain {
                    continuation: continuation.id,
                });
            }
            if let Some(value) = value {
                validate_address(*value, continuation.id, function.frame_slots)?;
            }
            let has_value = value.is_some();
            let valid = matches!(
                (function.return_type, has_value),
                (ValueType::Cell, true) | (ValueType::Void, false)
            );
            if !valid {
                return Err(ContinuationIrError::ReturnTypeMismatch {
                    continuation: continuation.id,
                    function: continuation.function,
                    expected: function.return_type,
                    has_value,
                });
            }
            Ok(())
        }
        Terminator::Halt if continuation.function == main => Ok(()),
        Terminator::Halt => Err(ContinuationIrError::HaltOutsideMain {
            continuation: continuation.id,
            function: continuation.function,
        }),
    }
}

fn validate_address(
    address: Address,
    continuation: ContinuationId,
    frame_slots: usize,
) -> Result<(), ContinuationIrError> {
    match address {
        Address::Frame(slot) if slot.index() >= frame_slots => {
            Err(ContinuationIrError::FrameSlotOutOfBounds {
                continuation,
                slot,
                frame_slots,
            })
        }
        Address::Frame(_) | Address::AbiValue => Ok(()),
    }
}

fn validate_successor(
    continuation: &Continuation,
    target: ContinuationId,
    continuations: &HashMap<ContinuationId, &Continuation>,
) -> Result<(), ContinuationIrError> {
    let Some(target_continuation) = continuations.get(&target) else {
        return Err(ContinuationIrError::UnknownSuccessor {
            continuation: continuation.id,
            target,
        });
    };
    if target_continuation.function != continuation.function {
        return Err(ContinuationIrError::SuccessorOwnedByAnotherFunction {
            continuation: continuation.id,
            target,
            owner: target_continuation.function,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(value: u16) -> ContinuationId {
        ContinuationId::new(value).unwrap()
    }

    fn descriptor(
        id: usize,
        parameters: Vec<FrameSlot>,
        frame_slots: usize,
        return_type: ValueType,
        entry: u16,
    ) -> FunctionDescriptor {
        FunctionDescriptor::new(
            FunctionId::new(id),
            parameters,
            frame_slots,
            return_type,
            cid(entry),
        )
    }

    fn continuation(
        id: u16,
        function: usize,
        body: Vec<FrameInstruction>,
        terminator: Terminator,
    ) -> Continuation {
        Continuation::new(cid(id), FunctionId::new(function), body, terminator)
    }

    #[test]
    fn continuation_id_rejects_zero_and_preserves_high_byte() {
        assert_eq!(ContinuationId::new(0), None);
        assert_eq!(cid(0x1234).get(), 0x1234);
    }

    #[test]
    fn validates_main_signature_and_forbids_calls_to_main() {
        let main_with_parameter = descriptor(0, vec![FrameSlot::new(0)], 1, ValueType::Void, 1);
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main_with_parameter],
                vec![continuation(1, 0, vec![], Terminator::Halt)],
            ),
            Err(ContinuationIrError::MainHasParameters {
                function: FunctionId::new(0),
                parameters: 1,
            })
        );

        let value_main = descriptor(0, vec![], 0, ValueType::Cell, 1);
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![value_main],
                vec![continuation(1, 0, vec![], Terminator::Halt)],
            ),
            Err(ContinuationIrError::MainMustReturnVoid {
                function: FunctionId::new(0),
                actual: ValueType::Cell,
            })
        );

        let helper = descriptor(0, vec![], 0, ValueType::Void, 1);
        let main = descriptor(1, vec![], 0, ValueType::Void, 2);
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(1),
                vec![helper, main],
                vec![
                    continuation(
                        1,
                        0,
                        vec![],
                        Terminator::Call {
                            callee: FunctionId::new(1),
                            arguments: vec![],
                            return_to: cid(3),
                        },
                    ),
                    continuation(2, 1, vec![], Terminator::Halt),
                    continuation(3, 0, vec![], Terminator::Return { value: None }),
                ],
            ),
            Err(ContinuationIrError::CallToMain {
                continuation: cid(1),
            })
        );
    }

    #[test]
    fn accepts_calls_and_typed_returns() {
        let main = descriptor(0, vec![], 1, ValueType::Void, 1);
        let callee = descriptor(1, vec![FrameSlot::new(0)], 1, ValueType::Cell, 3);
        let continuations = vec![
            continuation(
                1,
                0,
                vec![],
                Terminator::Call {
                    callee: FunctionId::new(1),
                    arguments: vec![Address::Frame(FrameSlot::new(0))],
                    return_to: cid(2),
                },
            ),
            continuation(2, 0, vec![], Terminator::Halt),
            continuation(
                3,
                1,
                vec![],
                Terminator::Return {
                    value: Some(Address::Frame(FrameSlot::new(0))),
                },
            ),
        ];

        let program =
            ContinuationProgram::new(FunctionId::new(0), vec![main, callee], continuations)
                .unwrap();
        assert_eq!(program.functions().len(), 2);
        assert_eq!(
            program.continuation(cid(3)).unwrap().function(),
            FunctionId::new(1)
        );
    }

    #[test]
    fn rejects_nested_out_of_bounds_frame_address() {
        let main = descriptor(0, vec![], 1, ValueType::Void, 1);
        let body = vec![FrameInstruction::Loop {
            condition: Address::Frame(FrameSlot::new(0)),
            body: vec![FrameInstruction::Output {
                src: Address::Frame(FrameSlot::new(1)),
            }],
        }];

        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main],
                vec![continuation(1, 0, body, Terminator::Halt)],
            ),
            Err(ContinuationIrError::FrameSlotOutOfBounds {
                continuation: cid(1),
                slot: FrameSlot::new(1),
                frame_slots: 1,
            })
        );
    }

    #[test]
    fn rejects_invalid_transfer() {
        let main = descriptor(0, vec![], 1, ValueType::Void, 1);
        let address = Address::AbiValue;
        let body = vec![FrameInstruction::Transfer {
            src: address,
            targets: vec![FrameTransferTarget {
                dst: address,
                factor: 1,
            }],
        }];

        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main],
                vec![continuation(1, 0, body, Terminator::Halt)],
            ),
            Err(ContinuationIrError::TransferSourceIsTarget {
                continuation: cid(1),
                address,
            })
        );
    }

    #[test]
    fn rejects_cross_function_successor() {
        let main = descriptor(0, vec![], 0, ValueType::Void, 1);
        let callee = descriptor(1, vec![], 0, ValueType::Void, 2);

        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main, callee],
                vec![
                    continuation(1, 0, vec![], Terminator::Goto { target: cid(2) }),
                    continuation(2, 1, vec![], Terminator::Return { value: None }),
                ],
            ),
            Err(ContinuationIrError::SuccessorOwnedByAnotherFunction {
                continuation: cid(1),
                target: cid(2),
                owner: FunctionId::new(1),
            })
        );
    }

    #[test]
    fn rejects_call_arity_mismatch() {
        let main = descriptor(0, vec![], 0, ValueType::Void, 1);
        let callee = descriptor(1, vec![FrameSlot::new(0)], 1, ValueType::Void, 3);
        let continuations = vec![
            continuation(
                1,
                0,
                vec![],
                Terminator::Call {
                    callee: FunctionId::new(1),
                    arguments: vec![],
                    return_to: cid(2),
                },
            ),
            continuation(2, 0, vec![], Terminator::Halt),
            continuation(3, 1, vec![], Terminator::Return { value: None }),
        ];

        assert_eq!(
            ContinuationProgram::new(FunctionId::new(0), vec![main, callee], continuations),
            Err(ContinuationIrError::CallArgumentCountMismatch {
                continuation: cid(1),
                callee: FunctionId::new(1),
                expected: 1,
                actual: 0,
            })
        );
    }

    #[test]
    fn enforces_return_and_halt_ownership() {
        let main = descriptor(0, vec![], 0, ValueType::Void, 1);
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main],
                vec![continuation(
                    1,
                    0,
                    vec![],
                    Terminator::Return { value: None }
                )],
            ),
            Err(ContinuationIrError::ReturnFromMain {
                continuation: cid(1),
            })
        );

        let main = descriptor(0, vec![], 0, ValueType::Void, 1);
        let callee = descriptor(1, vec![], 0, ValueType::Void, 2);
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main, callee],
                vec![
                    continuation(1, 0, vec![], Terminator::Halt),
                    continuation(2, 1, vec![], Terminator::Halt),
                ],
            ),
            Err(ContinuationIrError::HaltOutsideMain {
                continuation: cid(2),
                function: FunctionId::new(1),
            })
        );
    }

    #[test]
    fn validates_parameter_slots_and_return_types() {
        let main = descriptor(0, vec![], 0, ValueType::Void, 1);
        let invalid = descriptor(1, vec![FrameSlot::new(1)], 1, ValueType::Cell, 2);
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main, invalid],
                vec![
                    continuation(1, 0, vec![], Terminator::Halt),
                    continuation(
                        2,
                        1,
                        vec![],
                        Terminator::Return {
                            value: Some(Address::AbiValue),
                        },
                    ),
                ],
            ),
            Err(ContinuationIrError::ParameterSlotOutOfBounds {
                function: FunctionId::new(1),
                slot: FrameSlot::new(1),
                frame_slots: 1,
            })
        );

        let main = descriptor(0, vec![], 0, ValueType::Void, 1);
        let callee = descriptor(1, vec![], 0, ValueType::Cell, 2);
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main, callee],
                vec![
                    continuation(1, 0, vec![], Terminator::Halt),
                    continuation(2, 1, vec![], Terminator::Return { value: None }),
                ],
            ),
            Err(ContinuationIrError::ReturnTypeMismatch {
                continuation: cid(2),
                function: FunctionId::new(1),
                expected: ValueType::Cell,
                has_value: false,
            })
        );
    }
}
