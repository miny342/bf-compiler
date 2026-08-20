//! Compiler components for producing Brainfuck programs.
//!
//! The crate exposes the BFC source frontend, a small destructive cell IR, a
//! Brainfuck-shaped IR, and the final Brainfuck backend.

mod ast;
mod bf_ir;
mod codegen;
mod frontend;
mod ir;
mod lexer;
mod parser;

pub use bf_ir::{BfInstruction, BfProgram};
pub use codegen::{CodegenError, compile, lower};
pub use frontend::{FrontendError, SourceCompileError, compile_source, lower_source};
pub use ir::{CellId, Instruction, IrError, Program, TransferTarget};
