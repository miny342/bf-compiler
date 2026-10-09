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
    // Explicit source-only experiment; imported allocated CIR stays untouched.
    // Replace general cloning with closed-callee expansion even when general
    // function inlining is disabled for the comparison.
    let closed = std::env::var("BFC_EVAL_CLOSED_INLINE").unwrap_or_default();
    let lowered = if matches!(closed.as_str(), "1" | "2" | "3" | "4") {
        let max_weight = std::env::var("BFC_EVAL_CLOSED_INLINE_WEIGHT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(128);
        let trial = if closed == "4" {
            crate::cir::inline::inline_wrappers(&lowered, options, max_weight, true)
        } else if closed == "3" {
            crate::cir::inline::inline_forwarding(&lowered, options, max_weight)
        } else {
            crate::cir::inline::inline_closed(&lowered, options, closed == "2", max_weight)
        };
        trial
            .map_err(|detail| ContinuationLoweringError::InvalidHir {
                function: None,
                detail,
            })?
            .0
    } else if options.inline_functions {
        crate::cir::inline::inline_automatic(&lowered, options)
            .map_err(|detail| ContinuationLoweringError::InvalidHir {
                function: None,
                detail,
            })?
            .0
    } else {
        lowered
    };
    let lowered = if std::env::var("BFC_EVAL_ENTRY_PREFIX").as_deref() == Ok("1") {
        let max_arguments = std::env::var("BFC_EVAL_ENTRY_PREFIX_ARGUMENTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8);
        crate::cir::inline::hoist_prefixes(&lowered, max_arguments)
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
