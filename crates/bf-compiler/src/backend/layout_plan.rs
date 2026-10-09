//! Shared frame-layout planning and cost estimation, independent of BF emission.

use super::frame_layout::{AbiConfig, FrameLayout, FrameLayoutError};
use super::regions::{RegionPlan, maximum_branch_depth};
use crate::{
    Address, AggregateRegion, ContinuationProgram, FrameInstruction, FrameSlot, FunctionId,
    Terminator,
};
use std::collections::{BTreeSet, HashMap};

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

#[cfg(test)]
pub(crate) fn build_layouts_with_regions(
    program: &ContinuationProgram,
    config: AbiConfig,
    regions: Option<&RegionPlan>,
) -> Result<HashMap<FunctionId, FunctionLayout>, FrameLayoutError> {
    build_layouts_for_codegen(program, config, regions, false, false, false)
}

/// Apply optional physical guards after compact planning. Inlining cost
/// estimates keep using build_layouts_with_route and therefore do not change.
pub(crate) fn build_layouts_for_codegen(
    program: &ContinuationProgram,
    config: AbiConfig,
    regions: Option<&RegionPlan>,
    inplace_compare: bool,
    boundary_nibbles: bool,
    static_frames: bool,
) -> Result<HashMap<FunctionId, FunctionLayout>, FrameLayoutError> {
    let mut layouts = build_layouts_with_route(
        program,
        config,
        regions,
        has_global_portal(program) || boundary_nibbles,
    )?;
    if inplace_compare {
        let mut operands: HashMap<FunctionId, BTreeSet<usize>> = HashMap::new();
        // Extra truth banks would widen dynamic activations and every scan
        // through them. Only grow frames selected for fixed global contexts.
        let fixed = if static_frames {
            crate::cir::analysis::call_graph::global_context_functions(program)
        } else {
            Default::default()
        };
        for continuation in program.continuations() {
            if fixed.contains(&continuation.function()) {
                collect_truth_slots(
                    continuation.body(),
                    operands.entry(continuation.function()).or_default(),
                );
            }
            collect_compare_slots(
                continuation.body(),
                operands.entry(continuation.function()).or_default(),
            );
        }
        for (function, slots) in operands {
            let frame = &mut layouts
                .get_mut(&function)
                .expect("validated continuation function")
                .frame;
            let mut augmented = frame.clone();
            if augmented.reserve_compare_slots(&slots).is_ok() {
                *frame = augmented;
            } else {
                // New guards must not reject a previously valid compare bank.
                let mut original = BTreeSet::new();
                for c in program
                    .continuations()
                    .iter()
                    .filter(|c| c.function() == function)
                {
                    collect_compare_slots(c.body(), &mut original);
                }
                frame.reserve_compare_slots(&original)?;
            }
        }
    }
    Ok(layouts)
}

fn collect_compare_slots(body: &[FrameInstruction], slots: &mut BTreeSet<usize>) {
    for instruction in body {
        match instruction {
            FrameInstruction::Compare {
                left: Address::Frame(left),
                right: Address::Frame(right),
                ..
            }
            | FrameInstruction::SubWithBorrow {
                left: Address::Frame(left),
                right: Address::Frame(right),
                ..
            } if left != right => {
                slots.insert(left.index());
                slots.insert(right.index());
            }
            FrameInstruction::Loop { body, .. } => collect_compare_slots(body, slots),
            FrameInstruction::Branch {
                then_body,
                else_body,
                ..
            } => {
                collect_compare_slots(then_body, slots);
                collect_compare_slots(else_body, slots);
            }
            _ => {}
        }
    }
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

/// Match the preserving truth-copy emitter without expanding control flow.
/// Only fixed contexts may pay for new banks; compact inline estimates stay
/// independent of this optional physical layout.
fn collect_truth_slots(body: &[FrameInstruction], slots: &mut BTreeSet<usize>) {
    for (i, instruction) in body.iter().enumerate() {
        if let FrameInstruction::Copy {
            src: Address::Frame(source),
            dst: Address::Frame(destination),
        } = instruction
        {
            let next = if matches!(body.get(i+1),Some(FrameInstruction::AddConst{dst,..}) if *dst==Address::Frame(*destination))
            {
                i + 2
            } else {
                i + 1
            };
            if source != destination
                && matches!(body.get(next),Some(FrameInstruction::Branch{condition,..}) if *condition==Address::Frame(*destination))
            {
                slots.insert(source.index());
            }
        }
        match instruction {
            FrameInstruction::Loop { body, .. } => collect_truth_slots(body, slots),
            FrameInstruction::Branch {
                then_body,
                else_body,
                ..
            } => {
                collect_truth_slots(then_body, slots);
                collect_truth_slots(else_body, slots);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Continuation, ContinuationId, FunctionDescriptor, GlobalDescriptor, GlobalId, ValueType,
    };

    #[test]
    fn extra_truth_banks_fall_back_before_exceeding_the_frame_limit() {
        let function = FunctionId::new(0);
        let entry = ContinuationId::new(1).unwrap();
        let condition = Address::Frame(FrameSlot::new(9000));
        let mut body = vec![FrameInstruction::Output {
            src: Address::Global(GlobalId::new(0)),
        }];
        for i in 0..9000 {
            let source = Address::Frame(FrameSlot::new(i));
            body.extend([
                FrameInstruction::Copy {
                    src: source,
                    dst: condition,
                },
                FrameInstruction::Branch {
                    condition,
                    then_body: vec![FrameInstruction::Output { src: source }],
                    else_body: vec![],
                },
            ]);
        }
        let program = ContinuationProgram::new_with_globals(
            function,
            vec![GlobalDescriptor::cell(GlobalId::new(0))],
            vec![FunctionDescriptor::new(
                function,
                vec![],
                9001,
                ValueType::Void,
                entry,
            )],
            vec![Continuation::new(entry, function, body, Terminator::Halt)],
        )
        .unwrap();
        let layouts =
            build_layouts_for_codegen(&program, AbiConfig::default(), None, true, false, true)
                .unwrap();
        assert!(
            layouts[&function]
                .frame
                .truth_guards(FrameSlot::new(0))
                .is_none()
        );
        assert_eq!(
            layouts[&function].frame.frame_chunks(),
            build_layouts(&program, AbiConfig::default()).unwrap()[&function]
                .frame
                .frame_chunks()
        );
    }
}
