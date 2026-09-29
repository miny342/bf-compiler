//! Compiler components for producing Brainfuck programs.
//!
//! The crate exposes the BFC source frontend, continuation and frame ABI IR,
//! a small destructive cell IR, a Brainfuck-shaped IR, and both backends.

mod backend;
mod bf;
mod cell;
mod cir;
mod frontend;

pub use backend::codegen::{
    AbiCodegenError, AbiCodegenOptions, CompiledProfileArtifact, ProfileGranularity,
    compile_continuations, compile_continuations_unbounded,
    compile_continuations_unbounded_with_profile, compile_continuations_with_profile,
    lower_continuations, lower_continuations_unbounded, lower_continuations_unbounded_with_profile,
    lower_continuations_with_codegen_options, lower_continuations_with_config,
    lower_continuations_with_profile, lower_continuations_with_profile_and_codegen_options,
};
pub use backend::frame_layout::{
    AbiConfig, AbiField, DEFAULT_CHUNK_CELLS, FrameLayout, FrameLayoutError, PROTOCOL_CELLS,
    TAPE_CELLS,
};
pub use backend::static_layout::{StaticLayout, StaticLayoutError};
pub use bf::ir::{
    AnnotatedBfInstruction, AnnotatedBfOperation, AnnotatedBfProgram, BfInstruction, BfProgram,
    ProfileSiteRecord, ProfileSiteTable,
};
pub use bf::optimizer::{
    BfOptimizationStats, optimize_annotated_bf, optimize_bf, optimize_bf_with_stats,
};
pub use cell::codegen::{CodegenError, compile, lower};
pub use cell::ir::{CellId, Instruction, IrError, Program, TransferTarget};
pub use cir::input::format::{
    SelfhostCirArrayOp, SelfhostCirBinaryOp, SelfhostCirCallArgument, SelfhostCirContinuation,
    SelfhostCirError, SelfhostCirFunction, SelfhostCirGlobalOp, SelfhostCirInstruction,
    SelfhostCirParameter, SelfhostCirProgram, SelfhostCirReturnType, SelfhostCirStorage,
    SelfhostCirTerminator, SelfhostCirUnaryOp,
};
pub use cir::input::lowering::{
    SelfhostCirLoweringError, lower_selfhost_cir, lower_selfhost_cir_with_options,
};
pub use cir::ir::{
    Address, AggregateRegion, ArrayRegion, Continuation, ContinuationId, ContinuationIrError,
    ContinuationProgram, FrameAggregateDescriptor, FrameAggregateId, FrameArrayDescriptor,
    FrameArrayId, FrameInstruction, FrameSlot, FrameTransferTarget, FunctionDescriptor, FunctionId,
    GlobalDescriptor, GlobalId, LogicalOffset, ParameterLocation, SourceFileDescriptor, SourceSpan,
    Terminator, ValueOperand, ValueType,
};
pub use cir::optimizer::{
    ContinuationOptimizationOptions, ContinuationOptimizationStats, optimize_continuations,
    optimize_continuations_with_options,
};
pub use cir::structure::{LocalStructureStats, structure_local_control_flow};
pub use cir::vm::{
    ContinuationPhaseBoundary, ContinuationPhaseConfig, ContinuationRunOptions,
    ContinuationRunProgress, ContinuationRunStats, ContinuationTerminatorKind, ContinuationVmError,
    run_continuations_with_io,
};
pub use frontend::{
    FrontendError, SourceCompileError, SourceFile, SourceLocation, compile_source, compile_sources,
    lower_source, lower_source_with_options, lower_sources, lower_sources_with_options,
};
