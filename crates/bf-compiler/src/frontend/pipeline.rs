//! Source compilation order from parsed tokens through allocated continuation IR.
//!
//! All graph expansion happens before slot reuse. External binary CIR has a
//! separate adapter that preserves its already-shared flat storage identities.

use super::hir::{HirProgram, HirSourceFile};
use super::lexer::Token;
use super::lowering::{ContinuationLoweringError, lower_hir_unallocated};
use super::{FrontendError, macro_expansion, parser, semantic};
use crate::{ContinuationOptimizationOptions, ContinuationOptimizationStats, ContinuationProgram};

pub(super) fn lower_tokens(
    tokens: Vec<Token>,
    options: ContinuationOptimizationOptions,
    source_files: Vec<HirSourceFile>,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), FrontendError> {
    let ast = parser::parse(tokens)?;
    let ast = macro_expansion::expand(ast)?;
    let mut hir = semantic::analyze(&ast)?;
    hir.source_files = source_files;
    lower_hir(&hir, options).map_err(|error| FrontendError::without_offset(error.to_string()))
}

/// Lower reachable HIR, optionally inline virtual CIR, then assign frame slots.
pub(crate) fn lower_hir(
    program: &HirProgram,
    options: ContinuationOptimizationOptions,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), ContinuationLoweringError> {
    let lowered = lower_hir_unallocated(program)?;
    let lowered = if options.inline_functions {
        crate::cir::inline::inline_automatic(&lowered, options)
            .map_err(|detail| ContinuationLoweringError::InvalidHir {
                function: None,
                detail,
            })?
            .0
    } else {
        lowered
    };
    crate::cir::pipeline::optimize_and_allocate(&lowered, options).map_err(Into::into)
}
