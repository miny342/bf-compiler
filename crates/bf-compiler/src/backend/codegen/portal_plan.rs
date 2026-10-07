//! Hidden continuation IDs and routing plans for dynamic aggregate access.

use super::*;

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
    /// Return to the caller using its retained site PC, after a local global selector.
    pub(super) frame_return: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PortalSite {
    /// Original terminator whose resume liveness covers every hidden leaf.
    pub(super) origin: ContinuationId,
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
    pub(super) kind: PortalAccessKind,
    /// None uses the generic seven-byte protocol (fixed frames or capacity fallback).
    pub(super) return_selector: Option<u8>,
}

#[derive(Debug)]
pub(super) struct PortalPlan {
    pub(super) accessors: Vec<PortalAccessor>,
    pub(super) sites: HashMap<ContinuationId, PortalSite>,
    pub(super) ordered_sites: Vec<PortalSite>,
    pub(super) routers: Vec<GlobalPortalRouter>,
    pub(super) return_globals: Vec<GlobalId>,
    pub(super) frame_returns: bool,
}

impl PortalPlan {
    #[cfg(test)]
    pub(super) fn new(program: &ContinuationProgram) -> Result<Self, AbiCodegenError> {
        Self::with_frame_returns(program, false)
    }

    pub(super) fn with_frame_returns(
        program: &ContinuationProgram,
        frame_returns: bool,
    ) -> Result<Self, AbiCodegenError> {
        // One-byte private return selector; preserve the generic protocol for
        // unusually many globals rather than truncating the selector.
        let globals = program
            .continuations()
            .iter()
            .filter_map(|c| match c.terminator() {
                Terminator::ArrayLoad {
                    array: AggregateRegion::Global(g),
                    ..
                }
                | Terminator::ArrayStore {
                    array: AggregateRegion::Global(g),
                    ..
                }
                | Terminator::AggregateLoad {
                    source: AggregateRegion::Global(g),
                    ..
                }
                | Terminator::AggregateStore {
                    destination: AggregateRegion::Global(g),
                    ..
                } => Some(*g),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let frame_returns = frame_returns && globals.len() <= 256;
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
        let mut return_globals = Vec::new();
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
                    let key = (global, frame_returns.then_some(operation.kind()));
                    Some(if let Some(id) = router_ids.get(&key).copied() {
                        id
                    } else {
                        let id = allocate_hidden_id(&mut used)?;
                        router_ids.insert(key, id);
                        let return_selector = if frame_returns {
                            let index = return_globals
                                .iter()
                                .position(|g| *g == global)
                                .unwrap_or_else(|| {
                                    return_globals.push(global);
                                    return_globals.len() - 1
                                });
                            Some(index as u8)
                        } else {
                            None
                        };
                        routers.push(GlobalPortalRouter {
                            id,
                            global,
                            kind: operation.kind(),
                            return_selector,
                        });
                        id
                    })
                }
                AggregateRegion::Frame(_) | AggregateRegion::Outbox => None,
            };
            let frame_return = frame_returns && matches!(region, AggregateRegion::Global(_));
            let key = (operation.kind(), frame_return);
            let accessor = if let Some(accessor) = accessor_ids.get(&key).copied() {
                accessor
            } else {
                let id = allocate_hidden_id(&mut used)?;
                accessor_ids.insert(key, id);
                accessors.push(PortalAccessor {
                    id,
                    kind: key.0,
                    frame_return,
                });
                id
            };
            let resumes = (0..cells)
                .map(|_| allocate_hidden_id(&mut used))
                .collect::<Result<Vec<_>, _>>()?;
            let first_site = ordered_sites.len();
            for (leaf, &resume) in resumes.iter().enumerate() {
                let site = PortalSite {
                    origin: continuation.id(),
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
            return_globals,
            frame_returns,
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
