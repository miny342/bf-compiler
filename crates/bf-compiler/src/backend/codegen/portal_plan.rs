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
