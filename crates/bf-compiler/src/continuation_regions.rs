//! Private output plans over semantic CIR. Resume edges are never expanded.
//!
//! Expand soft paths until a hard boundary or an ancestor block is reached.
//! Backedges restart a native loop around that ancestor; nested loops can exit
//! to an outer ancestor by setting only its restart flag. This is bounded CFG
//! unfolding, not a dispatcher per soft block. Public wire formats stay intact.

use std::collections::{HashMap, HashSet};

use crate::abi_codegen::maximum_branch_depth;
use crate::continuation_ir::{
    BoundaryKind, Continuation, ContinuationId, ContinuationProgram, EdgeKind, FunctionId,
    Terminator,
};

const MAX_TERMINALS: usize = 255;
const MAX_EXPANDED_BLOCKS: usize = 4096;
const MAX_DEPTH: usize = 128;

#[derive(Debug)]
pub(crate) struct RegionNode {
    pub id: ContinuationId,
    pub flow: RegionFlow,
    pub loop_header: bool,
}

#[derive(Debug)]
pub(crate) enum RegionFlow {
    /// No body is emitted here. Restart the ancestor named by RegionNode::id.
    Continue,
    Terminal(u8),
    Goto(Box<RegionNode>),
    Branch(Box<RegionNode>, Box<RegionNode>),
}

#[derive(Debug)]
pub(crate) struct Region {
    pub root: RegionNode,
    pub terminals: Vec<ContinuationId>,
    pub branch_temporaries: usize,
}

#[derive(Debug, Default)]
pub(crate) struct RegionPlan {
    pub entries: HashSet<ContinuationId>,
    pub regions: HashMap<ContinuationId, Region>,
    pub branch_temporaries: HashMap<FunctionId, usize>,
    pub bounded_fallbacks: usize,
}

impl RegionPlan {
    pub fn new(program: &ContinuationProgram) -> Self {
        let nodes = program
            .continuations()
            .iter()
            .map(|c| (c.id(), c))
            .collect::<HashMap<_, _>>();
        let mut pending = program
            .functions()
            .iter()
            .map(|f| f.entry())
            .collect::<Vec<_>>();
        pending.extend(program.continuations().iter().flat_map(|c| {
            c.terminator()
                .edges()
                .filter_map(|(id, kind)| (kind == EdgeKind::Resume).then_some(id))
        }));
        let mut plan = Self::default();
        while let Some(id) = pending.pop() {
            if !plan.entries.insert(id) {
                continue;
            }
            let c = nodes[&id];
            if c.terminator().boundary() != BoundaryKind::Soft {
                continue;
            }
            let mut builder = Builder {
                nodes: &nodes,
                remaining: MAX_EXPANDED_BLOCKS,
                terminals: Vec::new(),
                active: HashSet::new(),
                loop_headers: HashSet::new(),
            };
            let region = if let Some((root, branch_temporaries)) = builder.expand(id, 0) {
                Some(Region {
                    root,
                    branch_temporaries,
                    terminals: builder.terminals,
                })
            } else {
                plan.bounded_fallbacks += 1;
                None
            };
            if let Some(region) = region {
                let depth = plan.branch_temporaries.entry(c.function()).or_default();
                *depth = (*depth).max(region.branch_temporaries);
                plan.regions.insert(id, region);
            } else {
                // A fallback block still writes PCs for its ordinary successors.
                pending.extend(c.terminator().edges().map(|(target, _)| target));
            }
        }
        plan
    }
}

struct Builder<'a> {
    nodes: &'a HashMap<ContinuationId, &'a Continuation>,
    remaining: usize,
    terminals: Vec<ContinuationId>,
    active: HashSet<ContinuationId>,
    loop_headers: HashSet<ContinuationId>,
}

impl Builder<'_> {
    fn expand(&mut self, id: ContinuationId, depth: usize) -> Option<(RegionNode, usize)> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        if self.active.contains(&id) {
            self.loop_headers.insert(id);
            return Some((
                RegionNode {
                    id,
                    flow: RegionFlow::Continue,
                    loop_header: false,
                },
                0,
            ));
        }
        if depth == MAX_DEPTH {
            return None;
        }
        self.active.insert(id);
        let c = self.nodes[&id];
        let mut temporaries = maximum_branch_depth(c.body());
        let flow = match c.terminator() {
            Terminator::Goto { target } => {
                let (child, child_depth) = self.expand(*target, depth + 1)?;
                temporaries = temporaries.max(child_depth);
                RegionFlow::Goto(Box::new(child))
            }
            Terminator::Branch {
                then_target,
                else_target,
                ..
            } => {
                let (then_node, then_depth) = self.expand(*then_target, depth + 1)?;
                let (else_node, else_depth) = self.expand(*else_target, depth + 1)?;
                // Two private gates protect a consumed condition whose allocated
                // slot may be reused immediately by either successor.
                temporaries = temporaries.max(2 + then_depth.max(else_depth));
                RegionFlow::Branch(Box::new(then_node), Box::new(else_node))
            }
            terminal => {
                debug_assert_ne!(terminal.boundary(), BoundaryKind::Soft);
                let index = if let Some(index) = self.terminals.iter().position(|&t| t == id) {
                    index
                } else {
                    if self.terminals.len() == MAX_TERMINALS {
                        return None;
                    }
                    self.terminals.push(id);
                    self.terminals.len() - 1
                };
                RegionFlow::Terminal((index + 1) as u8)
            }
        };
        self.active.remove(&id);
        let loop_header = self.loop_headers.remove(&id);
        // A restart flag is live across every path inside this loop. The
        // selected hard terminal runs only after all these flags are zero.
        temporaries += usize::from(loop_header);
        Some((
            RegionNode {
                id,
                flow,
                loop_header,
            },
            temporaries,
        ))
    }
}
