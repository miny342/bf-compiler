//! Program-wide graph work on virtual storage, followed by per-function layout.

use crate::{
    ContinuationIrError, ContinuationOptimizationOptions, ContinuationOptimizationStats,
    ContinuationProgram, optimize_continuations_with_options,
};

/// Optimize a virtual graph and assign physical slots without inlining calls.
/// Also used by inline cost trials; it must not recursively select inline sites.
/// After allocation, cleanup may thread edges but cannot duplicate instructions
/// whose original virtual identities have been merged into reused slots.
pub(crate) fn optimize_and_allocate(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), ContinuationIrError> {
    optimize_impl(program, options, true)
}

/// Keep inline trials conservative: scalar replacement and bounded carry
/// expansion run after inline sites have been chosen. Crediting their smaller
/// frames or bodies here can expand more calls, increasing generated BF and
/// runtime copies despite the cheaper local estimate.
pub(crate) fn optimize_for_inline_cost(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), ContinuationIrError> {
    optimize_impl(program, options, false)
}

fn optimize_impl(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
    fields: bool,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), ContinuationIrError> {
    let (graph, mut stats) = optimize_continuations_with_options(program, options)?;
    let allocated = allocate_impl(&graph, true, fields)?;
    // Frame fusion may empty a body. Thread those edges after allocation, but
    // never duplicate or reconstruct control using the reused physical slots.
    let (cleaned, cleanup) = optimize_continuations_with_options(
        &allocated,
        ContinuationOptimizationOptions {
            inline_branch_successors: false,
            structure_local_control_flow: false,
            ..Default::default()
        },
    )?;
    stats.continuations_after = cleanup.continuations_after;
    stats.empty_gotos_after = cleanup.empty_gotos_after;
    stats.empty_gotos_threaded += cleanup.empty_gotos_threaded;
    stats.unreachable_continuations_removed += cleanup.unreachable_continuations_removed;
    stats.successor_references_rewritten += cleanup.successor_references_rewritten;
    stats.function_entries_rewritten += cleanup.function_entries_rewritten;
    stats.continuation_ids_compacted |= cleanup.continuation_ids_compacted;
    Ok((cleaned, stats))
}

#[cfg(test)]
pub(crate) fn allocate(
    program: &ContinuationProgram,
    reuse_slots: bool,
) -> Result<ContinuationProgram, ContinuationIrError> {
    allocate_impl(program, reuse_slots, true)
}

fn allocate_impl(
    program: &ContinuationProgram,
    reuse_slots: bool,
    fields: bool,
) -> Result<ContinuationProgram, ContinuationIrError> {
    let mut functions = Vec::with_capacity(program.functions().len());
    let mut continuations = Vec::with_capacity(program.continuations().len());
    for function in program.functions() {
        let body = program
            .continuations()
            .iter()
            .filter(|c| c.function() == function.id())
            .cloned()
            .collect();
        let (descriptor, body) = if fields {
            let (descriptor, body) = crate::cir::scaled_offset::optimize(function.clone(), body);
            crate::cir::aggregate_fields::scalarize(descriptor, body)
        } else {
            (function.clone(), body)
        };
        let (descriptor, fused) = crate::cir::arithmetic_fusion::fuse_function(descriptor, body);
        let fused = crate::cir::constant_transfer::fold_continuations(fused);
        let (descriptor, fused) = crate::cir::computation_graph::optimize(descriptor, fused);
        let (descriptor, fused) = if fields {
            crate::cir::boundary_copies::optimize(descriptor, fused)
        } else {
            (descriptor, fused)
        };
        let (descriptor, fused) = crate::cir::portal_offset::optimize(descriptor, fused);
        let (descriptor, allocated) = if reuse_slots {
            let (descriptor, fused) = crate::cir::value_placement::optimize(descriptor, fused);
            let (descriptor, allocated) = crate::cir::frame_allocation::allocate(descriptor, fused);
            crate::cir::value_placement::finish(descriptor, allocated)
        } else {
            (descriptor, fused)
        };
        functions.push(descriptor);
        continuations.extend(allocated);
    }
    ContinuationProgram::new_with_globals(
        program.main(),
        program.globals().to_vec(),
        functions,
        continuations,
    )
    .map(|p| p.with_source_files(program.source_files().to_vec()))
}
