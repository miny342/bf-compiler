//! Compiler components for producing Brainfuck programs.
//!
//! The crate exposes the BFC source frontend, continuation and frame ABI IR,
//! a small destructive cell IR, a Brainfuck-shaped IR, and both backends.

mod abi_codegen;
mod ast;
mod bf_ir;
mod codegen;
#[cfg(test)]
mod continuation_adapter;
mod continuation_ir;
mod continuation_lowering;
mod frame_layout;
mod frontend;
mod hir;
mod ir;
mod lexer;
mod parser;
mod semantic;

pub use abi_codegen::{
    AbiCodegenError, compile_continuations, lower_continuations, lower_continuations_with_config,
};
pub use bf_ir::{BfInstruction, BfProgram};
pub use codegen::{CodegenError, compile, lower};
pub use continuation_ir::{
    Address, ArrayRegion, Continuation, ContinuationId, ContinuationIrError, ContinuationProgram,
    FrameArrayDescriptor, FrameArrayId, FrameInstruction, FrameSlot, FrameTransferTarget,
    FunctionDescriptor, FunctionId, GlobalDescriptor, GlobalId, ParameterLocation, Terminator,
    ValueOperand, ValueType,
};
pub use frame_layout::{
    AbiConfig, AbiField, DEFAULT_CHUNK_CELLS, FrameLayout, FrameLayoutError, PROTOCOL_CELLS,
    TAPE_CELLS,
};
pub use frontend::{FrontendError, SourceCompileError, compile_source, lower_source};
pub use ir::{CellId, Instruction, IrError, Program, TransferTarget};
