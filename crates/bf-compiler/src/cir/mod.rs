//! Continuation IR, graph transformations, storage allocation, and direct execution.

pub(crate) mod analysis;
pub(crate) mod arithmetic_fusion;
pub(crate) mod constant_transfer;
pub(crate) mod effects;
pub(crate) mod frame_allocation;
pub(crate) mod inline;
pub(crate) mod input;
pub(crate) mod ir;
pub(crate) mod operands;
pub(crate) mod optimizer;
pub(crate) mod pipeline;
pub(crate) mod structure;
pub(crate) mod virtual_cleanup;
pub(crate) mod vm;
