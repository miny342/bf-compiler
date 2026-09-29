//! Shared frame-layout planning and cost estimation, independent of BF emission.

use super::frame_layout::{AbiConfig, FrameLayout, FrameLayoutError};
use super::regions::{RegionPlan, maximum_branch_depth};
use crate::{AggregateRegion, ContinuationProgram, FrameSlot, FunctionId, Terminator};
use std::collections::HashMap;

pub(crate) const GLOBAL_ROUTE_NIBBLE_CELLS: usize = 16;

#[derive(Debug)]
pub(crate) struct FunctionLayout {
    pub(crate) frame: FrameLayout,
    pub(crate) branch_temporary_start: usize,
    pub(crate) region_selector: Option<FrameSlot>,
    pub(crate) portal_temporary_start: usize,
    pub(crate) portal_temporary_cells: usize,
}

#[cfg(test)]
pub(crate) fn build_layouts(
    program: &ContinuationProgram,
    config: AbiConfig,
) -> Result<HashMap<FunctionId, FunctionLayout>, FrameLayoutError> {
    build_layouts_with_regions(program, config, None)
}

pub(crate) fn build_layouts_with_regions(
    program: &ContinuationProgram,
    config: AbiConfig,
    regions: Option<&RegionPlan>,
) -> Result<HashMap<FunctionId, FunctionLayout>, FrameLayoutError> {
    build_layouts_with_route(program, config, regions, has_global_portal(program))
}

pub(crate) fn has_global_portal(program: &ContinuationProgram) -> bool {
    program.continuations().iter().any(|continuation| {
        matches!(
            continuation.terminator(),
            Terminator::ArrayLoad {
                array: AggregateRegion::Global(_),
                ..
            } | Terminator::ArrayStore {
                array: AggregateRegion::Global(_),
                ..
            } | Terminator::AggregateLoad {
                source: AggregateRegion::Global(_),
                ..
            } | Terminator::AggregateStore {
                destination: AggregateRegion::Global(_),
                ..
            }
        )
    })
}

pub(crate) fn build_layouts_with_route(
    program: &ContinuationProgram,
    config: AbiConfig,
    regions: Option<&RegionPlan>,
    global_portal: bool,
) -> Result<HashMap<FunctionId, FunctionLayout>, FrameLayoutError> {
    let route_cells = if global_portal {
        GLOBAL_ROUTE_NIBBLE_CELLS
    } else {
        0
    };
    let mut layouts = HashMap::with_capacity(program.functions().len());
    for function in program.functions() {
        let branch_temporaries = program
            .continuations()
            .iter()
            .filter(|continuation| continuation.function() == function.id())
            .map(|continuation| maximum_branch_depth(continuation.body()))
            .max()
            .unwrap_or(0);
        let region_depth = regions.and_then(|plan| plan.branch_temporaries.get(&function.id()));
        let branch_temporaries = branch_temporaries.max(region_depth.copied().unwrap_or(0));
        let selector_cells = usize::from(region_depth.is_some());
        let selector_start = function
            .frame_slots()
            .checked_add(branch_temporaries)
            .ok_or(FrameLayoutError::SizeOverflow)?;
        let region_selector = region_depth.map(|_| FrameSlot::new(selector_start));
        let portal_temporary_start = selector_start
            .checked_add(selector_cells)
            .ok_or(FrameLayoutError::SizeOverflow)?;
        let value_cells = {
            let portal_temporaries = program
                .continuations()
                .iter()
                .filter(|continuation| continuation.function() == function.id())
                .filter_map(|continuation| match continuation.terminator() {
                    Terminator::AggregateLoad { cells, .. }
                    | Terminator::AggregateStore { cells, .. } => Some(*cells),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            portal_temporary_start.checked_add(portal_temporaries)
        }
        .ok_or(FrameLayoutError::SizeOverflow)?;
        let portal_temporary_cells = value_cells - portal_temporary_start;
        let frame = FrameLayout::with_aggregates_and_route(
            config,
            value_cells,
            function.frame_aggregates(),
            function.outbox_cells(),
            route_cells,
        )?;
        layouts.insert(
            function.id(),
            FunctionLayout {
                frame,
                branch_temporary_start: function.frame_slots(),
                region_selector,
                portal_temporary_start,
                portal_temporary_cells,
            },
        );
    }
    Ok(layouts)
}

/// Use the actual B1 layout, including selectors, loop gates and portal scratch.
pub(crate) fn estimated_frame_chunks(
    program: &ContinuationProgram,
) -> Result<HashMap<FunctionId, usize>, FrameLayoutError> {
    estimated_frame_chunks_with_route(program, has_global_portal(program))
}

pub(crate) fn estimated_frame_chunks_with_route(
    program: &ContinuationProgram,
    global_portal: bool,
) -> Result<HashMap<FunctionId, usize>, FrameLayoutError> {
    let regions = RegionPlan::new(program);
    let layouts =
        build_layouts_with_route(program, AbiConfig::default(), Some(&regions), global_portal)?;
    Ok(layouts
        .into_iter()
        .map(|(id, layout)| (id, layout.frame.frame_chunks()))
        .collect())
}
