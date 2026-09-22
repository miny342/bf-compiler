//! Program-wide graph work on virtual storage, followed by per-function layout.

use crate::{
    ContinuationIrError, ContinuationOptimizationOptions, ContinuationOptimizationStats,
    ContinuationProgram, optimize_continuations_with_options,
};

pub(crate) fn finish(
    program: &ContinuationProgram,
    options: ContinuationOptimizationOptions,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), ContinuationIrError> {
    let (graph, mut stats) = optimize_continuations_with_options(program, options)?;
    let allocated = allocate(&graph, true)?;
    // Frame fusion may empty a body. Thread those edges after allocation, but
    // never duplicate or reconstruct control using the reused physical slots.
    let (cleaned, cleanup) = optimize_continuations_with_options(
        &allocated,
        ContinuationOptimizationOptions {
            inline_branch_successors: false,
            structure_local_control_flow: false,
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

pub(crate) fn allocate(
    program: &ContinuationProgram,
    reuse_slots: bool,
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
        let (descriptor, fused) = crate::frame_fusion::fuse_function(function.clone(), body);
        let (descriptor, allocated) = if reuse_slots {
            crate::frame_allocation::allocate(descriptor, fused)
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
