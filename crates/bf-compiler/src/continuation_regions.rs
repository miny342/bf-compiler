//! Private output plans over semantic CIR. Resume edges are never expanded.
//!
//! This prototype duplicates acyclic soft paths. Nodes in (or reaching) a soft
//! cycle keep ordinary dispatch; the existing CFG optimizer can reconstruct
//! local loops before this pass. No semantic IDs or public wire formats change.

use std::collections::{HashMap, HashSet};

use crate::abi_codegen::{maximum_branch_depth, maximum_terminator_branch_depth};
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
}

#[derive(Debug)]
pub(crate) enum RegionFlow {
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
    pub cyclic_fallbacks: usize,
    pub bounded_fallbacks: usize,
}

impl RegionPlan {
    pub fn new(program: &ContinuationProgram) -> Self {
        let nodes = program
            .continuations()
            .iter()
            .map(|c| (c.id(), c))
            .collect::<HashMap<_, _>>();
        // Reverse topological elimination leaves cycles and their predecessors.
        // Falling back for that entire set also handles irreducible soft SCCs.
        let mut degree = HashMap::new();
        let mut predecessors: HashMap<_, Vec<_>> = HashMap::new();
        let mut ready = Vec::new();
        for c in program.continuations() {
            let targets = c
                .terminator()
                .edges()
                .filter(|(_, kind)| *kind == EdgeKind::Normal);
            let mut count = 0;
            for (target, _) in targets {
                predecessors.entry(target).or_default().push(c.id());
                count += 1;
            }
            degree.insert(c.id(), count);
            if count == 0 {
                ready.push(c.id());
            }
        }
        let mut acyclic = HashSet::new();
        while let Some(id) = ready.pop() {
            acyclic.insert(id);
            for predecessor in predecessors.get(&id).into_iter().flatten() {
                let remaining = degree.get_mut(predecessor).unwrap();
                *remaining -= 1;
                if *remaining == 0 {
                    ready.push(*predecessor);
                }
            }
        }

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
            let region = if acyclic.contains(&id) {
                let mut builder = Builder {
                    nodes: &nodes,
                    remaining: MAX_EXPANDED_BLOCKS,
                    terminals: Vec::new(),
                };
                if let Some((root, branch_temporaries)) = builder.expand(id, 0) {
                    Some(Region {
                        root,
                        branch_temporaries,
                        terminals: builder.terminals,
                    })
                } else {
                    plan.bounded_fallbacks += 1;
                    None
                }
            } else {
                plan.cyclic_fallbacks += 1;
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
}

impl Builder<'_> {
    fn expand(&mut self, id: ContinuationId, depth: usize) -> Option<(RegionNode, usize)> {
        if depth == MAX_DEPTH || self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
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
            }
            | Terminator::BranchWithBodies {
                then_target,
                else_target,
                ..
            } => {
                let (then_node, then_depth) = self.expand(*then_target, depth + 1)?;
                let (else_node, else_depth) = self.expand(*else_target, depth + 1)?;
                // Two private gates protect a consumed condition whose allocated
                // slot may be reused immediately by either successor.
                temporaries = temporaries.max(
                    2 + then_depth
                        .max(else_depth)
                        .max(maximum_terminator_branch_depth(c.terminator())),
                );
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
        Some((RegionNode { id, flow }, temporaries))
    }
}
