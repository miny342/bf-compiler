//! Frame scratch layout and hidden portal continuation planning.

use super::*;

#[derive(Debug)]
pub(super) struct FunctionLayout {
    pub(super) frame: FrameLayout,
    pub(super) branch_temporary_start: usize,
    pub(super) region_selector: Option<FrameSlot>,
    pub(super) portal_temporary_start: usize,
    pub(super) portal_temporary_cells: usize,
}

#[cfg(test)]
pub(super) fn build_layouts(
    program: &ContinuationProgram,
    config: AbiConfig,
) -> Result<HashMap<FunctionId, FunctionLayout>, AbiCodegenError> {
    build_layouts_with_regions(program, config, None)
}

pub(super) fn build_layouts_with_regions(
    program: &ContinuationProgram,
    config: AbiConfig,
    regions: Option<&RegionPlan>,
) -> Result<HashMap<FunctionId, FunctionLayout>, AbiCodegenError> {
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

pub(super) fn build_layouts_with_route(
    program: &ContinuationProgram,
    config: AbiConfig,
    regions: Option<&RegionPlan>,
    global_portal: bool,
) -> Result<HashMap<FunctionId, FunctionLayout>, AbiCodegenError> {
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
) -> Result<HashMap<FunctionId, usize>, AbiCodegenError> {
    estimated_frame_chunks_with_route(program, has_global_portal(program))
}

pub(crate) fn estimated_frame_chunks_with_route(
    program: &ContinuationProgram,
    global_portal: bool,
) -> Result<HashMap<FunctionId, usize>, AbiCodegenError> {
    let regions = RegionPlan::new(program);
    let layouts =
        build_layouts_with_route(program, AbiConfig::default(), Some(&regions), global_portal)?;
    Ok(layouts
        .into_iter()
        .map(|(id, layout)| (id, layout.frame.frame_chunks()))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum PortalAccessKind {
    Load,
    Store,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PortalOffset {
    Byte(Address),
    Word(LogicalOffset),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PortalOperation {
    Load { destination: ValueOperand },
    Store { source: ValueOperand },
}

impl PortalOperation {
    pub(super) const fn kind(self) -> PortalAccessKind {
        match self {
            Self::Load { .. } => PortalAccessKind::Load,
            Self::Store { .. } => PortalAccessKind::Store,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PortalAccessor {
    pub(super) id: ContinuationId,
    pub(super) kind: PortalAccessKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PortalSite {
    pub(super) resume: ContinuationId,
    pub(super) accessor: ContinuationId,
    pub(super) function: FunctionId,
    pub(super) region: AggregateRegion,
    pub(super) offset: PortalOffset,
    pub(super) operation: PortalOperation,
    pub(super) leaf: usize,
    pub(super) cells: usize,
    pub(super) return_to: ContinuationId,
    pub(super) next_resume: Option<ContinuationId>,
    pub(super) router: Option<ContinuationId>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct GlobalPortalRouter {
    pub(super) id: ContinuationId,
    pub(super) global: GlobalId,
}

#[derive(Debug)]
pub(super) struct PortalPlan {
    pub(super) accessors: Vec<PortalAccessor>,
    pub(super) sites: HashMap<ContinuationId, PortalSite>,
    pub(super) ordered_sites: Vec<PortalSite>,
    pub(super) routers: Vec<GlobalPortalRouter>,
}

impl PortalPlan {
    pub(super) fn new(program: &ContinuationProgram) -> Result<Self, AbiCodegenError> {
        let mut used = program
            .continuations()
            .iter()
            .map(|continuation| continuation.id().get())
            .collect::<HashSet<_>>();
        let mut accessors = Vec::new();
        let mut accessor_ids = HashMap::new();
        let mut sites = HashMap::new();
        let mut ordered_sites = Vec::new();
        let mut routers = Vec::new();
        let mut router_ids = HashMap::new();
        for continuation in program.continuations() {
            let (region, offset, operation, cells, return_to) = match *continuation.terminator() {
                Terminator::ArrayLoad {
                    array,
                    index,
                    destination,
                    return_to,
                } => (
                    array,
                    PortalOffset::Byte(index),
                    PortalOperation::Load {
                        destination: ValueOperand::Cell(destination),
                    },
                    1,
                    return_to,
                ),
                Terminator::ArrayStore {
                    array,
                    index,
                    value,
                    return_to,
                } => (
                    array,
                    PortalOffset::Byte(index),
                    PortalOperation::Store {
                        source: ValueOperand::Cell(value),
                    },
                    1,
                    return_to,
                ),
                Terminator::AggregateLoad {
                    source,
                    offset,
                    destination,
                    cells,
                    return_to,
                } => (
                    source,
                    PortalOffset::Word(offset),
                    PortalOperation::Load { destination },
                    cells,
                    return_to,
                ),
                Terminator::AggregateStore {
                    destination,
                    offset,
                    source,
                    cells,
                    return_to,
                } => (
                    destination,
                    PortalOffset::Word(offset),
                    PortalOperation::Store { source },
                    cells,
                    return_to,
                ),
                _ => continue,
            };
            let router = match region {
                AggregateRegion::Global(global) => {
                    Some(if let Some(id) = router_ids.get(&global).copied() {
                        id
                    } else {
                        let id = allocate_hidden_id(&mut used)?;
                        router_ids.insert(global, id);
                        routers.push(GlobalPortalRouter { id, global });
                        id
                    })
                }
                AggregateRegion::Frame(_) | AggregateRegion::Outbox => None,
            };
            let key = operation.kind();
            let accessor = if let Some(accessor) = accessor_ids.get(&key).copied() {
                accessor
            } else {
                let id = allocate_hidden_id(&mut used)?;
                accessor_ids.insert(key, id);
                accessors.push(PortalAccessor { id, kind: key });
                id
            };
            let resumes = (0..cells)
                .map(|_| allocate_hidden_id(&mut used))
                .collect::<Result<Vec<_>, _>>()?;
            let first_site = ordered_sites.len();
            for (leaf, &resume) in resumes.iter().enumerate() {
                let site = PortalSite {
                    resume,
                    accessor,
                    function: continuation.function(),
                    region,
                    offset,
                    operation,
                    leaf,
                    cells,
                    return_to,
                    next_resume: resumes.get(leaf + 1).copied(),
                    router,
                };
                ordered_sites.push(site);
            }
            sites.insert(
                continuation.id(),
                *ordered_sites
                    .get(first_site)
                    .expect("validated portal access contains at least one cell"),
            );
        }

        Ok(Self {
            accessors,
            sites,
            ordered_sites,
            routers,
        })
    }
}

pub(super) fn allocate_hidden_id(
    used: &mut HashSet<u16>,
) -> Result<ContinuationId, AbiCodegenError> {
    for value in 1..=u16::MAX {
        if used.insert(value) {
            return Ok(ContinuationId::new(value).expect("nonzero continuation ID"));
        }
    }
    Err(AbiCodegenError::ContinuationIdsExhausted)
}

pub(crate) fn maximum_branch_depth(instructions: &[FrameInstruction]) -> usize {
    instructions
        .iter()
        .map(|instruction| match instruction {
            FrameInstruction::Loop { body, .. } => maximum_branch_depth(body),
            FrameInstruction::Branch {
                then_body,
                else_body,
                ..
            } if else_body.is_empty() => maximum_branch_depth(then_body),
            FrameInstruction::Branch {
                then_body,
                else_body,
                ..
            } => 1 + maximum_branch_depth(then_body).max(maximum_branch_depth(else_body)),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}
