//! Compiler components for producing Brainfuck programs.
//!
//! The crate currently exposes a small, destructive intermediate
//! representation and its Brainfuck backend. A source language can be built
//! on top of this without depending on physical tape positions.

mod bf_ir;
mod codegen;
mod ir;

pub use bf_ir::{BfInstruction, BfProgram};
pub use codegen::{CodegenError, compile, lower};
pub use ir::{CellId, Instruction, IrError, Program, TransferTarget};
