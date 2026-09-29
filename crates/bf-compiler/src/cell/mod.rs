//! Independent low-level API for programs with statically allocated cells.

pub(crate) mod codegen;
#[cfg(test)]
pub(crate) mod continuation_adapter;
pub(crate) mod ir;
