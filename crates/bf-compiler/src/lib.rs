//! Compiler components for producing Brainfuck programs.
//!
//! The crate exposes the BFC source frontend, continuation and frame ABI IR,
//! a small destructive cell IR, a Brainfuck-shaped IR, and both backends.

mod abi_codegen;
mod ast;
mod bf_ir;
mod bf_optimizer;
mod codegen;
#[cfg(test)]
mod continuation_adapter;
mod continuation_ir;
mod continuation_lowering;
mod continuation_optimizer;
mod continuation_vm;
mod frame_allocation;
mod frame_layout;
mod frontend;
mod hir;
mod hir_inline;
mod hir_reachability;
mod ir;
mod lexer;
mod macro_expansion;
mod parser;
mod selfhost_cir;
mod selfhost_cir_adapter;
mod semantic;
mod static_layout;

pub use abi_codegen::{
    AbiCodegenError, CompiledProfileArtifact, ProfileGranularity, compile_continuations,
    compile_continuations_unbounded, compile_continuations_unbounded_with_profile,
    compile_continuations_with_profile, lower_continuations, lower_continuations_unbounded,
    lower_continuations_unbounded_with_profile, lower_continuations_with_config,
    lower_continuations_with_profile,
};
pub use bf_ir::{
    AnnotatedBfInstruction, AnnotatedBfOperation, AnnotatedBfProgram, BfInstruction, BfProgram,
    ProfileSiteRecord, ProfileSiteTable,
};
pub use bf_optimizer::{
    BfOptimizationStats, optimize_annotated_bf, optimize_bf, optimize_bf_with_stats,
};
pub use codegen::{CodegenError, compile, lower};
pub use continuation_ir::{
    Address, AggregateRegion, ArrayRegion, Continuation, ContinuationId, ContinuationIrError,
    ContinuationProgram, FrameAggregateDescriptor, FrameAggregateId, FrameArrayDescriptor,
    FrameArrayId, FrameInstruction, FrameSlot, FrameTransferTarget, FunctionDescriptor, FunctionId,
    GlobalDescriptor, GlobalId, LogicalOffset, ParameterLocation, Terminator, ValueOperand,
    ValueType,
};
pub use continuation_optimizer::{
    ContinuationOptimizationOptions, ContinuationOptimizationStats, optimize_continuations,
    optimize_continuations_with_options,
};
pub use continuation_vm::{
    ContinuationRunOptions, ContinuationRunProgress, ContinuationRunStats,
    ContinuationTerminatorKind, ContinuationVmError, run_continuations_with_io,
};
pub use frame_layout::{
    AbiConfig, AbiField, DEFAULT_CHUNK_CELLS, FrameLayout, FrameLayoutError, PROTOCOL_CELLS,
    TAPE_CELLS,
};
pub use frontend::{
    FrontendError, SourceCompileError, SourceFile, SourceLocation, compile_source, compile_sources,
    lower_source, lower_source_with_options, lower_sources, lower_sources_with_options,
};
pub use ir::{CellId, Instruction, IrError, Program, TransferTarget};
pub use selfhost_cir::{
    SelfhostCirArrayOp, SelfhostCirBinaryOp, SelfhostCirCallArgument, SelfhostCirContinuation,
    SelfhostCirError, SelfhostCirFunction, SelfhostCirGlobalOp, SelfhostCirInstruction,
    SelfhostCirParameter, SelfhostCirProgram, SelfhostCirReturnType, SelfhostCirStorage,
    SelfhostCirTerminator, SelfhostCirUnaryOp,
};
pub use selfhost_cir_adapter::{
    SelfhostCirLoweringError, lower_selfhost_cir, lower_selfhost_cir_with_options,
};
pub use static_layout::{StaticLayout, StaticLayoutError};
