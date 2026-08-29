//! Continuation-oriented IR for functions with frame-relative storage.
//!
//! A [`ContinuationProgram`] separates intra-continuation cell operations from
//! control flow. Calls are terminators, so a backend can switch frames before
//! dispatching the continuation named by the callee's descriptor.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::num::NonZeroU16;

/// The 16-bit logical offset can address this many payload cells.
const MAX_AGGREGATE_CELLS: usize = u16::MAX as usize + 1;

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

/// The stable identity of a file-scope object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GlobalId(usize);

impl GlobalId {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

/// The identity of an aligned aggregate region in the current activation frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameAggregateId(usize);

impl FrameAggregateId {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

/// Version-0 compatibility name for a frame aggregate containing `cell[N]`.
pub type FrameArrayId = FrameAggregateId;

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

/// A source-language value category supported by the continuation ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    Cell,
    /// Version-0 fixed-length `cell` array.
    Array(usize),
    /// A Version-1 flattened aggregate payload.
    Aggregate {
        cells: usize,
    },
    Void,
}

/// A statically allocated file-scope object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalDescriptor {
    id: GlobalId,
    value_type: ValueType,
}

impl GlobalDescriptor {
    pub const fn new(id: GlobalId, value_type: ValueType) -> Self {
        Self { id, value_type }
    }

    pub const fn cell(id: GlobalId) -> Self {
        Self::new(id, ValueType::Cell)
    }

    pub const fn array(id: GlobalId, cells: usize) -> Self {
        Self::new(id, ValueType::Array(cells))
    }

    pub const fn aggregate(id: GlobalId, cells: usize) -> Self {
        Self::new(id, ValueType::Aggregate { cells })
    }

    pub const fn id(self) -> GlobalId {
        self.id
    }

    pub const fn value_type(self) -> ValueType {
        self.value_type
    }
}

/// An aligned aggregate region owned by one function activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameAggregateDescriptor {
    id: FrameAggregateId,
    cells: usize,
}

impl FrameAggregateDescriptor {
    pub const fn new(id: FrameAggregateId, cells: usize) -> Self {
        Self { id, cells }
    }

    pub const fn id(self) -> FrameAggregateId {
        self.id
    }

    pub const fn cells(self) -> usize {
        self.cells
    }
}

/// Version-0 compatibility name for an aggregate descriptor.
pub type FrameArrayDescriptor = FrameAggregateDescriptor;

/// An aggregate region visible while the current function is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AggregateRegion {
    Frame(FrameAggregateId),
    Global(GlobalId),
    /// The current activation's caller-owned aggregate result inbox.
    Outbox,
}

/// Version-0 compatibility name for an aggregate region.
pub type ArrayRegion = AggregateRegion;

/// A compiler-owned little-endian 16-bit logical payload offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogicalOffset {
    pub low: Address,
    pub high: Address,
}

impl LogicalOffset {
    pub const fn new(low: Address, high: Address) -> Self {
        Self { low, high }
    }
}

/// An address visible to frame-relative instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Address {
    /// A cell owned by the currently executing function frame.
    Frame(FrameSlot),
    /// A scalar object in static storage.
    Global(GlobalId),
    /// A statically indexed cell within an aggregate region.
    ArrayElement {
        array: AggregateRegion,
        index: usize,
    },
    /// The scalar value cell in the ABI context.
    AbiValue,
}

/// The storage initialized by one function argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParameterLocation {
    Cell(FrameSlot),
    Array(FrameArrayId),
    Aggregate(FrameAggregateId),
}

/// A scalar or aggregate value passed across a continuation boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueOperand {
    Cell(Address),
    /// Version-0 whole-array operand.
    Array(AggregateRegion),
    /// A constant subrange within a flattened aggregate region.
    Aggregate {
        region: AggregateRegion,
        offset: usize,
        cells: usize,
    },
}

impl ValueOperand {
    pub const fn aggregate(region: AggregateRegion, cells: usize) -> Self {
        Self::Aggregate {
            region,
            offset: 0,
            cells,
        }
    }
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
    /// Replace `dst` with `src` while preserving `src`.
    ///
    /// Unlike [`Self::Transfer`], this operation is non-destructive.  Keeping
    /// it explicit lets the ABI backend choose an efficient implementation for
    /// copies that cross the activation-frame/static-storage boundary.
    Copy {
        src: Address,
        dst: Address,
    },
    /// Add `src * factor` to every target, then clear `src`.
    Transfer {
        src: Address,
        targets: Vec<FrameTransferTarget>,
    },
    /// Copy the first `cells` payload cells without copying protocol cells,
    /// chunk heads, or padding.
    AggregateCopy {
        src: AggregateRegion,
        dst: AggregateRegion,
        cells: usize,
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
    parameter_locations: Vec<ParameterLocation>,
    scalar_parameters: Vec<FrameSlot>,
    frame_slots: usize,
    frame_aggregates: Vec<FrameAggregateDescriptor>,
    outbox_cells: usize,
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
        let parameter_locations = parameters
            .iter()
            .copied()
            .map(ParameterLocation::Cell)
            .collect();
        Self {
            id,
            parameter_locations,
            scalar_parameters: parameters,
            frame_slots,
            frame_aggregates: Vec::new(),
            outbox_cells: 0,
            return_type,
            entry,
        }
    }

    /// Construct a descriptor containing typed scalar and aggregate storage.
    #[allow(clippy::too_many_arguments)]
    pub fn new_typed(
        id: FunctionId,
        parameters: Vec<ParameterLocation>,
        frame_slots: usize,
        frame_arrays: Vec<FrameArrayDescriptor>,
        outbox_cells: usize,
        return_type: ValueType,
        entry: ContinuationId,
    ) -> Self {
        let scalar_parameters = parameters
            .iter()
            .filter_map(|parameter| match parameter {
                ParameterLocation::Cell(slot) => Some(*slot),
                ParameterLocation::Array(_) | ParameterLocation::Aggregate(_) => None,
            })
            .collect();
        Self {
            id,
            parameter_locations: parameters,
            scalar_parameters,
            frame_slots,
            frame_aggregates: frame_arrays,
            outbox_cells,
            return_type,
            entry,
        }
    }

    /// Version-1 constructor using aggregate terminology.
    #[allow(clippy::too_many_arguments)]
    pub fn new_aggregates(
        id: FunctionId,
        parameters: Vec<ParameterLocation>,
        frame_slots: usize,
        frame_aggregates: Vec<FrameAggregateDescriptor>,
        outbox_cells: usize,
        return_type: ValueType,
        entry: ContinuationId,
    ) -> Self {
        Self::new_typed(
            id,
            parameters,
            frame_slots,
            frame_aggregates,
            outbox_cells,
            return_type,
            entry,
        )
    }

    pub const fn id(&self) -> FunctionId {
        self.id
    }

    pub fn parameters(&self) -> &[FrameSlot] {
        &self.scalar_parameters
    }

    pub fn parameter_locations(&self) -> &[ParameterLocation] {
        &self.parameter_locations
    }

    pub const fn frame_slots(&self) -> usize {
        self.frame_slots
    }

    pub fn frame_arrays(&self) -> &[FrameArrayDescriptor] {
        &self.frame_aggregates
    }

    pub fn frame_array(&self, id: FrameArrayId) -> Option<&FrameArrayDescriptor> {
        self.frame_aggregates
            .iter()
            .find(|aggregate| aggregate.id == id)
    }

    pub fn frame_aggregates(&self) -> &[FrameAggregateDescriptor] {
        &self.frame_aggregates
    }

    pub fn frame_aggregate(&self, id: FrameAggregateId) -> Option<&FrameAggregateDescriptor> {
        self.frame_aggregates
            .iter()
            .find(|aggregate| aggregate.id == id)
    }

    pub const fn outbox_cells(&self) -> usize {
        self.outbox_cells
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
    /// Call `callee` with already evaluated scalar and aggregate arguments.
    ///
    /// Arguments retain source evaluation order. A scalar result is returned
    /// through [`Address::AbiValue`] before `return_to` begins.
    Call {
        callee: FunctionId,
        arguments: Vec<ValueOperand>,
        return_to: ContinuationId,
    },
    Return {
        value: Option<ValueOperand>,
    },
    /// Load one dynamically selected array element and resume in this frame.
    ArrayLoad {
        array: ArrayRegion,
        index: Address,
        destination: Address,
        return_to: ContinuationId,
    },
    /// Store one dynamically selected array element and resume in this frame.
    ArrayStore {
        array: ArrayRegion,
        index: Address,
        value: Address,
        return_to: ContinuationId,
    },
    /// Load a dynamically selected flattened subobject through an aggregate
    /// portal. `cells` consecutive payload cells are copied atomically from
    /// the runtime logical offset into `destination`.
    AggregateLoad {
        source: AggregateRegion,
        offset: LogicalOffset,
        destination: ValueOperand,
        cells: usize,
        return_to: ContinuationId,
    },
    /// Store a fully evaluated subobject through an aggregate portal.
    AggregateStore {
        destination: AggregateRegion,
        offset: LogicalOffset,
        source: ValueOperand,
        cells: usize,
        return_to: ContinuationId,
    },
    /// Terminate the trampoline immediately from any user function.
    Abort,
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
    globals: Vec<GlobalDescriptor>,
    functions: Vec<FunctionDescriptor>,
    continuations: Vec<Continuation>,
}

impl ContinuationProgram {
    pub fn new(
        main: FunctionId,
        functions: Vec<FunctionDescriptor>,
        continuations: Vec<Continuation>,
    ) -> Result<Self, ContinuationIrError> {
        Self::new_with_globals(main, Vec::new(), functions, continuations)
    }

    pub fn new_with_globals(
        main: FunctionId,
        globals: Vec<GlobalDescriptor>,
        functions: Vec<FunctionDescriptor>,
        continuations: Vec<Continuation>,
    ) -> Result<Self, ContinuationIrError> {
        validate_program(main, &globals, &functions, &continuations)?;
        Ok(Self {
            main,
            globals,
            functions,
            continuations,
        })
    }

    pub const fn main(&self) -> FunctionId {
        self.main
    }

    pub fn globals(&self) -> &[GlobalDescriptor] {
        &self.globals
    }

    pub fn global(&self, id: GlobalId) -> Option<&GlobalDescriptor> {
        self.globals.iter().find(|global| global.id == id)
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
    DuplicateGlobalId {
        global: GlobalId,
    },
    InvalidGlobalType {
        global: GlobalId,
        actual: ValueType,
    },
    InvalidArrayLength {
        cells: usize,
    },
    InvalidAggregateSize {
        cells: usize,
    },
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
    DuplicateFrameArrayId {
        function: FunctionId,
        array: FrameArrayId,
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
    UnknownParameterArray {
        function: FunctionId,
        array: FrameArrayId,
    },
    DuplicateParameterLocation {
        function: FunctionId,
        parameter: ParameterLocation,
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
    UnknownGlobal {
        continuation: ContinuationId,
        global: GlobalId,
    },
    GlobalTypeMismatch {
        continuation: ContinuationId,
        global: GlobalId,
        expected: ValueType,
        actual: ValueType,
    },
    UnknownFrameArray {
        continuation: ContinuationId,
        array: FrameArrayId,
    },
    OutboxUnavailable {
        continuation: ContinuationId,
    },
    ArrayElementOutOfBounds {
        continuation: ContinuationId,
        array: ArrayRegion,
        index: usize,
        cells: usize,
    },
    AggregateCopySizeMismatch {
        continuation: ContinuationId,
        array: ArrayRegion,
        cells: usize,
        available: usize,
    },
    AggregateSubrangeOutOfBounds {
        continuation: ContinuationId,
        region: AggregateRegion,
        offset: usize,
        cells: usize,
        available: usize,
    },
    AggregateAccessHasZeroCells {
        continuation: ContinuationId,
    },
    AggregateAccessTypeMismatch {
        continuation: ContinuationId,
        expected_cells: usize,
        actual: ValueType,
    },
    LogicalOffsetAliases {
        continuation: ContinuationId,
        address: Address,
    },
    LogicalOffsetIsNotFrameOwned {
        continuation: ContinuationId,
        address: Address,
    },
    LogicalOffsetAliasesOperand {
        continuation: ContinuationId,
        address: Address,
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
    CallArgumentTypeMismatch {
        continuation: ContinuationId,
        callee: FunctionId,
        argument: usize,
        expected: ValueType,
        actual: ValueType,
    },
    CallerOutboxTooSmall {
        continuation: ContinuationId,
        required: usize,
        available: usize,
    },
    ArrayPortalRequiresArray {
        continuation: ContinuationId,
        array: ArrayRegion,
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
            Self::DuplicateGlobalId { global } => {
                write!(f, "global ID {} occurs more than once", global.index())
            }
            Self::InvalidGlobalType { global, actual } => {
                write!(f, "global {} has invalid type {actual:?}", global.index())
            }
            Self::InvalidArrayLength { cells } => {
                write!(f, "array length must be between 1 and 256, got {cells}")
            }
            Self::InvalidAggregateSize { cells } => write!(
                f,
                "aggregate payload must contain at most {} cells, got {cells}",
                MAX_AGGREGATE_CELLS
            ),
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
            Self::DuplicateFrameArrayId { function, array } => write!(
                f,
                "frame array ID {} occurs more than once in function {}",
                array.index(),
                function.index()
            ),
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
            Self::UnknownParameterArray { function, array } => write!(
                f,
                "parameter array {} does not exist in function {}",
                array.index(),
                function.index()
            ),
            Self::DuplicateParameterLocation {
                function,
                parameter,
            } => write!(
                f,
                "parameter location {parameter:?} occurs more than once in function {}",
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
            Self::UnknownGlobal {
                continuation,
                global,
            } => write!(
                f,
                "continuation {} references unknown global {}",
                continuation.get(),
                global.index()
            ),
            Self::GlobalTypeMismatch {
                continuation,
                global,
                expected,
                actual,
            } => write!(
                f,
                "continuation {} uses global {} as {expected:?}, but it has type {actual:?}",
                continuation.get(),
                global.index()
            ),
            Self::UnknownFrameArray {
                continuation,
                array,
            } => write!(
                f,
                "continuation {} references unknown frame array {}",
                continuation.get(),
                array.index()
            ),
            Self::OutboxUnavailable { continuation } => write!(
                f,
                "continuation {} references an empty aggregate outbox",
                continuation.get()
            ),
            Self::ArrayElementOutOfBounds {
                continuation,
                array,
                index,
                cells,
            } => write!(
                f,
                "array element {index} of {array:?} in continuation {} is outside its {cells}-cell region",
                continuation.get()
            ),
            Self::AggregateCopySizeMismatch {
                continuation,
                array,
                cells,
                available,
            } => write!(
                f,
                "aggregate copy of {cells} cells in continuation {} exceeds {array:?}'s {available}-cell capacity",
                continuation.get()
            ),
            Self::AggregateSubrangeOutOfBounds {
                continuation,
                region,
                offset,
                cells,
                available,
            } => write!(
                f,
                "aggregate subrange {offset}..{} of {region:?} in continuation {} exceeds its {available}-cell capacity",
                offset.saturating_add(*cells),
                continuation.get()
            ),
            Self::AggregateAccessHasZeroCells { continuation } => write!(
                f,
                "dynamic aggregate access in continuation {} has a zero-cell payload",
                continuation.get()
            ),
            Self::AggregateAccessTypeMismatch {
                continuation,
                expected_cells,
                actual,
            } => write!(
                f,
                "dynamic aggregate access in continuation {} moves {expected_cells} cells but its value has type {actual:?}",
                continuation.get()
            ),
            Self::LogicalOffsetAliases {
                continuation,
                address,
            } => write!(
                f,
                "logical offset bytes in continuation {} alias at {address:?}",
                continuation.get()
            ),
            Self::LogicalOffsetIsNotFrameOwned {
                continuation,
                address,
            } => write!(
                f,
                "logical offset byte {address:?} in continuation {} is not a compiler-owned frame slot",
                continuation.get()
            ),
            Self::LogicalOffsetAliasesOperand {
                continuation,
                address,
            } => write!(
                f,
                "logical offset byte {address:?} in continuation {} aliases the dynamic aggregate value operand",
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
            Self::CallArgumentTypeMismatch {
                continuation,
                callee,
                argument,
                expected,
                actual,
            } => write!(
                f,
                "argument {argument} in continuation {} calls function {} with type {actual:?}; expected {expected:?}",
                continuation.get(),
                callee.index()
            ),
            Self::CallerOutboxTooSmall {
                continuation,
                required,
                available,
            } => write!(
                f,
                "continuation {} calls an aggregate-returning function requiring {required} outbox cells, but only {available} are available",
                continuation.get()
            ),
            Self::ArrayPortalRequiresArray {
                continuation,
                array,
            } => write!(
                f,
                "continuation {} cannot use {array:?} as an array portal",
                continuation.get()
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
    globals: &[GlobalDescriptor],
    functions: &[FunctionDescriptor],
    continuations: &[Continuation],
) -> Result<(), ContinuationIrError> {
    let mut globals_by_id = HashMap::with_capacity(globals.len());
    for global in globals {
        validate_declared_type(global.value_type)?;
        if global.value_type == ValueType::Void {
            return Err(ContinuationIrError::InvalidGlobalType {
                global: global.id,
                actual: global.value_type,
            });
        }
        if globals_by_id.insert(global.id, global).is_some() {
            return Err(ContinuationIrError::DuplicateGlobalId { global: global.id });
        }
    }

    let mut functions_by_id = HashMap::with_capacity(functions.len());
    for function in functions {
        if functions_by_id.insert(function.id, function).is_some() {
            return Err(ContinuationIrError::DuplicateFunctionId {
                function: function.id,
            });
        }

        validate_declared_type(function.return_type)?;
        validate_aggregate_size(function.outbox_cells)?;

        let mut aggregates = HashSet::with_capacity(function.frame_aggregates.len());
        for aggregate in &function.frame_aggregates {
            validate_aggregate_size(aggregate.cells)?;
            if !aggregates.insert(aggregate.id) {
                return Err(ContinuationIrError::DuplicateFrameArrayId {
                    function: function.id,
                    array: aggregate.id,
                });
            }
        }

        let mut parameters = HashSet::with_capacity(function.parameter_locations.len());
        let mut aggregate_parameters = HashSet::with_capacity(function.parameter_locations.len());
        for &parameter in &function.parameter_locations {
            match parameter {
                ParameterLocation::Cell(slot) if slot.index() >= function.frame_slots => {
                    return Err(ContinuationIrError::ParameterSlotOutOfBounds {
                        function: function.id,
                        slot,
                        frame_slots: function.frame_slots,
                    });
                }
                ParameterLocation::Array(array) | ParameterLocation::Aggregate(array)
                    if !aggregates.contains(&array) =>
                {
                    return Err(ContinuationIrError::UnknownParameterArray {
                        function: function.id,
                        array,
                    });
                }
                ParameterLocation::Array(array) | ParameterLocation::Aggregate(array)
                    if !aggregate_parameters.insert(array) =>
                {
                    return Err(ContinuationIrError::DuplicateParameterLocation {
                        function: function.id,
                        parameter,
                    });
                }
                ParameterLocation::Cell(_)
                | ParameterLocation::Array(_)
                | ParameterLocation::Aggregate(_) => {}
            }
            if !parameters.insert(parameter) {
                return Err(match parameter {
                    ParameterLocation::Cell(slot) => ContinuationIrError::DuplicateParameterSlot {
                        function: function.id,
                        slot,
                    },
                    ParameterLocation::Array(_) | ParameterLocation::Aggregate(_) => {
                        ContinuationIrError::DuplicateParameterLocation {
                            function: function.id,
                            parameter,
                        }
                    }
                });
            }
        }
    }
    let Some(main_descriptor) = functions_by_id.get(&main).copied() else {
        return Err(ContinuationIrError::UnknownMainFunction { function: main });
    };
    if !main_descriptor.parameter_locations.is_empty() {
        return Err(ContinuationIrError::MainHasParameters {
            function: main,
            parameters: main_descriptor.parameter_locations.len(),
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
        validate_instructions(
            &continuation.body,
            continuation.id,
            function,
            &globals_by_id,
        )?;
        validate_terminator(
            continuation,
            function,
            main,
            &functions_by_id,
            &continuations_by_id,
            &globals_by_id,
        )?;
    }

    Ok(())
}

fn validate_declared_type(value_type: ValueType) -> Result<(), ContinuationIrError> {
    match value_type {
        ValueType::Array(cells) => validate_array_length(cells)?,
        ValueType::Aggregate { cells } => validate_aggregate_size(cells)?,
        ValueType::Cell | ValueType::Void => {}
    }
    Ok(())
}

fn validate_array_length(cells: usize) -> Result<(), ContinuationIrError> {
    if !(1..=256).contains(&cells) {
        return Err(ContinuationIrError::InvalidArrayLength { cells });
    }
    Ok(())
}

fn validate_aggregate_size(cells: usize) -> Result<(), ContinuationIrError> {
    if cells > MAX_AGGREGATE_CELLS {
        return Err(ContinuationIrError::InvalidAggregateSize { cells });
    }
    Ok(())
}

fn validate_instructions(
    instructions: &[FrameInstruction],
    continuation: ContinuationId,
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<(), ContinuationIrError> {
    for instruction in instructions {
        match instruction {
            FrameInstruction::Set { dst, .. }
            | FrameInstruction::AddConst { dst, .. }
            | FrameInstruction::Input { dst } => {
                validate_address(*dst, continuation, function, globals)?;
            }
            FrameInstruction::Output { src } => {
                validate_address(*src, continuation, function, globals)?;
            }
            FrameInstruction::Copy { src, dst } => {
                validate_address(*src, continuation, function, globals)?;
                validate_address(*dst, continuation, function, globals)?;
            }
            FrameInstruction::Transfer { src, targets } => {
                validate_address(*src, continuation, function, globals)?;
                let mut seen = HashSet::with_capacity(targets.len());
                for target in targets {
                    validate_address(target.dst, continuation, function, globals)?;
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
            FrameInstruction::AggregateCopy { src, dst, cells } => {
                validate_aggregate_size(*cells)?;
                for array in [*src, *dst] {
                    let available = aggregate_region_cells(array, continuation, function, globals)?;
                    if *cells > available {
                        return Err(ContinuationIrError::AggregateCopySizeMismatch {
                            continuation,
                            array,
                            cells: *cells,
                            available,
                        });
                    }
                }
            }
            FrameInstruction::Loop { condition, body } => {
                validate_address(*condition, continuation, function, globals)?;
                validate_instructions(body, continuation, function, globals)?;
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                validate_address(*condition, continuation, function, globals)?;
                validate_instructions(then_body, continuation, function, globals)?;
                validate_instructions(else_body, continuation, function, globals)?;
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
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<(), ContinuationIrError> {
    match &continuation.terminator {
        Terminator::Goto { target } => validate_successor(continuation, *target, continuations),
        Terminator::Branch {
            condition,
            then_target,
            else_target,
        } => {
            validate_address(*condition, continuation.id, function, globals)?;
            validate_successor(continuation, *then_target, continuations)?;
            validate_successor(continuation, *else_target, continuations)
        }
        Terminator::Call {
            callee,
            arguments,
            return_to,
        } => {
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
            if arguments.len() != callee_descriptor.parameter_locations.len() {
                return Err(ContinuationIrError::CallArgumentCountMismatch {
                    continuation: continuation.id,
                    callee: *callee,
                    expected: callee_descriptor.parameter_locations.len(),
                    actual: arguments.len(),
                });
            }
            for (index, (&argument, &parameter)) in arguments
                .iter()
                .zip(&callee_descriptor.parameter_locations)
                .enumerate()
            {
                let actual = value_operand_type(argument, continuation.id, function, globals)?;
                let expected = parameter_type(parameter, callee_descriptor);
                if !value_types_match(actual, expected) {
                    return Err(ContinuationIrError::CallArgumentTypeMismatch {
                        continuation: continuation.id,
                        callee: *callee,
                        argument: index,
                        expected,
                        actual,
                    });
                }
            }
            if let Some(required) = aggregate_type_cells(callee_descriptor.return_type)
                && function.outbox_cells < required
            {
                return Err(ContinuationIrError::CallerOutboxTooSmall {
                    continuation: continuation.id,
                    required,
                    available: function.outbox_cells,
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
                let actual = value_operand_type(*value, continuation.id, function, globals)?;
                if !value_types_match(actual, function.return_type) {
                    return Err(ContinuationIrError::ReturnTypeMismatch {
                        continuation: continuation.id,
                        function: continuation.function,
                        expected: function.return_type,
                        has_value: true,
                    });
                }
            }
            let has_value = value.is_some();
            let valid = (function.return_type == ValueType::Void) != has_value;
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
        Terminator::ArrayLoad {
            array,
            index,
            destination,
            return_to,
        } => {
            validate_portal_array(*array, continuation.id, function, globals)?;
            validate_address(*index, continuation.id, function, globals)?;
            validate_address(*destination, continuation.id, function, globals)?;
            validate_successor(continuation, *return_to, continuations)
        }
        Terminator::ArrayStore {
            array,
            index,
            value,
            return_to,
        } => {
            validate_portal_array(*array, continuation.id, function, globals)?;
            validate_address(*index, continuation.id, function, globals)?;
            validate_address(*value, continuation.id, function, globals)?;
            validate_successor(continuation, *return_to, continuations)
        }
        Terminator::AggregateLoad {
            source,
            offset,
            destination,
            cells,
            return_to,
        } => {
            validate_dynamic_aggregate_region(*source, *cells, continuation.id, function, globals)?;
            validate_logical_offset(*offset, continuation.id, function, globals)?;
            validate_aggregate_access_operand(
                *destination,
                *cells,
                continuation.id,
                function,
                globals,
            )?;
            validate_logical_offset_operand_alias(*offset, *destination, continuation.id)?;
            validate_successor(continuation, *return_to, continuations)
        }
        Terminator::AggregateStore {
            destination,
            offset,
            source,
            cells,
            return_to,
        } => {
            validate_dynamic_aggregate_region(
                *destination,
                *cells,
                continuation.id,
                function,
                globals,
            )?;
            validate_logical_offset(*offset, continuation.id, function, globals)?;
            validate_aggregate_access_operand(*source, *cells, continuation.id, function, globals)?;
            validate_logical_offset_operand_alias(*offset, *source, continuation.id)?;
            validate_successor(continuation, *return_to, continuations)
        }
        Terminator::Abort => Ok(()),
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
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<(), ContinuationIrError> {
    match address {
        Address::Frame(slot) if slot.index() >= function.frame_slots => {
            Err(ContinuationIrError::FrameSlotOutOfBounds {
                continuation,
                slot,
                frame_slots: function.frame_slots,
            })
        }
        Address::Global(global) => {
            let descriptor = global_descriptor(global, continuation, globals)?;
            if descriptor.value_type != ValueType::Cell {
                return Err(ContinuationIrError::GlobalTypeMismatch {
                    continuation,
                    global,
                    expected: ValueType::Cell,
                    actual: descriptor.value_type,
                });
            }
            Ok(())
        }
        Address::ArrayElement { array, index } => {
            let cells = aggregate_region_cells(array, continuation, function, globals)?;
            if index >= cells {
                return Err(ContinuationIrError::ArrayElementOutOfBounds {
                    continuation,
                    array,
                    index,
                    cells,
                });
            }
            Ok(())
        }
        Address::Frame(_) | Address::AbiValue => Ok(()),
    }
}

fn global_descriptor<'a>(
    global: GlobalId,
    continuation: ContinuationId,
    globals: &'a HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<&'a GlobalDescriptor, ContinuationIrError> {
    globals
        .get(&global)
        .copied()
        .ok_or(ContinuationIrError::UnknownGlobal {
            continuation,
            global,
        })
}

fn aggregate_region_cells(
    aggregate: AggregateRegion,
    continuation: ContinuationId,
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<usize, ContinuationIrError> {
    match aggregate {
        AggregateRegion::Frame(aggregate) => function
            .frame_aggregate(aggregate)
            .map(|descriptor| descriptor.cells)
            .ok_or(ContinuationIrError::UnknownFrameArray {
                continuation,
                array: aggregate,
            }),
        AggregateRegion::Global(global) => {
            let descriptor = global_descriptor(global, continuation, globals)?;
            match descriptor.value_type {
                ValueType::Array(cells) | ValueType::Aggregate { cells } => Ok(cells),
                actual => Err(ContinuationIrError::GlobalTypeMismatch {
                    continuation,
                    global,
                    expected: ValueType::Aggregate { cells: 0 },
                    actual,
                }),
            }
        }
        AggregateRegion::Outbox => Ok(function.outbox_cells),
    }
}

fn validate_portal_array(
    array: ArrayRegion,
    continuation: ContinuationId,
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<(), ContinuationIrError> {
    if array == ArrayRegion::Outbox {
        return Err(ContinuationIrError::ArrayPortalRequiresArray {
            continuation,
            array,
        });
    }
    aggregate_region_cells(array, continuation, function, globals)?;
    Ok(())
}

fn value_operand_type(
    value: ValueOperand,
    continuation: ContinuationId,
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<ValueType, ContinuationIrError> {
    match value {
        ValueOperand::Cell(address) => {
            validate_address(address, continuation, function, globals)?;
            Ok(ValueType::Cell)
        }
        ValueOperand::Array(array) => Ok(ValueType::Array(aggregate_region_cells(
            array,
            continuation,
            function,
            globals,
        )?)),
        ValueOperand::Aggregate {
            region,
            offset,
            cells,
        } => {
            validate_aggregate_size(cells)?;
            let available = aggregate_region_cells(region, continuation, function, globals)?;
            let in_bounds = offset
                .checked_add(cells)
                .is_some_and(|end| end <= available);
            if !in_bounds {
                return Err(ContinuationIrError::AggregateSubrangeOutOfBounds {
                    continuation,
                    region,
                    offset,
                    cells,
                    available,
                });
            }
            Ok(ValueType::Aggregate { cells })
        }
    }
}

fn parameter_type(parameter: ParameterLocation, function: &FunctionDescriptor) -> ValueType {
    match parameter {
        ParameterLocation::Cell(_) => ValueType::Cell,
        ParameterLocation::Array(array) => ValueType::Array(
            function
                .frame_array(array)
                .expect("validated parameter array must exist")
                .cells,
        ),
        ParameterLocation::Aggregate(aggregate) => ValueType::Aggregate {
            cells: function
                .frame_aggregate(aggregate)
                .expect("validated parameter aggregate must exist")
                .cells,
        },
    }
}

fn aggregate_type_cells(value_type: ValueType) -> Option<usize> {
    match value_type {
        ValueType::Array(cells) | ValueType::Aggregate { cells } => Some(cells),
        ValueType::Cell | ValueType::Void => None,
    }
}

fn value_types_match(left: ValueType, right: ValueType) -> bool {
    left == right
        || matches!(
            (left, right),
            (ValueType::Array(left), ValueType::Aggregate { cells: right })
                | (ValueType::Aggregate { cells: left }, ValueType::Array(right))
                if left == right
        )
}

fn validate_logical_offset(
    offset: LogicalOffset,
    continuation: ContinuationId,
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<(), ContinuationIrError> {
    validate_address(offset.low, continuation, function, globals)?;
    validate_address(offset.high, continuation, function, globals)?;
    for address in [offset.low, offset.high] {
        if !matches!(address, Address::Frame(_)) {
            return Err(ContinuationIrError::LogicalOffsetIsNotFrameOwned {
                continuation,
                address,
            });
        }
    }
    if offset.low == offset.high {
        return Err(ContinuationIrError::LogicalOffsetAliases {
            continuation,
            address: offset.low,
        });
    }
    Ok(())
}

fn validate_logical_offset_operand_alias(
    offset: LogicalOffset,
    operand: ValueOperand,
    continuation: ContinuationId,
) -> Result<(), ContinuationIrError> {
    let ValueOperand::Cell(address) = operand else {
        return Ok(());
    };
    if address == offset.low || address == offset.high {
        return Err(ContinuationIrError::LogicalOffsetAliasesOperand {
            continuation,
            address,
        });
    }
    Ok(())
}

fn validate_dynamic_aggregate_region(
    region: AggregateRegion,
    cells: usize,
    continuation: ContinuationId,
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<(), ContinuationIrError> {
    if cells == 0 {
        return Err(ContinuationIrError::AggregateAccessHasZeroCells { continuation });
    }
    validate_aggregate_size(cells)?;
    if region == AggregateRegion::Outbox {
        return Err(ContinuationIrError::ArrayPortalRequiresArray {
            continuation,
            array: region,
        });
    }
    let available = aggregate_region_cells(region, continuation, function, globals)?;
    if cells > available {
        return Err(ContinuationIrError::AggregateCopySizeMismatch {
            continuation,
            array: region,
            cells,
            available,
        });
    }
    Ok(())
}

fn validate_aggregate_access_operand(
    operand: ValueOperand,
    cells: usize,
    continuation: ContinuationId,
    function: &FunctionDescriptor,
    globals: &HashMap<GlobalId, &GlobalDescriptor>,
) -> Result<(), ContinuationIrError> {
    let actual = value_operand_type(operand, continuation, function, globals)?;
    let actual_cells = match actual {
        ValueType::Cell => 1,
        ValueType::Array(cells) | ValueType::Aggregate { cells } => cells,
        ValueType::Void => 0,
    };
    if actual_cells != cells {
        return Err(ContinuationIrError::AggregateAccessTypeMismatch {
            continuation,
            expected_cells: cells,
            actual,
        });
    }
    Ok(())
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
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                    return_to: cid(2),
                },
            ),
            continuation(2, 0, vec![], Terminator::Halt),
            continuation(
                3,
                1,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))),
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
    fn validates_copy_addresses() {
        let main = descriptor(0, vec![], 1, ValueType::Void, 1);
        let body = vec![FrameInstruction::Copy {
            src: Address::Frame(FrameSlot::new(0)),
            dst: Address::Frame(FrameSlot::new(1)),
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
                            value: Some(ValueOperand::Cell(Address::AbiValue)),
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

    #[test]
    fn accepts_typed_arrays_globals_portals_and_aggregate_returns() {
        let main = FunctionDescriptor::new_typed(
            FunctionId::new(0),
            vec![],
            2,
            vec![FrameArrayDescriptor::new(FrameArrayId::new(0), 17)],
            17,
            ValueType::Void,
            cid(1),
        );
        let callee = FunctionDescriptor::new_typed(
            FunctionId::new(1),
            vec![ParameterLocation::Array(FrameArrayId::new(0))],
            0,
            vec![FrameArrayDescriptor::new(FrameArrayId::new(0), 17)],
            0,
            ValueType::Array(17),
            cid(3),
        );
        let globals = vec![
            GlobalDescriptor::cell(GlobalId::new(0)),
            GlobalDescriptor::array(GlobalId::new(1), 17),
        ];
        let continuations = vec![
            continuation(
                1,
                0,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Global(GlobalId::new(0)),
                        value: 1,
                    },
                    FrameInstruction::AggregateCopy {
                        src: ArrayRegion::Global(GlobalId::new(1)),
                        dst: ArrayRegion::Frame(FrameArrayId::new(0)),
                        cells: 17,
                    },
                ],
                Terminator::Call {
                    callee: FunctionId::new(1),
                    arguments: vec![ValueOperand::Array(ArrayRegion::Frame(FrameArrayId::new(
                        0,
                    )))],
                    return_to: cid(2),
                },
            ),
            continuation(
                2,
                0,
                vec![],
                Terminator::ArrayStore {
                    array: ArrayRegion::Global(GlobalId::new(1)),
                    index: Address::Frame(FrameSlot::new(0)),
                    value: Address::Frame(FrameSlot::new(1)),
                    return_to: cid(4),
                },
            ),
            continuation(
                3,
                1,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Array(ArrayRegion::Frame(FrameArrayId::new(
                        0,
                    )))),
                },
            ),
            continuation(4, 0, vec![], Terminator::Halt),
        ];

        assert!(
            ContinuationProgram::new_with_globals(
                FunctionId::new(0),
                globals,
                vec![main, callee],
                continuations,
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_array_call_type_and_outbox_mismatches() {
        let main = FunctionDescriptor::new_typed(
            FunctionId::new(0),
            vec![],
            0,
            vec![FrameArrayDescriptor::new(FrameArrayId::new(0), 8)],
            7,
            ValueType::Void,
            cid(1),
        );
        let callee = FunctionDescriptor::new_typed(
            FunctionId::new(1),
            vec![ParameterLocation::Array(FrameArrayId::new(0))],
            0,
            vec![FrameArrayDescriptor::new(FrameArrayId::new(0), 8)],
            0,
            ValueType::Array(8),
            cid(2),
        );
        let result = ContinuationProgram::new(
            FunctionId::new(0),
            vec![main, callee],
            vec![
                continuation(
                    1,
                    0,
                    vec![],
                    Terminator::Call {
                        callee: FunctionId::new(1),
                        arguments: vec![ValueOperand::Array(ArrayRegion::Frame(
                            FrameArrayId::new(0),
                        ))],
                        return_to: cid(3),
                    },
                ),
                continuation(
                    2,
                    1,
                    vec![],
                    Terminator::Return {
                        value: Some(ValueOperand::Array(ArrayRegion::Frame(FrameArrayId::new(
                            0,
                        )))),
                    },
                ),
                continuation(3, 0, vec![], Terminator::Halt),
            ],
        );
        assert_eq!(
            result,
            Err(ContinuationIrError::CallerOutboxTooSmall {
                continuation: cid(1),
                required: 8,
                available: 7,
            })
        );
    }

    #[test]
    fn rejects_invalid_regions_and_array_element_bounds() {
        let main = FunctionDescriptor::new_typed(
            FunctionId::new(0),
            vec![],
            0,
            vec![FrameArrayDescriptor::new(FrameArrayId::new(0), 4)],
            0,
            ValueType::Void,
            cid(1),
        );
        assert_eq!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main],
                vec![continuation(
                    1,
                    0,
                    vec![FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: ArrayRegion::Frame(FrameArrayId::new(0)),
                            index: 4,
                        },
                    }],
                    Terminator::Halt,
                )],
            ),
            Err(ContinuationIrError::ArrayElementOutOfBounds {
                continuation: cid(1),
                array: ArrayRegion::Frame(FrameArrayId::new(0)),
                index: 4,
                cells: 4,
            })
        );
    }

    #[test]
    fn accepts_version_one_aggregate_subranges_and_dynamic_accesses() {
        let payload = FrameAggregateId::new(0);
        let parameter = FrameAggregateId::new(0);
        let main = FunctionDescriptor::new_aggregates(
            FunctionId::new(0),
            vec![],
            2,
            vec![FrameAggregateDescriptor::new(payload, 320)],
            300,
            ValueType::Void,
            cid(1),
        );
        let helper = FunctionDescriptor::new_aggregates(
            FunctionId::new(1),
            vec![ParameterLocation::Aggregate(parameter)],
            0,
            vec![FrameAggregateDescriptor::new(parameter, 300)],
            0,
            ValueType::Aggregate { cells: 300 },
            cid(5),
        );
        let offset = LogicalOffset::new(
            Address::Frame(FrameSlot::new(0)),
            Address::Frame(FrameSlot::new(1)),
        );
        let payload_subrange = ValueOperand::Aggregate {
            region: AggregateRegion::Frame(payload),
            offset: 10,
            cells: 300,
        };
        let continuations = vec![
            continuation(
                1,
                0,
                vec![],
                Terminator::AggregateLoad {
                    source: AggregateRegion::Global(GlobalId::new(0)),
                    offset,
                    destination: payload_subrange,
                    cells: 300,
                    return_to: cid(2),
                },
            ),
            continuation(
                2,
                0,
                vec![],
                Terminator::AggregateStore {
                    destination: AggregateRegion::Global(GlobalId::new(0)),
                    offset,
                    source: payload_subrange,
                    cells: 300,
                    return_to: cid(3),
                },
            ),
            continuation(
                3,
                0,
                vec![],
                Terminator::Call {
                    callee: FunctionId::new(1),
                    arguments: vec![payload_subrange],
                    return_to: cid(4),
                },
            ),
            continuation(4, 0, vec![], Terminator::Abort),
            continuation(
                5,
                1,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::aggregate(
                        AggregateRegion::Frame(parameter),
                        300,
                    )),
                },
            ),
        ];

        let program = ContinuationProgram::new_with_globals(
            FunctionId::new(0),
            vec![GlobalDescriptor::aggregate(GlobalId::new(0), 1024)],
            vec![main, helper],
            continuations,
        )
        .unwrap();
        assert_eq!(
            program.function(FunctionId::new(0)).unwrap().outbox_cells(),
            300
        );
        assert_eq!(
            program.global(GlobalId::new(0)).unwrap().value_type(),
            ValueType::Aggregate { cells: 1024 }
        );
    }

    #[test]
    fn abort_is_valid_in_non_main_functions_without_a_return_value() {
        let main = descriptor(0, vec![], 0, ValueType::Void, 1);
        let helper = FunctionDescriptor::new_aggregates(
            FunctionId::new(1),
            vec![],
            0,
            vec![],
            0,
            ValueType::Aggregate { cells: 512 },
            cid(2),
        );
        assert!(
            ContinuationProgram::new(
                FunctionId::new(0),
                vec![main, helper],
                vec![
                    continuation(1, 0, vec![], Terminator::Halt),
                    continuation(2, 1, vec![], Terminator::Abort),
                ],
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_invalid_dynamic_aggregate_accesses() {
        let aggregate = FrameAggregateId::new(0);
        let descriptor = || {
            FunctionDescriptor::new_aggregates(
                FunctionId::new(0),
                vec![],
                2,
                vec![FrameAggregateDescriptor::new(aggregate, 10)],
                0,
                ValueType::Void,
                cid(1),
            )
        };
        let offset = LogicalOffset::new(
            Address::Frame(FrameSlot::new(0)),
            Address::Frame(FrameSlot::new(1)),
        );

        let zero = ContinuationProgram::new(
            FunctionId::new(0),
            vec![descriptor()],
            vec![
                continuation(
                    1,
                    0,
                    vec![],
                    Terminator::AggregateLoad {
                        source: AggregateRegion::Frame(aggregate),
                        offset,
                        destination: ValueOperand::Aggregate {
                            region: AggregateRegion::Frame(aggregate),
                            offset: 0,
                            cells: 0,
                        },
                        cells: 0,
                        return_to: cid(2),
                    },
                ),
                continuation(2, 0, vec![], Terminator::Halt),
            ],
        );
        assert_eq!(
            zero,
            Err(ContinuationIrError::AggregateAccessHasZeroCells {
                continuation: cid(1),
            })
        );

        let out_of_bounds = ContinuationProgram::new(
            FunctionId::new(0),
            vec![descriptor()],
            vec![
                continuation(
                    1,
                    0,
                    vec![],
                    Terminator::AggregateStore {
                        destination: AggregateRegion::Frame(aggregate),
                        offset,
                        source: ValueOperand::Aggregate {
                            region: AggregateRegion::Frame(aggregate),
                            offset: 8,
                            cells: 3,
                        },
                        cells: 3,
                        return_to: cid(2),
                    },
                ),
                continuation(2, 0, vec![], Terminator::Halt),
            ],
        );
        assert_eq!(
            out_of_bounds,
            Err(ContinuationIrError::AggregateSubrangeOutOfBounds {
                continuation: cid(1),
                region: AggregateRegion::Frame(aggregate),
                offset: 8,
                cells: 3,
                available: 10,
            })
        );

        let aliases = ContinuationProgram::new(
            FunctionId::new(0),
            vec![descriptor()],
            vec![
                continuation(
                    1,
                    0,
                    vec![],
                    Terminator::AggregateLoad {
                        source: AggregateRegion::Frame(aggregate),
                        offset: LogicalOffset::new(offset.low, offset.low),
                        destination: ValueOperand::Cell(Address::Frame(FrameSlot::new(0))),
                        cells: 1,
                        return_to: cid(2),
                    },
                ),
                continuation(2, 0, vec![], Terminator::Halt),
            ],
        );
        assert_eq!(
            aliases,
            Err(ContinuationIrError::LogicalOffsetAliases {
                continuation: cid(1),
                address: offset.low,
            })
        );

        let operand_aliases = ContinuationProgram::new(
            FunctionId::new(0),
            vec![descriptor()],
            vec![
                continuation(
                    1,
                    0,
                    vec![],
                    Terminator::AggregateStore {
                        destination: AggregateRegion::Frame(aggregate),
                        offset,
                        source: ValueOperand::Cell(offset.high),
                        cells: 1,
                        return_to: cid(2),
                    },
                ),
                continuation(2, 0, vec![], Terminator::Halt),
            ],
        );
        assert_eq!(
            operand_aliases,
            Err(ContinuationIrError::LogicalOffsetAliasesOperand {
                continuation: cid(1),
                address: offset.high,
            })
        );
    }

    #[test]
    fn version_zero_array_names_remain_compatible_aliases() {
        let id: FrameArrayId = FrameAggregateId::new(7);
        let descriptor = FrameArrayDescriptor::new(id, 4);
        let region: ArrayRegion = AggregateRegion::Frame(id);
        assert_eq!(descriptor.id(), FrameAggregateId::new(7));
        assert_eq!(
            ValueOperand::Array(region),
            ValueOperand::Array(AggregateRegion::Frame(id))
        );
    }
}
