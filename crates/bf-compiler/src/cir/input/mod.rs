//! External binary CIR input and adaptation to the compiler's continuation IR.
//!
//! The current format is also emitted by the stage-2 selfhost compiler. The
//! existing `SelfhostCir*` public API names are retained for compatibility.

pub(crate) mod format;
pub(crate) mod lowering;
