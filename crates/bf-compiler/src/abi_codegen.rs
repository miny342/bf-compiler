use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;

use crate::bf_optimizer::{optimize_annotated_bf, optimize_bf};
use crate::continuation_ir::{
    Address, AggregateRegion, ArrayRegion, Continuation, ContinuationId, ContinuationProgram,
    FrameInstruction, FrameSlot, FrameTransferTarget, FunctionDescriptor, FunctionId, GlobalId,
    LogicalOffset, ParameterLocation, SourceSpan, Terminator, ValueOperand, ValueType,
};
use crate::continuation_regions::{Region, RegionFlow, RegionNode, RegionPlan};
use crate::frame_layout::{
    AbiConfig, AbiField, FrameLayout, FrameLayoutError, PROTOCOL_CELLS,
    aggregate_element_physical_offset, aggregate_page_portal_offset,
};
use crate::static_layout::{StaticLayout, StaticLayoutError};
use crate::{
    AnnotatedBfInstruction, AnnotatedBfOperation, AnnotatedBfProgram, BfProgram, ProfileSiteTable,
};

mod region_emission;

/// Selects how much compiler provenance is emitted into a profile map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileGranularity {
    /// ABI templates only.
    Abi,
    /// ABI templates plus functions and continuations.
    Continuation,
    /// Continuation provenance plus frame instructions and terminators.
    Instruction,
    /// Instruction sites with source spans and source-file metadata when the
    /// program originated in the BFC frontend.
    Source,
}

/// BF backend choices independent of source/CIR lowering and ABI geometry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AbiCodegenOptions {
    /// Skip the standard tape capacity check during code generation.
    pub unlimited_tape: bool,
    /// Bound byte-wise frame/global crossings using two nibble counters.
    /// Off by default: RemoteTransfer can execute unary loops directly.
    /// This does not change offset decomposition or portal window jumps.
    pub nibble_transfer: bool,
}

/// Lower BF with explicit backend options, without profiling metadata.
pub fn lower_continuations_with_codegen_options(
    program: &ContinuationProgram,
    options: AbiCodegenOptions,
) -> Result<BfProgram, AbiCodegenError> {
    Ok(lower_continuations_with_profile_and_codegen_options(
        program,
        ProfileGranularity::Abi,
        options,
    )?
    .into_plain())
}

/// Lower annotated BF using the same backend options as ordinary output.
pub fn lower_continuations_with_profile_and_codegen_options(
    program: &ContinuationProgram,
    granularity: ProfileGranularity,
    options: AbiCodegenOptions,
) -> Result<AnnotatedBfProgram, AbiCodegenError> {
    lower_continuations_annotated_with_options(
        program,
        AbiConfig::default(),
        !options.unlimited_tape,
        granularity,
        options.nibble_transfer,
        false,
    )
}

/// A normal BF artifact plus its sidecar profile map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledProfileArtifact {
    pub source: String,
    pub map: bf_profiling::ProfileMap,
}

impl CompiledProfileArtifact {
    pub fn embedded_source(&self) -> Result<String, bf_profiling::EmbeddedProfileError> {
        bf_profiling::embed_profile_markers(&self.source, &self.map)
    }
}

/// Compile continuation IR with the default ABI configuration.
pub fn compile_continuations(program: &ContinuationProgram) -> Result<String, AbiCodegenError> {
    Ok(optimize_bf(&lower_continuations(program)?).to_source())
}

/// Compile an artifact with provenance sidecar metadata.
pub fn compile_continuations_with_profile(
    program: &ContinuationProgram,
    granularity: ProfileGranularity,
) -> Result<CompiledProfileArtifact, AbiCodegenError> {
    let annotated = lower_continuations_with_profile(program, granularity)?;
    let optimized = optimize_annotated_bf(&annotated);
    Ok(optimized.profile_artifact(false))
}

/// Compile an unbounded-tape artifact with provenance sidecar metadata.
pub fn compile_continuations_unbounded_with_profile(
    program: &ContinuationProgram,
    granularity: ProfileGranularity,
) -> Result<CompiledProfileArtifact, AbiCodegenError> {
    let annotated = lower_continuations_unbounded_with_profile(program, granularity)?;
    let optimized = optimize_annotated_bf(&annotated);
    Ok(optimized.profile_artifact(false))
}

/// Compile continuation IR without enforcing the standard 30,000-cell tape.
pub fn compile_continuations_unbounded(
    program: &ContinuationProgram,
) -> Result<String, AbiCodegenError> {
    Ok(optimize_bf(&lower_continuations_unbounded(program)?).to_source())
}

/// Lower continuation IR to BF IR with the default ABI configuration.
pub fn lower_continuations(program: &ContinuationProgram) -> Result<BfProgram, AbiCodegenError> {
    lower_continuations_with_options(program, AbiConfig::default(), true)
}

/// Lower continuation IR to provenance-carrying BF IR.
pub fn lower_continuations_with_profile(
    program: &ContinuationProgram,
    granularity: ProfileGranularity,
) -> Result<AnnotatedBfProgram, AbiCodegenError> {
    lower_continuations_annotated_with_options(
        program,
        AbiConfig::default(),
        true,
        granularity,
        false,
        false,
    )
}

/// Lower continuation IR to provenance-carrying BF IR without the standard
/// tape-capacity check.
pub fn lower_continuations_unbounded_with_profile(
    program: &ContinuationProgram,
    granularity: ProfileGranularity,
) -> Result<AnnotatedBfProgram, AbiCodegenError> {
    lower_continuations_annotated_with_options(
        program,
        AbiConfig::default(),
        false,
        granularity,
        false,
        false,
    )
}

/// Lower continuation IR without enforcing the standard 30,000-cell tape.
pub fn lower_continuations_unbounded(
    program: &ContinuationProgram,
) -> Result<BfProgram, AbiCodegenError> {
    lower_continuations_with_options(program, AbiConfig::default(), false)
}

/// Lower continuation IR using an explicitly selected ABI chunk geometry.
pub fn lower_continuations_with_config(
    program: &ContinuationProgram,
    config: AbiConfig,
) -> Result<BfProgram, AbiCodegenError> {
    lower_continuations_with_options(program, config, true)
}

fn lower_continuations_with_options(
    program: &ContinuationProgram,
    config: AbiConfig,
    check_capacity: bool,
) -> Result<BfProgram, AbiCodegenError> {
    Ok(lower_continuations_annotated_with_options(
        program,
        config,
        check_capacity,
        ProfileGranularity::Abi,
        false,
        false,
    )?
    .into_plain())
}

fn lower_continuations_annotated_with_options(
    program: &ContinuationProgram,
    config: AbiConfig,
    check_capacity: bool,
    granularity: ProfileGranularity,
    nibble_transfer: bool,
    region_emission: bool,
) -> Result<AnnotatedBfProgram, AbiCodegenError> {
    // B1 is a private experiment exercised by region_probe. Public entry
    // points deliberately keep region_emission=false during this prototype.
    let regions = region_emission.then(|| RegionPlan::new(program));
    let layouts = build_layouts_with_regions(program, config, regions.as_ref())?;
    let static_layout = if check_capacity {
        StaticLayout::new(config, program.globals())?
    } else {
        StaticLayout::new_unbounded(config, program.globals())?
    };
    let portal = PortalPlan::new(program)?;
    let mut emitter = AbiEmitter::new(
        program,
        &layouts,
        &static_layout,
        &portal,
        config,
        granularity,
    );
    emitter.nibble_transfer = nibble_transfer;
    emitter.regions = regions.as_ref();
    if let Some(regions) = &regions {
        emitter.dispatch_encoding = DispatchEncoding::with_regions(program, &portal, Some(regions));
    }
    emitter.with_profile_site(
        "abi",
        "abi.initialization",
        "ABI initialization",
        |emitter| emitter.initialize_main(check_capacity),
    )?;
    emitter.with_profile_site("abi", "abi.dispatcher", "ABI dispatcher", |emitter| {
        emitter.emit_dispatcher()
    })?;
    Ok(AnnotatedBfProgram::new(emitter.output, emitter.sites))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiCodegenError {
    Layout(FrameLayoutError),
    StaticLayout(StaticLayoutError),
    MissingFunctionLayout { function: FunctionId },
    ContinuationIdsExhausted,
}

impl fmt::Display for AbiCodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => error.fmt(f),
            Self::StaticLayout(error) => error.fmt(f),
            Self::MissingFunctionLayout { function } => {
                write!(f, "function {} has no ABI frame layout", function.index())
            }
            Self::ContinuationIdsExhausted => {
                write!(f, "array portals exhaust the 16-bit continuation ID space")
            }
        }
    }
}

impl Error for AbiCodegenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layout(error) => Some(error),
            Self::StaticLayout(error) => Some(error),
            Self::MissingFunctionLayout { .. } | Self::ContinuationIdsExhausted => None,
        }
    }
}

impl From<FrameLayoutError> for AbiCodegenError {
    fn from(error: FrameLayoutError) -> Self {
        Self::Layout(error)
    }
}

impl From<StaticLayoutError> for AbiCodegenError {
    fn from(error: StaticLayoutError) -> Self {
        Self::StaticLayout(error)
    }
}

#[derive(Debug)]
struct FunctionLayout {
    frame: FrameLayout,
    branch_temporary_start: usize,
    region_selector: Option<FrameSlot>,
    portal_temporary_start: usize,
    portal_temporary_cells: usize,
}

const GLOBAL_ROUTE_NIBBLE_CELLS: usize = 16;
const ROUTE_OFFSET_LOW: usize = 0;
const ROUTE_OFFSET_HIGH: usize = 1;
const ROUTE_VALUE: usize = 2;
const ROUTE_ACCESSOR_LOW: usize = 3;
const ROUTE_ACCESSOR_HIGH: usize = 4;
const ROUTE_RESUME_LOW: usize = 5;
const ROUTE_RESUME_HIGH: usize = 6;
const ROUTE_SCRATCH_START: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    /// Offset from the current function's dispatch-context base.
    Relative(isize),
    /// Absolute position in the static prefix.
    Global(usize),
}

#[cfg(test)]
fn build_layouts(
    program: &ContinuationProgram,
    config: AbiConfig,
) -> Result<HashMap<FunctionId, FunctionLayout>, AbiCodegenError> {
    build_layouts_with_regions(program, config, None)
}

fn build_layouts_with_regions(
    program: &ContinuationProgram,
    config: AbiConfig,
    regions: Option<&RegionPlan>,
) -> Result<HashMap<FunctionId, FunctionLayout>, AbiCodegenError> {
    let has_global_portal = program.continuations().iter().any(|continuation| {
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
    });
    let route_cells = if has_global_portal {
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
            .map(|continuation| {
                maximum_branch_depth(continuation.body())
                    .max(maximum_terminator_branch_depth(continuation.terminator()))
            })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PortalAccessKind {
    Load,
    Store,
}

#[derive(Debug, Clone, Copy)]
enum PortalOffset {
    Byte(Address),
    Word(LogicalOffset),
}

#[derive(Debug, Clone, Copy)]
enum PortalOperation {
    Load { destination: ValueOperand },
    Store { source: ValueOperand },
}

impl PortalOperation {
    const fn kind(self) -> PortalAccessKind {
        match self {
            Self::Load { .. } => PortalAccessKind::Load,
            Self::Store { .. } => PortalAccessKind::Store,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PortalAccessor {
    id: ContinuationId,
    kind: PortalAccessKind,
}

#[derive(Debug, Clone, Copy)]
struct PortalSite {
    resume: ContinuationId,
    accessor: ContinuationId,
    function: FunctionId,
    region: AggregateRegion,
    offset: PortalOffset,
    operation: PortalOperation,
    leaf: usize,
    cells: usize,
    return_to: ContinuationId,
    next_resume: Option<ContinuationId>,
    router: Option<ContinuationId>,
}

#[derive(Debug, Clone, Copy)]
struct GlobalPortalRouter {
    id: ContinuationId,
    global: GlobalId,
}

#[derive(Debug, Clone, Copy)]
enum DispatchEntry<'a> {
    Continuation(&'a Continuation),
    PortalAccessor(PortalAccessor),
    PortalResume(PortalSite),
    GlobalPortalRouter(GlobalPortalRouter),
}

impl DispatchEntry<'_> {
    fn id(self) -> ContinuationId {
        match self {
            Self::Continuation(continuation) => continuation.id(),
            Self::PortalAccessor(accessor) => accessor.id,
            Self::PortalResume(site) => site.resume,
            Self::GlobalPortalRouter(router) => router.id,
        }
    }
}

/// Maps stable logical continuation IDs to the bytes stored in ABI PC fields.
/// Dense programs balance the two countdown levels instead of filling 256-entry
/// low-byte pages first. Sparse public IR retains its original page geometry.
/// Portal entries receive the first dense ranks to reduce PC transport costs.
/// Reverse pages where it further lowers their aggregate countdown distance.
#[derive(Debug)]
struct DispatchEncoding {
    reversed_low_pages: HashMap<u8, (u8, u8)>,
    page_width: u16,
    dense_ranks: Vec<u16>,
}

impl DispatchEncoding {
    fn new(program: &ContinuationProgram, portal: &PortalPlan) -> Self {
        Self::with_regions(program, portal, None)
    }

    fn with_regions(
        program: &ContinuationProgram,
        portal: &PortalPlan,
        regions: Option<&RegionPlan>,
    ) -> Self {
        let mut entries = program
            .continuations()
            .iter()
            .filter(|c| regions.is_none_or(|plan| plan.entries.contains(&c.id())))
            .map(|c| (c.id(), false))
            .chain(portal.accessors.iter().map(|a| (a.id, true)))
            .chain(portal.ordered_sites.iter().map(|s| (s.resume, true)))
            .chain(portal.routers.iter().map(|r| (r.id, true)))
            .collect::<Vec<_>>();
        let count = entries.len();
        let maximum = entries.iter().map(|(id, _)| id.get()).max().unwrap_or(1);
        // IDs are nonzero and unique, including hidden portal IDs. Equality
        // therefore proves that 1..=maximum is dense. Balancing p + n/p
        // bounds both linear countdown levels by approximately sqrt(n).
        // Include the unused zero code when choosing the width so that both
        // encoded bytes fit, even for a program with all 65,535 IDs occupied.
        // Region emission removes soft-only entries without rewriting semantic
        // IDs. Compact those remaining entries in the private PC encoding too.
        let dense = regions.is_some() || (count == usize::from(maximum) && count > 256);
        let page_width = if dense && count > 256 {
            (1_u16..=256)
                .find(|&width| usize::from(width).pow(2) > count)
                .unwrap_or(256)
        } else {
            256
        };
        let mut dense_ranks = Vec::new();
        if dense {
            // Portal PCs cross global/frame boundaries by value. Keep their
            // byte values small to reduce navigation while transferring them,
            // as well as the countdown needed to dispatch them.
            entries.sort_unstable_by_key(|&(id, hidden)| (!hidden, id.get()));
            dense_ranks.resize(usize::from(maximum) + 1, 0);
            for (index, &(id, _)) in entries.iter().enumerate() {
                dense_ranks[usize::from(id.get())] = (index + 1) as u16;
            }
        }
        let mut pages = BTreeMap::<u8, (Vec<u8>, Vec<u8>)>::new();
        for (id, hidden) in entries {
            let rank = dense_ranks
                .get(usize::from(id.get()))
                .copied()
                .unwrap_or(id.get());
            let value = (rank / page_width) * 256 + rank % page_width;
            let page = pages.entry((value >> 8) as u8).or_default();
            page.0.push(value as u8);
            if hidden {
                page.1.push(value as u8);
            }
        }

        let reversed_low_pages = pages
            .into_iter()
            .filter_map(|(high, (lows, hidden))| {
                if hidden.is_empty() {
                    return None;
                }
                let minimum = *lows.iter().min().expect("dispatch page is nonempty");
                let maximum = *lows.iter().max().expect("dispatch page is nonempty");
                let ascending_cost = hidden
                    .iter()
                    .map(|&low| usize::from(low - minimum))
                    .sum::<usize>();
                let descending_cost = hidden
                    .iter()
                    .map(|&low| usize::from(maximum - low))
                    .sum::<usize>();
                (descending_cost < ascending_cost).then_some((high, (minimum, maximum)))
            })
            .collect();
        Self {
            reversed_low_pages,
            page_width,
            dense_ranks,
        }
    }

    fn encode(&self, id: ContinuationId) -> u16 {
        let rank = self
            .dense_ranks
            .get(usize::from(id.get()))
            .copied()
            .unwrap_or(id.get());
        let value = (rank / self.page_width) * 256 + rank % self.page_width;
        let high = (value >> 8) as u8;
        let low = value as u8;
        let encoded_low = self
            .reversed_low_pages
            .get(&high)
            .map_or(low, |&(minimum, maximum)| minimum + (maximum - low));
        (u16::from(high) << 8) | u16::from(encoded_low)
    }
}

#[derive(Debug)]
struct PortalPlan {
    accessors: Vec<PortalAccessor>,
    sites: HashMap<ContinuationId, PortalSite>,
    ordered_sites: Vec<PortalSite>,
    routers: Vec<GlobalPortalRouter>,
}

impl PortalPlan {
    fn new(program: &ContinuationProgram) -> Result<Self, AbiCodegenError> {
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

fn allocate_hidden_id(used: &mut HashSet<u16>) -> Result<ContinuationId, AbiCodegenError> {
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

pub(crate) fn maximum_terminator_branch_depth(terminator: &Terminator) -> usize {
    match terminator {
        Terminator::BranchWithBodies {
            then_body,
            else_body,
            ..
        } => {
            let nested = maximum_branch_depth(then_body).max(maximum_branch_depth(else_body));
            if else_body.is_empty() {
                nested
            } else {
                nested + 1
            }
        }
        _ => 0,
    }
}

fn instruction_kind(instruction: &FrameInstruction) -> &'static str {
    match instruction {
        FrameInstruction::SubWithBorrow { .. } => "sub_with_borrow",
        FrameInstruction::Compare { .. } => "compare",
        FrameInstruction::Set { .. } => "set",
        FrameInstruction::AddConst { .. } => "add_const",
        FrameInstruction::Copy { .. } => "copy",
        FrameInstruction::Transfer { .. } => "transfer",
        FrameInstruction::AggregateCopy { .. } => "aggregate_copy",
        FrameInstruction::Input { .. } => "input",
        FrameInstruction::Output { .. } => "output",
        FrameInstruction::Loop { .. } => "loop",
        FrameInstruction::Branch { .. } => "branch",
    }
}

fn terminator_kind(terminator: &Terminator) -> &'static str {
    match terminator {
        Terminator::Goto { .. } => "goto",
        Terminator::Branch { .. } => "branch",
        Terminator::BranchWithBodies { .. } => "branch_with_bodies",
        Terminator::Call { .. } => "call",
        Terminator::Return { .. } => "return",
        Terminator::ArrayLoad { .. } => "array_load",
        Terminator::ArrayStore { .. } => "array_store",
        Terminator::AggregateLoad { .. } => "aggregate_load",
        Terminator::AggregateStore { .. } => "aggregate_store",
        Terminator::Abort => "abort",
        Terminator::Halt => "halt",
    }
}

struct AbiEmitter<'a> {
    program: &'a ContinuationProgram,
    layouts: &'a HashMap<FunctionId, FunctionLayout>,
    static_layout: &'a StaticLayout,
    portal: &'a PortalPlan,
    regions: Option<&'a RegionPlan>,
    dispatch_encoding: DispatchEncoding,
    config: AbiConfig,
    output: Vec<AnnotatedBfInstruction>,
    sites: ProfileSiteTable,
    site_stack: Vec<bf_profiling::ProfileSiteId>,
    /// Structural path of the frame instruction currently being emitted.
    ///
    /// Frame instructions do not have IDs of their own, so their position in
    /// the continuation body is part of the stable profile identity.  The
    /// named segments distinguish nested loop and branch bodies that would
    /// otherwise share the same numeric index.
    instruction_path: Vec<String>,
    granularity: ProfileGranularity,
    /// Physical during initialization; context-base-relative in dispatcher.
    position: isize,
    branch_temporary_depth: usize,
    nibble_transfer: bool,
    current_source: Option<SourceSpan>,
}

impl<'a> AbiEmitter<'a> {
    fn new(
        program: &'a ContinuationProgram,
        layouts: &'a HashMap<FunctionId, FunctionLayout>,
        static_layout: &'a StaticLayout,
        portal: &'a PortalPlan,
        config: AbiConfig,
        granularity: ProfileGranularity,
    ) -> Self {
        let dispatch_encoding = DispatchEncoding::new(program, portal);
        let mut sites = ProfileSiteTable::with_root("BFC artifact");
        if matches!(granularity, ProfileGranularity::Source) {
            sites.set_source_files(
                program
                    .source_files()
                    .iter()
                    .map(|file| bf_profiling::ProfileFile {
                        id: file.id,
                        path: file.path.clone(),
                    })
                    .collect(),
            );
        }
        Self {
            program,
            layouts,
            static_layout,
            portal,
            regions: None,
            dispatch_encoding,
            config,
            output: Vec::new(),
            sites,
            site_stack: vec![bf_profiling::ProfileSiteId(0)],
            instruction_path: Vec::new(),
            granularity,
            position: 0,
            branch_temporary_depth: 0,
            nibble_transfer: false,
            current_source: None,
        }
    }

    fn current_profile_site(&self) -> bf_profiling::ProfileSiteId {
        *self.site_stack.last().expect("root profile site")
    }

    fn with_profile_site<T>(
        &mut self,
        kind: &str,
        stable_key: impl Into<String>,
        label: impl Into<String>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        self.with_profile_attributes(kind, stable_key, label, BTreeMap::new(), emit)
    }

    fn with_profile_attributes<T>(
        &mut self,
        kind: &str,
        stable_key: impl Into<String>,
        label: impl Into<String>,
        attributes: BTreeMap<String, String>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        let site = self.sites.intern(
            Some(self.current_profile_site()),
            kind,
            stable_key,
            label,
            attributes,
        );
        if matches!(self.granularity, ProfileGranularity::Source) {
            self.sites.set_source(
                site,
                self.current_source
                    .map(|source| bf_profiling::ProfileSourceSpan {
                        file_id: source.file_id,
                        start_byte: source.start_byte,
                        end_byte: source.end_byte,
                    }),
            );
        }
        self.site_stack.push(site);
        let result = emit(self);
        self.site_stack.pop();
        result
    }

    fn with_instruction_path<T>(
        &mut self,
        segment: impl Into<String>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        self.instruction_path.push(segment.into());
        let result = emit(self);
        self.instruction_path.pop();
        result
    }

    fn with_source_span<T>(
        &mut self,
        source: Option<SourceSpan>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        let previous = self.current_source;
        self.current_source = source;
        let result = emit(self);
        self.current_source = previous;
        result
    }

    fn instruction_path_key(&self) -> String {
        self.instruction_path.join(".")
    }

    fn emit_operation(&mut self, operation: AnnotatedBfOperation) {
        self.output.push(AnnotatedBfInstruction::new(
            self.current_profile_site(),
            operation,
        ));
    }

    fn emit_loop(&mut self, body: Vec<AnnotatedBfInstruction>) {
        self.emit_operation(AnnotatedBfOperation::Loop(body));
    }

    fn with_profile_site_infallible(
        &mut self,
        kind: &str,
        stable_key: impl Into<String>,
        label: impl Into<String>,
        emit: impl FnOnce(&mut Self),
    ) {
        let site = self.sites.intern(
            Some(self.current_profile_site()),
            kind,
            stable_key,
            label,
            BTreeMap::new(),
        );
        if matches!(self.granularity, ProfileGranularity::Source) {
            self.sites.set_source(
                site,
                self.current_source
                    .map(|source| bf_profiling::ProfileSourceSpan {
                        file_id: source.file_id,
                        start_byte: source.start_byte,
                        end_byte: source.end_byte,
                    }),
            );
        }
        self.site_stack.push(site);
        emit(self);
        self.site_stack.pop();
    }

    fn initialize_main(&mut self, check_capacity: bool) -> Result<(), AbiCodegenError> {
        let function = self.function(self.program.main())?;
        let function_id = function.id();
        let entry = function.entry();
        let frame = self.layout(function_id)?.frame.clone();
        if check_capacity {
            frame.validate_main_capacity(self.static_layout.anchor_head())?;
        }

        let stride = self.config.stride();
        let frame_bottom = self.static_layout.anchor_head() + stride;
        for chunk in 0..frame.frame_chunks() {
            self.set_raw((frame_bottom + chunk * stride) as isize, 1);
        }
        let context_base = frame_bottom + (frame.frame_chunks() - frame.context_chunks()) * stride;
        self.set_raw(
            (context_base as isize) + frame.abi_offset(AbiField::Active),
            1,
        );
        self.set_pc_raw(context_base as isize, entry);
        self.move_to(context_base as isize + frame.abi_offset(AbiField::Active));

        // Rebase bookkeeping without moving the runtime pointer.
        self.position = frame.abi_offset(AbiField::Active);
        Ok(())
    }

    fn emit_dispatcher(&mut self) -> Result<(), AbiCodegenError> {
        let body = self.capture(|emitter| {
            emitter.move_to(0);
            let mut pages = BTreeMap::<u8, Vec<DispatchEntry<'_>>>::new();
            for continuation in emitter.program.continuations() {
                if emitter
                    .regions
                    .is_some_and(|plan| !plan.entries.contains(&continuation.id()))
                {
                    continue;
                }
                let encoded = emitter.dispatch_encoding.encode(continuation.id());
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::Continuation(continuation));
            }
            for &accessor in &emitter.portal.accessors {
                let encoded = emitter.dispatch_encoding.encode(accessor.id);
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::PortalAccessor(accessor));
            }
            for &site in &emitter.portal.ordered_sites {
                let encoded = emitter.dispatch_encoding.encode(site.resume);
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::PortalResume(site));
            }
            for &router in &emitter.portal.routers {
                let encoded = emitter.dispatch_encoding.encode(router.id);
                pages
                    .entry((encoded >> 8) as u8)
                    .or_default()
                    .push(DispatchEntry::GlobalPortalRouter(router));
            }
            let pages = pages
                .into_iter()
                .map(|(high, mut entries)| {
                    // A dispatched body can migrate to a fresh context whose PcLow
                    // is zero until the end of this dispatcher cycle. Keeping the
                    // zero case first prevents that fresh context from being
                    // mistaken for continuation xx00 later in the same page.
                    entries.sort_by_key(|entry| emitter.dispatch_encoding.encode(entry.id()) as u8);
                    (high, entries)
                })
                .collect::<Vec<_>>();
            let high_span = usize::from(
                pages.last().expect("nonempty dispatcher").0
                    - pages.first().expect("nonempty dispatcher").0,
            ) + 1;
            if high_span == pages.len() {
                emitter.with_profile_site(
                    "abi",
                    "abi.dispatch.pages.countdown",
                    "dispatch page countdown",
                    |emitter| emitter.emit_page_countdown(&pages),
                )?;
            } else {
                emitter.with_profile_site(
                    "abi",
                    "abi.dispatch.pages.compare",
                    "dispatch page equality scan",
                    |emitter| {
                        for (high, entries) in &pages {
                            emitter.emit_dispatch_page(*high, entries)?;
                        }
                        Ok(())
                    },
                )?;
            }
            emitter.move_abi_field(AbiField::NextPcLow, AbiField::PcLow);
            emitter.move_abi_field(AbiField::NextPcHigh, AbiField::PcHigh);
            let active = emitter.current_abi_offset(AbiField::Active)?;
            emitter.move_to(active);
            Ok(())
        })?;
        self.emit_loop(body);
        Ok(())
    }

    fn emit_dispatch_page(
        &mut self,
        high: u8,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            format!("abi.dispatch.page.{high}"),
            format!("dispatch page {high}"),
            |emitter| emitter.emit_dispatch_page_inner(high, entries),
        )
    }

    fn emit_dispatch_page_inner(
        &mut self,
        high: u8,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            "abi.dispatch.page.select",
            "dispatch page selector",
            |emitter| {
                emitter.copy_abi_field(AbiField::PcHigh, AbiField::Condition)?;
                emitter.add_abi_field(AbiField::Condition, 0_u8.wrapping_sub(high))?;
                emitter.set_abi_field(AbiField::Branch, 1)?;
                emitter.clear_branch_on_nonzero(AbiField::Condition)
            },
        )?;

        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            // Once a page matches, make every later equality page fail.
            emitter.clear_abi_field(AbiField::PcHigh)?;
            emitter.emit_dispatch_page_body(entries)?;
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn emit_page_countdown(
        &mut self,
        pages: &[(u8, Vec<DispatchEntry<'a>>)],
    ) -> Result<(), AbiCodegenError> {
        let minimum = pages.first().expect("nonempty dispatcher").0;
        let maximum = pages.last().expect("nonempty dispatcher").0;
        let mut cases = [None; 256];
        for (index, (high, _)) in pages.iter().enumerate() {
            cases[usize::from(high.wrapping_sub(minimum))] = Some(index);
        }
        self.add_abi_field(AbiField::PcHigh, 0_u8.wrapping_sub(minimum))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.emit_page_countdown_level(0, maximum - minimum, &cases, pages)
    }

    fn emit_page_countdown_level(
        &mut self,
        level: u8,
        maximum: u8,
        cases: &[Option<usize>; 256],
        pages: &[(u8, Vec<DispatchEntry<'a>>)],
    ) -> Result<(), AbiCodegenError> {
        let pc = self.current_abi_offset(AbiField::PcHigh)?;
        self.move_to(pc);
        let nonzero = self.capture(|emitter| {
            if level == maximum {
                emitter.clear_current();
                emitter.clear_abi_field(AbiField::Branch)?;
            } else {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_page_countdown_level(level + 1, maximum, cases, pages)?;
            }
            emitter.move_to(pc);
            Ok(())
        })?;
        self.emit_loop(nonzero);
        self.move_to(0);

        if let Some(index) = cases[usize::from(level)] {
            let (high, entries) = &pages[index];
            self.with_profile_site(
                "abi",
                format!("abi.dispatch.page.{high}"),
                format!("dispatch page {high}"),
                |emitter| emitter.emit_page_countdown_case(Some(entries)),
            )
        } else {
            self.emit_page_countdown_case(None)
        }
    }

    fn emit_page_countdown_case(
        &mut self,
        entries: Option<&[DispatchEntry<'a>]>,
    ) -> Result<(), AbiCodegenError> {
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            if let Some(entries) = entries {
                emitter.emit_dispatch_page_body(entries)?;
            }
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn emit_dispatch_page_body(
        &mut self,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        let low_span = usize::from(
            (self
                .dispatch_encoding
                .encode(entries.last().expect("nonempty dispatch page").id()) as u8)
                - (self
                    .dispatch_encoding
                    .encode(entries.first().expect("nonempty dispatch page").id())
                    as u8),
        ) + 1;
        if low_span == entries.len() {
            self.with_profile_site(
                "abi",
                "abi.dispatch.page.countdown",
                "dispatch page countdown",
                |emitter| emitter.emit_countdown_dispatch(entries),
            )
        } else {
            self.with_profile_site(
                "abi",
                "abi.dispatch.page.compare",
                "dispatch page equality scan",
                |emitter| emitter.emit_equality_dispatch(entries),
            )
        }
    }

    fn emit_countdown_dispatch(
        &mut self,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        let minimum = entries
            .first()
            .map(|entry| self.dispatch_encoding.encode(entry.id()) as u8)
            .expect("every dispatch page contains at least one entry");
        let maximum = entries
            .last()
            .map(|entry| self.dispatch_encoding.encode(entry.id()) as u8)
            .expect("every dispatch page contains at least one entry");
        let mut cases = [None; 256];
        for &entry in entries {
            let relative = (self.dispatch_encoding.encode(entry.id()) as u8).wrapping_sub(minimum);
            cases[usize::from(relative)] = Some(entry);
        }
        // Normalize the page's occupied low-byte range to zero. Values below
        // `minimum` wrap above the range, so sparse or invalid PCs still reach
        // the no-match path rather than aliasing a case.
        self.add_abi_field(AbiField::PcLow, 0_u8.wrapping_sub(minimum))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.emit_countdown_level(0, maximum - minimum, &cases)
    }

    fn emit_equality_dispatch(
        &mut self,
        entries: &[DispatchEntry<'a>],
    ) -> Result<(), AbiCodegenError> {
        for &entry in entries {
            self.with_profile_site(
                "abi",
                format!("abi.dispatch.case.{}", entry.id().get()),
                format!("dispatch case {}", entry.id().get()),
                |emitter| emitter.emit_equality_case(entry),
            )?;
        }
        Ok(())
    }

    fn emit_equality_case(&mut self, entry: DispatchEntry<'a>) -> Result<(), AbiCodegenError> {
        let id = self.dispatch_encoding.encode(entry.id());
        self.copy_abi_field(AbiField::PcLow, AbiField::Condition)?;
        self.add_abi_field(AbiField::Condition, 0_u8.wrapping_sub(id as u8))?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;

        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.clear_abi_field(AbiField::PcLow)?;
            emitter.emit_dispatch_entry_body(entry)?;
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_dispatch_entry_gate(entry.id(), body)?;
        self.move_to(0);
        Ok(())
    }

    fn emit_countdown_level(
        &mut self,
        level: u8,
        maximum: u8,
        cases: &[Option<DispatchEntry<'a>>; 256],
    ) -> Result<(), AbiCodegenError> {
        let pc = self.current_abi_offset(AbiField::PcLow)?;
        self.move_to(pc);
        let nonzero = self.capture(|emitter| {
            if level == maximum {
                // No case can match above the last low byte in this page.
                emitter.clear_current();
                emitter.clear_abi_field(AbiField::Branch)?;
            } else {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_countdown_level(level + 1, maximum, cases)?;
            }
            emitter.move_to(pc);
            Ok(())
        })?;
        self.emit_loop(nonzero);
        self.move_to(0);

        let entry = cases[usize::from(level)];
        if let Some(entry) = entry {
            self.with_profile_site(
                "abi",
                format!("abi.dispatch.case.{}", entry.id().get()),
                format!("dispatch case {}", entry.id().get()),
                |emitter| emitter.emit_countdown_case(Some(entry)),
            )
        } else {
            self.emit_countdown_case(None)
        }
    }

    fn emit_countdown_case(
        &mut self,
        entry: Option<DispatchEntry<'a>>,
    ) -> Result<(), AbiCodegenError> {
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            if let Some(entry) = entry {
                emitter.emit_dispatch_entry_body(entry)?;
            }
            let branch = emitter.current_abi_offset(AbiField::Branch)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        if let Some(entry) = entry {
            self.emit_dispatch_entry_gate(entry.id(), body)?;
        } else {
            self.emit_loop(body);
        }
        self.move_to(0);
        Ok(())
    }

    /// Attribute only the one-shot gate brackets, so loop iterations measure
    /// actual BF dispatcher visits independently of the emitted body shape.
    fn emit_dispatch_entry_gate(
        &mut self,
        id: ContinuationId,
        body: Vec<AnnotatedBfInstruction>,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            format!("abi.dispatch.enter.{}", id.get()),
            "dispatcher entry",
            |emitter| {
                emitter.emit_loop(body);
                Ok(())
            },
        )
    }

    fn emit_dispatch_entry_body(
        &mut self,
        entry: DispatchEntry<'a>,
    ) -> Result<(), AbiCodegenError> {
        match entry {
            DispatchEntry::Continuation(continuation) => {
                if let Some(region) = self
                    .regions
                    .and_then(|plan| plan.regions.get(&continuation.id()))
                {
                    self.emit_region(region, continuation.function())
                } else {
                    self.emit_continuation_body(continuation)
                }
            }
            DispatchEntry::PortalAccessor(accessor) => {
                self.emit_aggregate_accessor(accessor)?;
                self.move_abi_field(AbiField::ReturnPcLow, AbiField::NextPcLow);
                self.move_abi_field(AbiField::ReturnPcHigh, AbiField::NextPcHigh);
                Ok(())
            }
            DispatchEntry::PortalResume(site) => self.emit_portal_resume(site),
            DispatchEntry::GlobalPortalRouter(router) => self.emit_global_portal_router(router),
        }
    }

    fn emit_continuation_body(
        &mut self,
        continuation: &Continuation,
    ) -> Result<(), AbiCodegenError> {
        self.with_continuation_site(continuation, |emitter| {
            emitter.emit_continuation_body_inner(continuation)
        })
    }

    fn with_continuation_site(
        &mut self,
        continuation: &Continuation,
        emit: impl FnOnce(&mut Self) -> Result<(), AbiCodegenError>,
    ) -> Result<(), AbiCodegenError> {
        if matches!(self.granularity, ProfileGranularity::Abi) {
            emit(self)
        } else {
            let function = continuation.function().index();
            let continuation_id = continuation.id().get();
            self.with_source_span(continuation.primary_source(), |emitter| {
                emitter.with_profile_site(
                    "function",
                    format!("function.{function}"),
                    format!("function {function}"),
                    |emitter| {
                        emitter.with_profile_site(
                            "continuation",
                            format!("function.{function}.continuation.{continuation_id}"),
                            format!("continuation {continuation_id}"),
                            emit,
                        )
                    },
                )
            })
        }
    }

    fn emit_continuation_body_inner(
        &mut self,
        continuation: &Continuation,
    ) -> Result<(), AbiCodegenError> {
        self.branch_temporary_depth = 0;
        self.emit_all(
            continuation.body(),
            continuation.function(),
            Some(continuation.body_sources()),
        )?;
        self.with_source_span(continuation.terminator_source(), |emitter| {
            emitter.emit_terminator(continuation)
        })
    }

    fn clear_branch_on_nonzero(&mut self, condition: AbiField) -> Result<(), AbiCodegenError> {
        let condition = self.current_abi_offset(condition)?;
        self.move_to(condition);
        let body = self.capture(|emitter| {
            emitter.clear_current();
            emitter.clear_abi_field(AbiField::Branch)?;
            emitter.move_to(condition);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn emit_all(
        &mut self,
        instructions: &[FrameInstruction],
        function: FunctionId,
        sources: Option<&[Option<SourceSpan>]>,
    ) -> Result<(), AbiCodegenError> {
        let mut index = 0;
        while index < instructions.len() {
            let FrameInstruction::Set {
                dst:
                    Address::ArrayElement {
                        array,
                        index: first,
                    },
                value: 0,
            } = instructions[index]
            else {
                let source = sources
                    .and_then(|sources| sources.get(index))
                    .copied()
                    .flatten()
                    .or(self.current_source);
                self.with_source_span(source, |emitter| {
                    emitter.with_instruction_path(index.to_string(), |emitter| {
                        emitter.emit_instruction(&instructions[index], function)
                    })
                })?;
                index += 1;
                continue;
            };
            let mut end = index + 1;
            while let Some(FrameInstruction::Set {
                dst:
                    Address::ArrayElement {
                        array: candidate,
                        index: element,
                    },
                value: 0,
            }) = instructions.get(end)
            {
                if *candidate != array || *element != first + (end - index) {
                    break;
                }
                end += 1;
            }
            if end - index >= 2 && array != AggregateRegion::Outbox {
                self.emit_aggregate_clear_range(
                    array,
                    first,
                    function,
                    &instructions[index..end],
                    index,
                    sources,
                )?;
            } else {
                for (offset, instruction) in instructions[index..end].iter().enumerate() {
                    let source = sources
                        .and_then(|sources| sources.get(index + offset))
                        .copied()
                        .flatten()
                        .or(self.current_source);
                    self.with_source_span(source, |emitter| {
                        emitter.with_instruction_path((index + offset).to_string(), |emitter| {
                            emitter.emit_instruction(instruction, function)
                        })
                    })?;
                }
            }
            index = end;
        }
        Ok(())
    }

    fn emit_aggregate_clear_range(
        &mut self,
        region: AggregateRegion,
        first: usize,
        function: FunctionId,
        instructions: &[FrameInstruction],
        instruction_start: usize,
        sources: Option<&[Option<SourceSpan>]>,
    ) -> Result<(), AbiCodegenError> {
        let base = self.portal_base_location(region, function)?;
        self.move_context_to_location(base);
        for (instruction_offset, instruction) in instructions.iter().enumerate() {
            let index = first + instruction_offset;
            let mut offset = aggregate_element_physical_offset(index, self.config)? as isize;
            // Global navigation rebases emitter bookkeeping at the aggregate
            // head. A frame-relative move retains the caller-context origin,
            // so keep the base displacement in that case.
            if let Location::Relative(base) = base {
                offset += base;
            }
            let source = sources
                .and_then(|sources| sources.get(instruction_start + instruction_offset))
                .copied()
                .flatten()
                .or(self.current_source);
            self.with_source_span(source, |emitter| {
                emitter.with_instruction_path(
                    (instruction_start + instruction_offset).to_string(),
                    |emitter| {
                        emitter.emit_instruction_site(instruction, function, |emitter| {
                            emitter.with_profile_site(
                                "abi",
                                "abi.frame.set",
                                "frame set",
                                |emitter| {
                                    emitter.clear(offset);
                                    Ok(())
                                },
                            )
                        })
                    },
                )
            })?;
        }
        self.move_location_to_context(base);
        Ok(())
    }

    fn emit_instruction(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        self.emit_instruction_site(instruction, function, |emitter| {
            emitter.emit_instruction_with_abi_site(instruction, function)
        })
    }

    fn emit_instruction_site<T>(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        if matches!(
            self.granularity,
            ProfileGranularity::Abi | ProfileGranularity::Continuation
        ) {
            emit(self)
        } else if matches!(self.granularity, ProfileGranularity::Source)
            && let Some(source) = self.current_source
        {
            // Source maps are intended to answer which source operation is
            // hot, not to repeat a million frame-instruction leaves. Keep
            // the ABI child sites below this source site, but intern all
            // instructions from the same source span together.
            self.with_profile_site(
                "source",
                format!(
                    "source.{}.{}.{}",
                    source.file_id, source.start_byte, source.end_byte
                ),
                format!(
                    "source file {} bytes {}..{}",
                    source.file_id, source.start_byte, source.end_byte
                ),
                emit,
            )
        } else {
            self.with_profile_site(
                "frame_instruction",
                format!(
                    "function.{}.frame_instruction.{}.{}",
                    function.index(),
                    self.instruction_path_key(),
                    instruction_kind(instruction)
                ),
                instruction_kind(instruction),
                emit,
            )
        }
    }

    fn emit_instruction_with_abi_site(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let kind = instruction_kind(instruction);
        self.with_profile_site(
            "abi",
            format!("abi.frame.{kind}"),
            format!("frame {kind}"),
            |emitter| emitter.emit_instruction_inner(instruction, function),
        )
    }

    fn emit_instruction_inner(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        match instruction {
            FrameInstruction::SubWithBorrow {
                left,
                right,
                difference,
                borrow,
                true_value,
                false_value,
            } => {
                self.emit_compare(
                    *left,
                    *right,
                    *borrow,
                    *true_value,
                    *false_value,
                    Some(*difference),
                    function,
                )?;
            }
            FrameInstruction::Compare {
                left,
                right,
                dst,
                true_value,
                false_value,
            } => {
                self.emit_compare(
                    *left,
                    *right,
                    *dst,
                    *true_value,
                    *false_value,
                    None,
                    function,
                )?;
            }
            FrameInstruction::Set { dst, value } => {
                let dst = self.address_location(*dst, function)?;
                self.set_location(dst, *value);
            }
            FrameInstruction::AddConst { dst, value } => {
                let dst = self.address_location(*dst, function)?;
                self.move_context_to_location(dst);
                self.adjust(*value);
                self.move_location_to_context(dst);
            }
            FrameInstruction::Copy { src, dst } => {
                let source_address = *src;
                let src = self.address_location(source_address, function)?;
                let dst = self.address_location(*dst, function)?;
                let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
                if self.nibble_transfer
                    && matches!(source_address, Address::Global(_))
                    && let (Location::Global(source), Location::Relative(destination)) = (src, dst)
                {
                    self.copy_global_to_relative_bits(source, destination);
                } else {
                    self.copy_locations(src, dst, restore);
                }
            }
            FrameInstruction::Transfer { src, targets } => {
                self.transfer(*src, targets, function)?;
            }
            FrameInstruction::AggregateCopy { src, dst, cells } => {
                self.aggregate_copy(*src, *dst, *cells, function)?;
            }
            FrameInstruction::Input { dst } => {
                let dst = self.address_location(*dst, function)?;
                self.move_context_to_location(dst);
                self.emit_operation(AnnotatedBfOperation::Input);
                self.move_location_to_context(dst);
            }
            FrameInstruction::Output { src } => {
                let src = self.address_location(*src, function)?;
                self.move_context_to_location(src);
                self.emit_operation(AnnotatedBfOperation::Output);
                self.move_location_to_context(src);
            }
            FrameInstruction::Loop { condition, body } => {
                let condition = self.address_location(*condition, function)?;
                self.move_context_to_location(condition);
                let body = self.capture(|emitter| {
                    emitter.move_location_to_context(condition);
                    emitter.with_instruction_path("loop", |emitter| {
                        emitter.emit_all(body, function, None)
                    })?;
                    emitter.move_context_to_location(condition);
                    Ok(())
                })?;
                self.emit_loop(body);
                self.move_location_to_context(condition);
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => self.emit_structured_branch(*condition, then_body, else_body, function)?,
        }
        Ok(())
    }

    /// Destructive unsigned comparison on four adjacent ABI scratch cells:
    /// L (right operand), E (initially 1), B (zero), R (left operand).
    /// `[>>>[-<]<<-]` exits at L when right <= left, or at E otherwise.
    /// Swapping the operands makes the second path exactly left < right,
    /// without a separate equality test. Join the paths after the countdown.
    /// Scratch0..3 are contiguous, inaccessible as
    /// public operand addresses, and all zero on exit. The context origin and
    /// surrounding structured-branch flags are preserved.
    #[allow(clippy::too_many_arguments)]
    fn emit_compare(
        &mut self,
        left: Address,
        right: Address,
        dst: Address,
        true_value: u8,
        false_value: u8,
        difference: Option<Address>,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let left = self.address_location(left, function)?;
        let right = self.address_location(right, function)?;
        let dst = self.address_location(dst, function)?;
        let difference = difference
            .map(|address| self.address_location(address, function))
            .transpose()?;
        // Restore is private ABI scratch, separate from structured-branch flags.
        // Retain the countdown remainder here when both outputs are requested.
        let remainder = self.current_abi_offset(AbiField::Restore)?;
        let l = self.current_abi_offset(AbiField::Scratch0)?;
        // Adjacent scratch cells: right operand, flag, zero, left operand.
        let e = self.current_abi_offset(AbiField::Scratch1)?;
        let b = self.current_abi_offset(AbiField::Scratch2)?;
        let r = self.current_abi_offset(AbiField::Scratch3)?;
        debug_assert_eq!([e - l, b - e, r - b], [1, 1, 1]);
        if left == right {
            self.copy_locations(right, Location::Relative(l), Location::Relative(e));
        } else {
            self.move_location_to_zero(right, Location::Relative(l));
        }
        self.move_location_to_zero(left, Location::Relative(r));
        if difference.is_some() {
            self.clear(remainder);
        }
        self.move_to(e);
        self.adjust(1);
        self.move_to(l);
        let countdown = self.capture_infallible(|emitter| {
            emitter.move_to(r);
            let nonzero = emitter.capture_infallible(|emitter| {
                emitter.adjust(255);
                emitter.move_to(b);
            });
            emitter.emit_loop(nonzero);
            // R nonzero exits at B; R zero stays at R. Subtract from L or E.
            emitter.push_move(-2);
            emitter.position = l;
            emitter.adjust(255);
        });
        self.emit_loop(countdown);
        // Exit at L=0 for right <= left, otherwise E=0 with L=right-left.
        self.push_move(1);
        self.position = e;
        let not_greater = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            if false_value != 0 {
                emitter.adjust(false_value);
            }
            emitter.move_to(r);
            if difference.is_some() {
                emitter.move_value(r, remainder);
            } else {
                emitter.clear_current();
            }
            emitter.move_to(b);
        });
        self.emit_loop(not_greater);
        self.position = b;
        self.move_to(l);
        let greater = self.capture_infallible(|emitter| {
            if difference.is_some() {
                let subtract = emitter.capture_infallible(|emitter| {
                    emitter.adjust(255);
                    emitter.move_to(remainder);
                    emitter.adjust(255);
                    emitter.move_to(l);
                });
                emitter.emit_loop(subtract);
            } else {
                emitter.clear_current();
            }
            if true_value != 0 {
                emitter.move_to(e);
                emitter.adjust(true_value);
            }
            emitter.move_to(l);
        });
        self.emit_loop(greater);
        self.move_to(0);
        if let Some(difference) = difference {
            self.move_location(Location::Relative(remainder), difference);
        }
        self.move_location(Location::Relative(e), dst);
        Ok(())
    }

    fn emit_structured_branch(
        &mut self,
        condition: Address,
        then_body: &[FrameInstruction],
        else_body: &[FrameInstruction],
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let condition = self.address_location(condition, function)?;

        // A missing else arm needs no flag: the condition cell itself gates
        // the then arm.  Clearing it before and after the body preserves the
        // destructive Branch contract even when the body writes it again.
        if else_body.is_empty() {
            self.move_context_to_location(condition);
            let then_loop = self.capture(|emitter| {
                emitter.clear_current();
                emitter.move_location_to_context(condition);
                emitter.with_instruction_path("then", |emitter| {
                    emitter.emit_all(then_body, function, None)
                })?;
                emitter.clear_location(condition);
                emitter.move_context_to_location(condition);
                Ok(())
            })?;
            self.emit_loop(then_loop);
            self.move_location_to_context(condition);
            return Ok(());
        }

        let flag = Location::Relative(self.acquire_branch_temporary(function)?);
        self.set_location(flag, 1);

        self.move_context_to_location(condition);
        let then_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(condition);
            emitter.with_instruction_path("then", |emitter| {
                emitter.emit_all(then_body, function, None)
            })?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.move_context_to_location(condition);
            Ok(())
        })?;
        self.emit_loop(then_loop);
        self.move_location_to_context(condition);

        self.move_context_to_location(flag);
        let else_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(flag);
            emitter.with_instruction_path("else", |emitter| {
                emitter.emit_all(else_body, function, None)
            })?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.move_context_to_location(flag);
            Ok(())
        })?;
        self.emit_loop(else_loop);
        self.move_location_to_context(flag);
        self.branch_temporary_depth -= 1;
        Ok(())
    }

    /// Emit a terminal branch whose arms contain only frame instructions.
    /// The selected arm can therefore run before the next-PC write without a
    /// context migration.  This is the backend counterpart of
    /// `Terminator::BranchWithBodies`.
    fn emit_branch_with_bodies(
        &mut self,
        condition: Address,
        then_body: &[FrameInstruction],
        then_target: ContinuationId,
        else_body: &[FrameInstruction],
        else_target: ContinuationId,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let condition = self.address_location(condition, function)?;
        if else_body.is_empty() {
            self.set_next_pc(else_target)?;
            self.move_context_to_location(condition);
            let body = self.capture(|emitter| {
                emitter.clear_current();
                emitter.move_location_to_context(condition);
                emitter.with_instruction_path("then", |emitter| {
                    emitter.emit_all(then_body, function, None)
                })?;
                emitter.clear_location(condition);
                emitter.set_next_pc(then_target)?;
                emitter.move_context_to_location(condition);
                Ok(())
            })?;
            self.emit_loop(body);
            self.move_location_to_context(condition);
            return Ok(());
        }

        self.set_next_pc(then_target)?;
        if then_body.is_empty() {
            let flag = Location::Relative(self.acquire_branch_temporary(function)?);
            self.set_location(flag, 1);

            // A nonzero condition suppresses the else arm.
            self.move_context_to_location(condition);
            let condition_gate = self.capture(|emitter| {
                emitter.clear_current();
                emitter.move_location_to_context(condition);
                emitter.clear_location(flag);
                emitter.clear_location(condition);
                emitter.move_context_to_location(condition);
                Ok(())
            })?;
            self.emit_loop(condition_gate);
            self.move_location_to_context(condition);

            self.move_context_to_location(flag);
            let else_loop = self.capture(|emitter| {
                emitter.clear_current();
                emitter.move_location_to_context(flag);
                emitter.with_instruction_path("else", |emitter| {
                    emitter.emit_all(else_body, function, None)
                })?;
                emitter.clear_location(condition);
                emitter.clear_location(flag);
                emitter.set_next_pc(else_target)?;
                emitter.move_context_to_location(flag);
                Ok(())
            })?;
            self.emit_loop(else_loop);
            self.move_location_to_context(flag);
            self.branch_temporary_depth -= 1;
            return Ok(());
        }

        let flag = Location::Relative(self.acquire_branch_temporary(function)?);
        self.set_location(flag, 1);

        self.move_context_to_location(condition);
        let then_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(condition);
            emitter.with_instruction_path("then", |emitter| {
                emitter.emit_all(then_body, function, None)
            })?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.set_next_pc(then_target)?;
            emitter.move_context_to_location(condition);
            Ok(())
        })?;
        self.emit_loop(then_loop);
        self.move_location_to_context(condition);

        self.move_context_to_location(flag);
        let else_loop = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(flag);
            emitter.with_instruction_path("else", |emitter| {
                emitter.emit_all(else_body, function, None)
            })?;
            emitter.clear_location(condition);
            emitter.clear_location(flag);
            emitter.set_next_pc(else_target)?;
            emitter.move_context_to_location(flag);
            Ok(())
        })?;
        self.emit_loop(else_loop);
        self.move_location_to_context(flag);
        self.branch_temporary_depth -= 1;
        Ok(())
    }

    fn emit_terminator(&mut self, continuation: &Continuation) -> Result<(), AbiCodegenError> {
        if matches!(
            self.granularity,
            ProfileGranularity::Abi | ProfileGranularity::Continuation
        ) {
            self.emit_terminator_inner(continuation)
        } else {
            self.with_profile_site(
                "terminator",
                format!(
                    "function.{}.terminator.{}",
                    continuation.function().index(),
                    terminator_kind(continuation.terminator())
                ),
                terminator_kind(continuation.terminator()),
                |emitter| emitter.emit_terminator_inner(continuation),
            )
        }
    }

    fn emit_terminator_inner(
        &mut self,
        continuation: &Continuation,
    ) -> Result<(), AbiCodegenError> {
        match continuation.terminator() {
            Terminator::Goto { target } => self.set_next_pc(*target)?,
            Terminator::Branch {
                condition,
                then_target,
                else_target,
            } => {
                self.set_next_pc(*else_target)?;
                let condition = self.address_location(*condition, continuation.function())?;
                self.move_context_to_location(condition);
                let body = self.capture(|emitter| {
                    emitter.clear_current();
                    emitter.move_location_to_context(condition);
                    emitter.set_next_pc(*then_target)?;
                    emitter.move_context_to_location(condition);
                    Ok(())
                })?;
                self.emit_loop(body);
                self.move_location_to_context(condition);
            }
            Terminator::BranchWithBodies {
                condition,
                then_body,
                then_target,
                else_body,
                else_target,
            } => self.emit_branch_with_bodies(
                *condition,
                then_body,
                *then_target,
                else_body,
                *else_target,
                continuation.function(),
            )?,
            Terminator::Call {
                callee,
                arguments,
                return_to,
            } => self.emit_call(continuation.function(), *callee, arguments, *return_to)?,
            Terminator::Return { value } => self.emit_return(continuation.function(), *value)?,
            Terminator::ArrayLoad {
                array: _,
                index: _,
                destination: _,
                return_to: _,
            }
            | Terminator::ArrayStore { .. }
            | Terminator::AggregateLoad { .. }
            | Terminator::AggregateStore { .. } => self.emit_portal_start(
                *self
                    .portal
                    .sites
                    .get(&continuation.id())
                    .expect("every validated portal terminator has a portal site"),
            )?,
            Terminator::Abort | Terminator::Halt => self.clear_abi_field(AbiField::Active)?,
        }
        Ok(())
    }

    fn emit_call(
        &mut self,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.call", "ABI call", |emitter| {
            emitter.emit_call_inner(caller, callee, arguments, return_to)
        })
    }

    fn emit_call_inner(
        &mut self,
        caller: FunctionId,
        callee: FunctionId,
        arguments: &[ValueOperand],
        return_to: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let callee_function = self.function(callee)?;
        let parameters = callee_function
            .parameter_locations()
            .iter()
            .map(|parameter| {
                let cells = match parameter {
                    ParameterLocation::Cell(_) | ParameterLocation::AggregateElement { .. } => None,
                    ParameterLocation::Array(array) | ParameterLocation::Aggregate(array) => Some(
                        callee_function
                            .frame_aggregate(*array)
                            .expect("validated aggregate parameter")
                            .cells(),
                    ),
                };
                (*parameter, cells)
            })
            .collect::<Vec<_>>();
        let entry = callee_function.entry();
        let callee_frame = self.layout(callee)?.frame.clone();
        let caller_context_chunks = self.layout(caller)?.frame.context_chunks();
        let stride = self.config.stride();
        let callee_context_delta = (callee_frame.frame_chunks() * stride) as isize;
        let caller_frontier = (caller_context_chunks * stride) as isize;

        // Each copy restores its caller source. Repeating an Address for
        // multiple parameters therefore has the same value semantics as the
        // source language's left-to-right, already-evaluated argument list.
        // Copy before marking callee heads: while a global source is visited,
        // anchor-to-frontier normalization must still return to the caller.
        // Returned frame data is zero, so writing the future parameter region
        // before allocation is safe.
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        if !arguments.is_empty() {
            self.clear_location(restore);
        }
        for (&argument, &(parameter, parameter_cells)) in arguments.iter().zip(&parameters) {
            match (argument, parameter) {
                (ValueOperand::Cell(argument), ParameterLocation::Cell(parameter)) => {
                    let src = self.address_location(argument, caller)?;
                    let dst = Location::Relative(
                        callee_context_delta + callee_frame.frame_offset(parameter),
                    );
                    self.copy_locations_to_zeroed_destination(src, dst, restore);
                }
                (
                    ValueOperand::Cell(argument),
                    ParameterLocation::AggregateElement { aggregate, index },
                ) => {
                    let src = self.address_location(argument, caller)?;
                    let dst = Location::Relative(
                        callee_context_delta
                            + callee_frame.aggregate_element_offset(aggregate, index)?,
                    );
                    self.copy_locations_to_zeroed_destination(src, dst, restore);
                }
                (
                    ValueOperand::Array(_) | ValueOperand::Aggregate { .. },
                    ParameterLocation::Array(parameter) | ParameterLocation::Aggregate(parameter),
                ) => {
                    let cells = parameter_cells.expect("aggregate parameter size");
                    for index in 0..cells {
                        let src = self.value_operand_element_location(argument, index, caller)?;
                        let dst = Location::Relative(
                            callee_context_delta
                                + callee_frame.aggregate_element_offset(parameter, index)?,
                        );
                        self.copy_locations_to_zeroed_destination(src, dst, restore);
                    }
                }
                _ => unreachable!("validated call operand and parameter types must match"),
            }
        }

        // Returned frames are all-zero, so allocation only marks their heads.
        // The first callee head is the caller's current frontier.
        for chunk in 0..callee_frame.frame_chunks() {
            self.set(caller_frontier + (chunk * stride) as isize, 1);
        }

        // Context initialization deliberately uses only the common ABI fields;
        // parameters have ordinary scalar/array storage below this context.
        for field in AbiField::ALL {
            self.clear(callee_context_delta + callee_frame.abi_offset(field));
        }
        self.set(
            callee_context_delta + callee_frame.abi_offset(AbiField::Active),
            1,
        );
        self.set_pc_at(
            callee_context_delta,
            AbiField::NextPcLow,
            AbiField::NextPcHigh,
            entry,
        );
        self.set_pc_at(
            callee_context_delta,
            AbiField::ReturnPcLow,
            AbiField::ReturnPcHigh,
            return_to,
        );

        self.migrate_context(callee_context_delta);
        Ok(())
    }

    fn emit_return(
        &mut self,
        callee: FunctionId,
        value: Option<ValueOperand>,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.return", "ABI return", |emitter| {
            emitter.emit_return_inner(callee, value)
        })
    }

    fn emit_return_inner(
        &mut self,
        callee: FunctionId,
        value: Option<ValueOperand>,
    ) -> Result<(), AbiCodegenError> {
        let callee_frame = self.layout(callee)?.frame.clone();
        let stride = self.config.stride();
        let caller_delta = -((callee_frame.frame_chunks() * stride) as isize);
        let caller_value =
            Location::Relative(caller_delta + callee_frame.abi_offset(AbiField::Value));
        let restore = Location::Relative(callee_frame.abi_offset(AbiField::Restore));

        match value {
            Some(ValueOperand::Cell(value)) => {
                let value = self.address_location(value, callee)?;
                let callee_value = Location::Relative(callee_frame.abi_offset(AbiField::Value));
                self.copy_locations(value, callee_value, restore);
                self.move_location(callee_value, caller_value);
            }
            Some(operand @ (ValueOperand::Array(_) | ValueOperand::Aggregate { .. })) => {
                let cells = match self.function(callee)?.return_type() {
                    ValueType::Array(cells) | ValueType::Aggregate { cells } => cells,
                    _ => unreachable!("validated aggregate return type"),
                };
                self.clear_location(restore);
                for index in 0..cells {
                    let src = self.value_operand_element_location(operand, index, callee)?;
                    let dst = Location::Relative(caller_delta + self.common_outbox_offset(index));
                    self.copy_locations_with_zeroed_restore(src, dst, restore);
                }
                self.clear_location(caller_value);
            }
            None => self.clear_location(caller_value),
        }

        self.move_value(
            callee_frame.abi_offset(AbiField::ReturnPcLow),
            caller_delta + callee_frame.abi_offset(AbiField::NextPcLow),
        );
        self.move_value(
            callee_frame.abi_offset(AbiField::ReturnPcHigh),
            caller_delta + callee_frame.abi_offset(AbiField::NextPcHigh),
        );

        // The bottom head is below the context by every non-context chunk.
        // Clearing data as well as flags establishes the allocation invariant
        // needed by the next activation, including recursive calls.
        let frame_bottom =
            -(((callee_frame.frame_chunks() - callee_frame.context_chunks()) * stride) as isize);
        for chunk in 0..callee_frame.frame_chunks() {
            let head = frame_bottom + (chunk * stride) as isize;
            self.clear(head);
            for data in 0..self.config.chunk_cells() {
                self.clear(head + 1 + data as isize);
            }
        }

        self.migrate_context(caller_delta);
        Ok(())
    }

    fn transfer(
        &mut self,
        src: Address,
        targets: &[FrameTransferTarget],
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let src = self.address_location(src, function)?;
        if targets.is_empty() {
            self.clear_location(src);
            return Ok(());
        }
        let targets = targets
            .iter()
            .map(|target| Ok((self.address_location(target.dst, function)?, target.factor)))
            .collect::<Result<Vec<_>, AbiCodegenError>>()?;
        self.move_context_to_location(src);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            for &(dst, factor) in &targets {
                emitter.move_context_to_location(dst);
                emitter.adjust(factor);
                emitter.move_location_to_context(dst);
            }
            emitter.move_context_to_location(src);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_location_to_context(src);
        Ok(())
    }

    fn aggregate_copy(
        &mut self,
        src: ArrayRegion,
        dst: ArrayRegion,
        cells: usize,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        if src == dst {
            return Ok(());
        }
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        self.clear_location(restore);
        for index in 0..cells {
            let src = self.array_element_location(src, index, function)?;
            let dst = self.array_element_location(dst, index, function)?;
            self.copy_locations_with_zeroed_restore(src, dst, restore);
        }
        Ok(())
    }

    fn emit_portal_start(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.portal.start", "portal start", |emitter| {
            emitter.emit_portal_start_inner(site)
        })
    }

    fn emit_portal_start_inner(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        if let PortalOperation::Store { source } = site.operation
            && site.cells > 1
        {
            let layout = self.layout(site.function)?;
            debug_assert!(layout.portal_temporary_cells >= site.cells);
            let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
            self.clear_location(restore);
            for leaf in 0..site.cells {
                let src = self.value_operand_element_location(source, leaf, site.function)?;
                let dst = self.portal_temporary_location(site.function, leaf)?;
                self.copy_locations_with_zeroed_restore(src, dst, restore);
            }
        }
        self.emit_portal_call(site)
    }

    fn emit_portal_call(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        self.with_profile_attributes(
            "abi",
            "abi.portal.request",
            "portal request (static origin)",
            BTreeMap::from([
                ("region".into(), format!("{:?}", site.region)),
                ("function".into(), site.function.index().to_string()),
                ("cells".into(), site.cells.to_string()),
                ("leaf".into(), site.leaf.to_string()),
                (
                    "accessor_encoded".into(),
                    self.dispatch_encoding.encode(site.accessor).to_string(),
                ),
                (
                    "resume_encoded".into(),
                    self.dispatch_encoding.encode(site.resume).to_string(),
                ),
            ]),
            |emitter| emitter.emit_portal_call_inner(site),
        )
    }

    fn emit_portal_call_inner(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        if let Some(router) = site.router {
            return self.stage_global_portal(site, router);
        }
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);

        for field in AbiField::ALL {
            self.clear_location(self.portal_field_location(site.region, field, site.function)?);
        }
        self.clear_location(restore);
        let offset_low = self.portal_field_location(site.region, AbiField::Index, site.function)?;
        let offset_high =
            self.portal_field_location(site.region, AbiField::Scratch0, site.function)?;
        match site.offset {
            PortalOffset::Byte(index) => {
                let source = self.address_location(index, site.function)?;
                self.copy_locations_to_zeroed_destination(source, offset_low, restore);
            }
            PortalOffset::Word(offset) => {
                let low = self.address_location(offset.low, site.function)?;
                let high = self.address_location(offset.high, site.function)?;
                self.copy_locations_to_zeroed_destination(low, offset_low, restore);
                self.copy_locations_to_zeroed_destination(high, offset_high, restore);
            }
        }
        if let PortalOperation::Store { source } = site.operation {
            let value_source = if site.cells > 1 {
                self.portal_temporary_location(site.function, site.leaf)?
            } else {
                self.value_operand_element_location(source, site.leaf, site.function)?
            };
            let value_port =
                self.portal_field_location(site.region, AbiField::Value, site.function)?;
            self.copy_locations_to_zeroed_destination(value_source, value_port, restore);
        }
        self.set_location(
            self.portal_field_location(site.region, AbiField::Active, site.function)?,
            1,
        );
        self.set_pc_locations(
            site.region,
            site.function,
            AbiField::NextPcLow,
            AbiField::NextPcHigh,
            site.accessor,
        )?;
        self.set_pc_locations(
            site.region,
            site.function,
            AbiField::ReturnPcLow,
            AbiField::ReturnPcHigh,
            site.resume,
        )?;
        self.enter_portal(site.region, site.function)
    }

    fn stage_global_portal(
        &mut self,
        site: PortalSite,
        router: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        let route_low = self.route_location(ROUTE_OFFSET_LOW)?;
        let route_high = self.route_location(ROUTE_OFFSET_HIGH)?;
        self.clear_location(restore);
        self.with_profile_site(
            "abi",
            "abi.portal.stage.offset",
            "abi.portal.stage.offset",
            |emitter| {
                match site.offset {
                    PortalOffset::Byte(index) => {
                        let source = emitter.address_location(index, site.function)?;
                        emitter.copy_locations_with_zeroed_restore(source, route_low, restore);
                        emitter.clear_location(route_high);
                    }
                    PortalOffset::Word(offset) => {
                        let low = emitter.address_location(offset.low, site.function)?;
                        let high = emitter.address_location(offset.high, site.function)?;
                        emitter.copy_locations_with_zeroed_restore(low, route_low, restore);
                        emitter.copy_locations_with_zeroed_restore(high, route_high, restore);
                    }
                }

                Ok(())
            },
        )?;
        self.with_profile_site(
            "abi",
            "abi.portal.stage.payload",
            "abi.portal.stage.payload",
            |emitter| {
                if let PortalOperation::Store { source } = site.operation {
                    let value_source = if site.cells > 1 {
                        emitter.portal_temporary_location(site.function, site.leaf)?
                    } else {
                        emitter.value_operand_element_location(source, site.leaf, site.function)?
                    };
                    emitter.copy_locations_with_zeroed_restore(
                        value_source,
                        emitter.route_location(ROUTE_VALUE)?,
                        restore,
                    );
                } else {
                    emitter.clear_location(emitter.route_location(ROUTE_VALUE)?);
                }

                Ok(())
            },
        )?;
        self.with_profile_site(
            "abi",
            "abi.portal.stage.pc",
            "abi.portal.stage.pc",
            |emitter| {
                for (index, value) in [
                    (
                        ROUTE_ACCESSOR_LOW,
                        emitter.dispatch_encoding.encode(site.accessor) as u8,
                    ),
                    (
                        ROUTE_ACCESSOR_HIGH,
                        (emitter.dispatch_encoding.encode(site.accessor) >> 8) as u8,
                    ),
                    (
                        ROUTE_RESUME_LOW,
                        emitter.dispatch_encoding.encode(site.resume) as u8,
                    ),
                    (
                        ROUTE_RESUME_HIGH,
                        (emitter.dispatch_encoding.encode(site.resume) >> 8) as u8,
                    ),
                ] {
                    emitter.set_location(emitter.route_location(index)?, value);
                }

                Ok(())
            },
        )?;
        self.set_next_pc(router)
    }

    fn emit_global_portal_router(
        &mut self,
        router: GlobalPortalRouter,
    ) -> Result<(), AbiCodegenError> {
        let key = format!("abi.portal.router.global.{}", router.global.index());
        self.with_profile_site("abi", &key, "global portal router", |emitter| {
            emitter.emit_global_portal_router_inner(router)
        })
    }

    fn emit_global_portal_router_inner(
        &mut self,
        router: GlobalPortalRouter,
    ) -> Result<(), AbiCodegenError> {
        let region = AggregateRegion::Global(router.global);
        self.move_global_portal_request(region)?;
        self.set_location(
            self.portal_field_location(region, AbiField::Active, self.program.main())?,
            1,
        );
        self.enter_portal(region, self.program.main())
    }

    // Kept separate so the compact transport fixture exercises precisely the
    // production request path, with controlled bytes independent of PC layout.
    fn move_global_portal_request(
        &mut self,
        region: AggregateRegion,
    ) -> Result<(), AbiCodegenError> {
        // A static portal starts zero and its resume path clears every protocol
        // field after each access. Route staging is single-use, so moving the
        // seven request bytes is both smaller and cheaper than seven restored
        // cross-stack copies plus sixteen redundant remote clears.
        for (route, field) in [
            (ROUTE_OFFSET_LOW, AbiField::Index),
            (ROUTE_OFFSET_HIGH, AbiField::Scratch0),
            (ROUTE_VALUE, AbiField::Value),
            (ROUTE_ACCESSOR_LOW, AbiField::NextPcLow),
            (ROUTE_ACCESSOR_HIGH, AbiField::NextPcHigh),
            (ROUTE_RESUME_LOW, AbiField::ReturnPcLow),
            (ROUTE_RESUME_HIGH, AbiField::ReturnPcHigh),
        ] {
            self.with_profile_site(
                "abi",
                format!(
                    "abi.portal.route.field.{}",
                    [
                        "offset_low",
                        "offset_high",
                        "payload",
                        "accessor_low",
                        "accessor_high",
                        "resume_low",
                        "resume_high"
                    ][route]
                ),
                "request byte",
                |emitter| {
                    let source = emitter.route_location(route)?;
                    let destination =
                        emitter.portal_field_location(region, field, emitter.program.main())?;
                    // Each nibble transport duplicates the long navigation template.
                    // Restrict it to dynamic low bytes, where the bounded crossings
                    // repay that source-size cost; high bytes and payload values use
                    // the compact unary move.
                    let use_nibbles = emitter.nibble_transfer
                        && matches!(
                            route,
                            ROUTE_OFFSET_LOW | ROUTE_ACCESSOR_LOW | ROUTE_RESUME_LOW
                        );
                    if use_nibbles {
                        let Location::Relative(source) = source else {
                            unreachable!("route staging is frame-relative")
                        };
                        let Location::Global(destination) = destination else {
                            unreachable!("global portal fields are static")
                        };
                        emitter.move_relative_to_global_nibbles(source, destination)?;
                    } else {
                        emitter.with_profile_site_infallible(
                            "abi",
                            "abi.portal.route.transport.unary",
                            "unary transport",
                            |emitter| {
                                emitter.move_location_to_zero(source, destination);
                            },
                        );
                    }
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    fn emit_aggregate_accessor(&mut self, accessor: PortalAccessor) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.portal.accessor", "portal accessor", |emitter| {
            emitter.emit_aggregate_accessor_inner(accessor)
        })
    }

    fn emit_aggregate_accessor_inner(
        &mut self,
        accessor: PortalAccessor,
    ) -> Result<(), AbiCodegenError> {
        // A byte-sized logical offset can be dispatched directly without
        // moving the portal context. Larger offsets use the page portal
        // resolver, which moves only the request fields between page-local
        // protocol prefixes.
        self.emit_direct_or_page_accessor(accessor.kind)?;
        for field in [
            AbiField::Index,
            AbiField::Condition,
            AbiField::Restore,
            AbiField::Branch,
            AbiField::Scratch0,
            AbiField::Scratch1,
            AbiField::Scratch2,
            AbiField::Scratch3,
        ] {
            self.clear_abi_field(field)?;
        }
        Ok(())
    }

    fn emit_direct_or_page_accessor(
        &mut self,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        // Preserve the high byte while using Condition as a destructive
        // zero-test.  Branch is left set exactly when the high byte is zero.
        self.copy_abi_field(AbiField::Scratch0, AbiField::Condition)?;
        self.set_abi_field(AbiField::Branch, 1)?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;

        // Keep a copy of the direct-path decision.  The first guarded body
        // consumes Branch; the second one needs the inverse decision.
        self.copy_abi_field(AbiField::Branch, AbiField::Condition)?;

        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let direct = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.emit_direct_payload_countdown(kind)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(direct);
        self.move_to(0);

        // Condition is one for the direct path and zero for the page path.
        // Starting with Branch=1 and clearing it when Condition is nonzero
        // therefore selects the page resolver only for a nonzero high byte.
        self.set_abi_field(AbiField::Branch, 1)?;
        self.clear_branch_on_nonzero(AbiField::Condition)?;
        self.move_to(branch);
        let page = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.emit_page_portal_accessor(kind)?;
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(page);
        self.move_to(0);
        Ok(())
    }

    fn emit_direct_payload_countdown(
        &mut self,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            "abi.portal.payload.direct",
            "direct payload countdown",
            |emitter| {
                emitter.set_abi_field(AbiField::Branch, 1)?;
                emitter.emit_direct_payload_countdown_level(0, u8::MAX, kind)
            },
        )
    }

    fn emit_direct_payload_countdown_level(
        &mut self,
        level: u8,
        maximum: u8,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        let index = self.current_abi_offset(AbiField::Index)?;
        self.move_to(index);
        let body = self.capture(|emitter| {
            if level == maximum {
                emitter.clear_current();
                emitter.clear_abi_field(AbiField::Branch)?;
            } else {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_direct_payload_countdown_level(level + 1, maximum, kind)?;
            }
            emitter.move_to(index);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        self.emit_direct_payload_countdown_case(level, kind)
    }

    fn emit_direct_payload_countdown_case(
        &mut self,
        element: u8,
        kind: PortalAccessKind,
    ) -> Result<(), AbiCodegenError> {
        let branch = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(branch);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            let payload = emitter
                .config
                .logical_offset_from_head(PROTOCOL_CELLS + usize::from(element))
                as isize;
            let value = emitter.current_abi_offset(AbiField::Value)?;
            match kind {
                PortalAccessKind::Load => {
                    let scratch = emitter.current_abi_offset(AbiField::Scratch2)?;
                    emitter.copy(payload, value, scratch);
                }
                PortalAccessKind::Store => emitter.move_value(value, payload),
            }
            emitter.move_to(branch);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn emit_page_portal_accessor(&mut self, kind: PortalAccessKind) -> Result<(), AbiCodegenError> {
        self.with_profile_site(
            "abi",
            "abi.portal.page",
            "page portal resolver",
            |emitter| {
                let page_span = aggregate_page_portal_offset(1, emitter.config)? as isize;
                let sixteen_page_span = aggregate_page_portal_offset(16, emitter.config)? as isize;

                // Split the page byte into base-16 digits.  Moving the request
                // through sixteen pages at once avoids repeating the payload
                // transfer for every individual page.  The low and high
                // digits are kept in the current portal while the resolver
                // advances; Restore and Scratch3 hold their copies for the
                // reverse trip.
                emitter.split_abi_nibbles(
                    AbiField::Scratch0,
                    AbiField::Scratch1,
                    AbiField::Scratch2,
                )?;
                emitter.copy(
                    emitter.current_abi_offset(AbiField::Scratch1)?,
                    emitter.current_abi_offset(AbiField::Restore)?,
                    emitter.current_abi_offset(AbiField::Scratch0)?,
                );
                emitter.copy(
                    emitter.current_abi_offset(AbiField::Scratch2)?,
                    emitter.current_abi_offset(AbiField::Scratch3)?,
                    emitter.current_abi_offset(AbiField::Scratch0)?,
                );

                let high_fields: &[AbiField] = match kind {
                    PortalAccessKind::Load => &[
                        AbiField::Index,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch2,
                        AbiField::Scratch3,
                    ],
                    PortalAccessKind::Store => &[
                        AbiField::Index,
                        AbiField::Value,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch2,
                        AbiField::Scratch3,
                    ],
                };
                emitter.emit_page_portal_digit(
                    AbiField::Scratch2,
                    sixteen_page_span,
                    high_fields,
                )?;
                let low_fields: &[AbiField] = match kind {
                    PortalAccessKind::Load => &[
                        AbiField::Index,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch3,
                    ],
                    PortalAccessKind::Store => &[
                        AbiField::Index,
                        AbiField::Value,
                        AbiField::Restore,
                        AbiField::Scratch1,
                        AbiField::Scratch3,
                    ],
                };
                emitter.emit_page_portal_digit(AbiField::Scratch1, page_span, low_fields)?;

                // The low-byte direct resolver is emitted once. The moving
                // page portal makes its relative payload offsets identical
                // for every selected page.
                emitter.emit_direct_payload_countdown(kind)?;

                emitter.emit_page_portal_digit_reverse(
                    AbiField::Restore,
                    page_span,
                    kind,
                    &[AbiField::Scratch3],
                )?;
                emitter.emit_page_portal_digit_reverse(
                    AbiField::Scratch3,
                    sixteen_page_span,
                    kind,
                    &[],
                )?;

                Ok(())
            },
        )
    }

    fn emit_page_portal_digit(
        &mut self,
        counter: AbiField,
        delta: isize,
        carried: &[AbiField],
    ) -> Result<(), AbiCodegenError> {
        let counter_offset = self.current_abi_offset(counter)?;
        self.move_to(counter_offset);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            for field in carried {
                emitter.move_page_portal_field(*field, delta)?;
            }
            emitter.migrate_context(delta);
            emitter.move_to(counter_offset);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn emit_page_portal_digit_reverse(
        &mut self,
        counter: AbiField,
        delta: isize,
        kind: PortalAccessKind,
        carried: &[AbiField],
    ) -> Result<(), AbiCodegenError> {
        let counter_offset = self.current_abi_offset(counter)?;
        self.move_to(counter_offset);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            if kind == PortalAccessKind::Load {
                emitter.move_page_portal_field(AbiField::Value, -delta)?;
            }
            for field in carried {
                emitter.move_page_portal_field(*field, -delta)?;
            }
            emitter.move_page_portal_field(counter, -delta)?;
            emitter.migrate_context(-delta);
            emitter.move_to(counter_offset);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    fn move_page_portal_field(
        &mut self,
        field: AbiField,
        delta: isize,
    ) -> Result<(), AbiCodegenError> {
        let field = self.current_abi_offset(field)?;
        self.move_value(field, delta + field);
        Ok(())
    }

    #[cfg(test)]
    fn abi_field_offsets(&self, fields: [AbiField; 8]) -> Result<[isize; 8], AbiCodegenError> {
        let mut offsets = [0; 8];
        for (offset, field) in offsets.iter_mut().zip(fields) {
            *offset = self.current_abi_offset(field)?;
        }
        Ok(offsets)
    }

    #[cfg(test)]
    fn decompose_abi_byte(
        &mut self,
        source: AbiField,
        bits: &[isize; 8],
        temporary: isize,
    ) -> Result<(), AbiCodegenError> {
        self.clear(temporary);
        for &bit in bits {
            self.clear(bit);
        }
        let source = self.current_abi_offset(source)?;
        self.move_to(source);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.increment_bits(bits, temporary, 0, source);
        });
        self.emit_loop(body);
        self.move_to(0);
        Ok(())
    }

    /// Consume a byte with a bounded countdown. Each entered level updates
    /// the result directly; after the source reaches zero, all enclosing
    /// loops exit without touching it. No binary carry scratch cells are needed.
    fn split_abi_nibbles(
        &mut self,
        source: AbiField,
        low: AbiField,
        high: AbiField,
    ) -> Result<(), AbiCodegenError> {
        let source = self.current_abi_offset(source)?;
        let low = self.current_abi_offset(low)?;
        let high = self.current_abi_offset(high)?;
        self.clear(low);
        self.clear(high);
        self.split_nibble_level(source, low, high, 1);
        self.move_to(0);
        Ok(())
    }

    fn split_nibble_level(&mut self, source: isize, low: isize, high: isize, level: u8) {
        self.move_to(source);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(low);
            if level.is_multiple_of(16) {
                emitter.adjust(0_u8.wrapping_sub(15));
                emitter.move_to(high);
                emitter.adjust(1);
            } else {
                emitter.adjust(1);
            }
            if level < u8::MAX {
                emitter.split_nibble_level(source, low, high, level + 1);
            }
            emitter.move_to(source);
        });
        self.emit_loop(body);
    }

    fn emit_portal_resume(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        self.with_profile_site("abi", "abi.portal.resume", "portal resume", |emitter| {
            emitter.emit_portal_resume_inner(site)
        })
    }

    fn emit_portal_resume_inner(&mut self, site: PortalSite) -> Result<(), AbiCodegenError> {
        let is_load = matches!(site.operation, PortalOperation::Load { .. });
        self.with_profile_site(
            "abi",
            "abi.portal.resume.clear",
            "abi.portal.resume.clear",
            |emitter| {
                for field in AbiField::ALL {
                    if is_load && field == AbiField::Value {
                        continue;
                    }
                    emitter.clear_abi_field(field)?;
                }

                Ok(())
            },
        )?;
        let frame = self.layout(site.function)?.frame.clone();
        self.with_profile_site(
            "abi",
            "abi.portal.resume.transport",
            "abi.portal.resume.transport",
            |emitter| {
                match site.region {
                    AggregateRegion::Frame(aggregate) => {
                        let context_delta = -frame.aggregate_base_offset(aggregate)?;
                        if is_load {
                            let source = emitter.current_abi_offset(AbiField::Value)?;
                            let destination = context_delta + frame.abi_offset(AbiField::Value);
                            emitter.move_value(source, destination);
                        }
                        emitter.clear_abi_field(AbiField::Value)?;
                        emitter.migrate_context(context_delta);
                    }
                    AggregateRegion::Global(global) => {
                        let base = emitter.static_layout.aggregate_base_head(global)?;
                        if is_load {
                            emitter.move_global_portal_value_to_context(
                                base,
                                frame.abi_offset(AbiField::Value),
                            )?;
                        } else {
                            emitter.clear_abi_field(AbiField::Value)?;
                            emitter.emit_global_to_context(base, frame.context_chunks());
                        }
                    }
                    AggregateRegion::Outbox => {
                        unreachable!("outbox cannot use the aggregate portal")
                    }
                }

                Ok(())
            },
        )?;
        self.with_profile_site(
            "abi",
            "abi.portal.resume.deliver",
            "abi.portal.resume.deliver",
            |emitter| {
                if let PortalOperation::Load { destination } = site.operation {
                    let source = Location::Relative(frame.abi_offset(AbiField::Value));
                    let destination = if site.cells > 1 {
                        emitter.portal_temporary_location(site.function, site.leaf)?
                    } else {
                        emitter.value_operand_element_location(
                            destination,
                            site.leaf,
                            site.function,
                        )?
                    };
                    emitter.move_location(source, destination);
                }

                Ok(())
            },
        )?;
        if let Some(next_resume) = site.next_resume {
            self.with_profile_site(
                "abi",
                "abi.portal.resume.advance",
                "abi.portal.resume.advance",
                |emitter| {
                    emitter.increment_portal_offset(site.offset, site.function)?;

                    Ok(())
                },
            )?;
            let next = *self
                .portal
                .ordered_sites
                .iter()
                .find(|candidate| candidate.resume == next_resume)
                .expect("portal leaf resume chain must be complete");
            self.emit_portal_call(next)
        } else {
            if let PortalOperation::Load { destination } = site.operation
                && site.cells > 1
            {
                let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
                self.clear_location(restore);
                for leaf in 0..site.cells {
                    let source = self.portal_temporary_location(site.function, leaf)?;
                    let destination =
                        self.value_operand_element_location(destination, leaf, site.function)?;
                    self.copy_locations_with_zeroed_restore(source, destination, restore);
                }
            }
            if site.cells > 1 {
                for leaf in 0..site.cells {
                    let temporary = self.portal_temporary_location(site.function, leaf)?;
                    self.clear_location(temporary);
                }
            }
            self.set_next_pc(site.return_to)
        }
    }

    fn increment_portal_offset(
        &mut self,
        offset: PortalOffset,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let PortalOffset::Word(offset) = offset else {
            unreachable!("single-cell Version-0 access never increments its byte index")
        };
        let low = self.address_location(offset.low, function)?;
        let high = self.address_location(offset.high, function)?;
        let condition = Location::Relative(self.current_abi_offset(AbiField::Condition)?);
        let restore = Location::Relative(self.current_abi_offset(AbiField::Restore)?);
        let branch = Location::Relative(self.current_abi_offset(AbiField::Branch)?);
        self.copy_locations(low, condition, restore);
        self.move_context_to_location(condition);
        self.adjust(1);
        self.move_location_to_context(condition);
        self.set_location(branch, 1);
        self.move_context_to_location(condition);
        let nonzero = self.capture_infallible(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(condition);
            emitter.clear_location(branch);
            emitter.move_context_to_location(condition);
        });
        self.emit_loop(nonzero);
        self.move_location_to_context(condition);

        self.move_context_to_location(low);
        self.adjust(1);
        self.move_location_to_context(low);
        self.move_context_to_location(branch);
        let carry = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(branch);
            emitter.move_context_to_location(high);
            emitter.adjust(1);
            emitter.move_location_to_context(high);
            emitter.move_context_to_location(branch);
        });
        self.emit_loop(carry);
        self.move_location_to_context(branch);
        Ok(())
    }

    fn move_global_portal_value_to_context(
        &mut self,
        base: usize,
        destination: isize,
    ) -> Result<(), AbiCodegenError> {
        let context_chunks = self.config.portal_chunks();
        // The pointer currently uses the global portal base as its origin.
        self.emit_global_to_context(base, context_chunks);
        self.clear(destination);
        self.emit_context_to_global(base, context_chunks);

        let value = self.current_abi_offset(AbiField::Value)?;
        self.move_to(value);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            emitter.emit_global_to_context(base, context_chunks);
            emitter.move_to(destination);
            emitter.adjust(1);
            emitter.move_to(0);
            emitter.emit_context_to_global(base, context_chunks);
            emitter.move_to(value);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_to(0);
        self.emit_global_to_context(base, context_chunks);
        Ok(())
    }

    fn set_pc_locations(
        &mut self,
        array: AggregateRegion,
        function: FunctionId,
        low: AbiField,
        high: AbiField,
        value: ContinuationId,
    ) -> Result<(), AbiCodegenError> {
        let value = self.dispatch_encoding.encode(value);
        self.set_location(
            self.portal_field_location(array, low, function)?,
            value as u8,
        );
        self.set_location(
            self.portal_field_location(array, high, function)?,
            (value >> 8) as u8,
        );
        Ok(())
    }

    fn enter_portal(
        &mut self,
        array: AggregateRegion,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        match self.portal_base_location(array, function)? {
            Location::Relative(offset) => self.migrate_context(offset),
            Location::Global(position) => {
                let context_chunks = self.layout(function)?.frame.context_chunks();
                self.emit_context_to_global(position, context_chunks);
            }
        }
        Ok(())
    }

    fn portal_base_location(
        &self,
        array: AggregateRegion,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            AggregateRegion::Frame(aggregate) => Location::Relative(
                self.layout(function)?
                    .frame
                    .aggregate_base_offset(aggregate)?,
            ),
            AggregateRegion::Global(global) => {
                Location::Global(self.static_layout.aggregate_base_head(global)?)
            }
            AggregateRegion::Outbox => {
                unreachable!("validated portal aggregate cannot be outbox")
            }
        })
    }

    fn portal_field_location(
        &self,
        array: AggregateRegion,
        field: AbiField,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            AggregateRegion::Frame(aggregate) => Location::Relative(
                self.layout(function)?
                    .frame
                    .aggregate_portal_offset(aggregate, field)?,
            ),
            AggregateRegion::Global(global) => Location::Global(
                self.static_layout
                    .aggregate_portal_field_position(global, field)?,
            ),
            AggregateRegion::Outbox => {
                unreachable!("validated portal aggregate cannot be outbox")
            }
        })
    }

    fn array_element_location(
        &self,
        array: AggregateRegion,
        index: usize,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        Ok(match array {
            AggregateRegion::Frame(aggregate) => Location::Relative(
                self.layout(function)?
                    .frame
                    .aggregate_element_offset(aggregate, index)?,
            ),
            AggregateRegion::Global(global) => Location::Global(
                self.static_layout
                    .aggregate_element_position(global, index)?,
            ),
            AggregateRegion::Outbox => {
                Location::Relative(self.layout(function)?.frame.outbox_offset(index)?)
            }
        })
    }

    fn value_operand_element_location(
        &self,
        operand: ValueOperand,
        index: usize,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        match operand {
            ValueOperand::Cell(address) => {
                debug_assert_eq!(index, 0);
                self.address_location(address, function)
            }
            ValueOperand::Array(region) => self.array_element_location(region, index, function),
            ValueOperand::Aggregate {
                region,
                offset,
                cells: _,
            } => self.array_element_location(region, offset + index, function),
        }
    }

    fn portal_temporary_location(
        &self,
        function: FunctionId,
        index: usize,
    ) -> Result<Location, AbiCodegenError> {
        let layout = self.layout(function)?;
        debug_assert!(index < layout.portal_temporary_cells);
        Ok(Location::Relative(layout.frame.frame_offset(
            FrameSlot::new(layout.portal_temporary_start + index),
        )))
    }

    fn route_location(&self, index: usize) -> Result<Location, AbiCodegenError> {
        Ok(Location::Relative(
            self.layout(self.program.main())?
                .frame
                .route_offset(index)?,
        ))
    }

    fn common_outbox_offset(&self, index: usize) -> isize {
        let chunk = index / self.config.chunk_cells();
        let within = index % self.config.chunk_cells();
        let route_chunks = self
            .layout(self.program.main())
            .expect("main layout")
            .frame
            .route_chunks();
        -((route_chunks + chunk + 1) as isize * self.config.stride() as isize) + 1 + within as isize
    }

    fn acquire_branch_temporary(&mut self, function: FunctionId) -> Result<isize, AbiCodegenError> {
        let layout = self.layout(function)?;
        let start = layout.branch_temporary_start;
        let frame = layout.frame.clone();
        let slot = FrameSlot::new(start + self.branch_temporary_depth);
        self.branch_temporary_depth += 1;
        Ok(frame.frame_offset(slot))
    }

    fn address_location(
        &self,
        address: Address,
        function: FunctionId,
    ) -> Result<Location, AbiCodegenError> {
        let layout = self.layout(function)?;
        Ok(match address {
            Address::Frame(slot) => Location::Relative(layout.frame.frame_offset(slot)),
            Address::Global(global) => {
                Location::Global(self.static_layout.scalar_position(global)?)
            }
            Address::ArrayElement { array, index } => {
                self.array_element_location(array, index, function)?
            }
            Address::AbiValue => Location::Relative(layout.frame.abi_offset(AbiField::Value)),
        })
    }

    fn current_abi_offset(&self, field: AbiField) -> Result<isize, AbiCodegenError> {
        // Dispatch helpers use the common context geometry, so any layout is
        // sufficient. Using main also covers an empty function collection.
        Ok(self.layout(self.program.main())?.frame.abi_offset(field))
    }

    fn function(&self, id: FunctionId) -> Result<&FunctionDescriptor, AbiCodegenError> {
        self.program
            .function(id)
            .ok_or(AbiCodegenError::MissingFunctionLayout { function: id })
    }

    fn layout(&self, id: FunctionId) -> Result<&FunctionLayout, AbiCodegenError> {
        self.layouts
            .get(&id)
            .ok_or(AbiCodegenError::MissingFunctionLayout { function: id })
    }

    fn set_next_pc(&mut self, id: ContinuationId) -> Result<(), AbiCodegenError> {
        let id = self.dispatch_encoding.encode(id);
        self.set_abi_field(AbiField::NextPcLow, id as u8)?;
        self.set_abi_field(AbiField::NextPcHigh, (id >> 8) as u8)
    }

    fn set_pc_raw(&mut self, context_base: isize, id: ContinuationId) {
        let config = self.config;
        let id = self.dispatch_encoding.encode(id);
        self.set_raw(
            context_base + config.logical_offset_from_head(AbiField::PcLow.index()) as isize,
            id as u8,
        );
        self.set_raw(
            context_base + config.logical_offset_from_head(AbiField::PcHigh.index()) as isize,
            (id >> 8) as u8,
        );
    }

    fn set_pc_at(
        &mut self,
        context_base: isize,
        low: AbiField,
        high: AbiField,
        id: ContinuationId,
    ) {
        let id = self.dispatch_encoding.encode(id);
        let low = context_base + self.config.logical_offset_from_head(low.index()) as isize;
        let high = context_base + self.config.logical_offset_from_head(high.index()) as isize;
        self.set(low, id as u8);
        self.set(high, (id >> 8) as u8);
    }

    fn set_abi_field(&mut self, field: AbiField, value: u8) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.set(offset, value);
        Ok(())
    }

    fn add_abi_field(&mut self, field: AbiField, value: u8) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.move_to(offset);
        self.adjust(value);
        self.move_to(0);
        Ok(())
    }

    fn clear_abi_field(&mut self, field: AbiField) -> Result<(), AbiCodegenError> {
        let offset = self.current_abi_offset(field)?;
        self.clear(offset);
        Ok(())
    }

    fn copy_abi_field(&mut self, src: AbiField, dst: AbiField) -> Result<(), AbiCodegenError> {
        let src = self.current_abi_offset(src)?;
        let dst = self.current_abi_offset(dst)?;
        let restore = self.current_abi_offset(AbiField::Restore)?;
        self.copy(src, dst, restore);
        Ok(())
    }

    fn move_abi_field(&mut self, src: AbiField, dst: AbiField) {
        let src = self.config.logical_offset_from_head(src.index()) as isize;
        let dst = self.config.logical_offset_from_head(dst.index()) as isize;
        self.move_value(src, dst);
    }

    fn set_location(&mut self, location: Location, value: u8) {
        self.move_context_to_location(location);
        self.clear_current();
        self.adjust(value);
        self.move_location_to_context(location);
    }

    fn clear_location(&mut self, location: Location) {
        self.move_context_to_location(location);
        self.clear_current();
        self.move_location_to_context(location);
    }

    fn copy_locations(&mut self, src: Location, dst: Location, restore: Location) {
        if src == dst {
            return;
        }
        self.clear_location(restore);
        self.copy_locations_with_zeroed_restore(src, dst, restore);
    }

    fn copy_locations_with_zeroed_restore(
        &mut self,
        src: Location,
        dst: Location,
        restore: Location,
    ) {
        if src == dst {
            return;
        }
        self.clear_location(dst);
        self.copy_locations_to_zeroed_destination(src, dst, restore);
    }

    /// Copy a value when both the destination and restore cells are already
    /// zero. The copy restores `src` and leaves both scratch cells zero.
    /// Callers must establish the destination invariant, typically because a
    /// fresh frame or a freshly cleared aggregate slot is being populated.
    fn copy_locations_to_zeroed_destination(
        &mut self,
        src: Location,
        dst: Location,
        restore: Location,
    ) {
        if src == dst {
            return;
        }
        self.move_context_to_location(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            emitter.move_context_to_location(dst);
            emitter.adjust(1);
            emitter.move_location_to_context(dst);
            emitter.move_context_to_location(restore);
            emitter.adjust(1);
            emitter.move_location_to_context(restore);
            emitter.move_context_to_location(src);
        });
        self.emit_loop(body);
        self.move_location_to_context(src);
        self.move_location(restore, src);
    }

    /// Copy one static byte into the current frame without crossing the live
    /// stack once per source unit.  Nine shared static cells hold a temporary
    /// carry and eight binary digits.  The bits are folded into two nibbles,
    /// bounding stack crossings by the sum of those digits (at most 30)
    /// instead of expanding eight separate navigation templates.
    fn copy_global_to_relative_bits(&mut self, source: usize, destination: isize) {
        debug_assert!(self.config.chunk_cells() >= 9);
        let context_chunks = self.config.portal_chunks();
        let source = source as isize;
        let scratch =
            self.static_layout
                .remote_copy_scratch_position(0)
                .expect("D=16 static layout must reserve remote-copy scratch") as isize;
        let temporary = scratch - source;
        let bits = std::array::from_fn::<_, 8, _>(|index| scratch + 1 + index as isize - source);

        // Destination is frame-relative while the pointer still uses the
        // current context as its origin.
        self.clear(destination);
        self.emit_context_to_global(source as usize, context_chunks);

        // Scratch is shared by all copies, so establish and restore its zero
        // contract locally even if public Continuation IR supplied the copy.
        self.clear(temporary);
        for bit in bits {
            self.clear(bit);
        }

        // Consume the source into an eight-bit counter.  All movement here is
        // within static storage; recursive carry never scans the live stack.
        self.move_to(0);
        let decompose = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.increment_bits(&bits, temporary, 0, 0);
        });
        self.emit_loop(decompose);

        // Fold the binary digits into low/high nibble counters while staying
        // in static storage.  Reuse bit 0 and bit 4 as the digit cells.
        for index in 1..4 {
            self.move_static_value(bits[index], bits[0], 1_u8 << index);
        }
        for index in 5..8 {
            self.move_static_value(bits[index], bits[4], 1_u8 << (index - 4));
        }

        // Restore the source and build the destination one nibble unit at a
        // time.  Only these two loop bodies contain stack-navigation code.
        for (digit, contribution) in [(bits[0], 1_u8), (bits[4], 16_u8)] {
            self.move_to(digit);
            let transport = self.capture_infallible(|emitter| {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.adjust(contribution);
                emitter.emit_global_to_context(source as usize, context_chunks);
                emitter.move_to(destination);
                emitter.adjust(contribution);
                emitter.move_to(0);
                emitter.emit_context_to_global(source as usize, context_chunks);
                emitter.move_to(digit);
            });
            self.emit_loop(transport);
            self.move_to(0);
        }
        self.emit_global_to_context(source as usize, context_chunks);
    }

    /// Destructively move one frame-relative byte into a zero static cell.
    /// The otherwise unused lanes in the D=16 route chunk hold a carry and
    /// eight bits, bounding live-stack crossings by two nibble values (at
    /// most 30) instead of the source byte (at most 255).
    fn move_relative_to_global_nibbles(
        &mut self,
        source: isize,
        destination: usize,
    ) -> Result<(), AbiCodegenError> {
        debug_assert!(self.config.chunk_cells() >= GLOBAL_ROUTE_NIBBLE_CELLS);
        let Location::Relative(temporary) = self.route_location(ROUTE_SCRATCH_START)? else {
            unreachable!("route scratch is frame-relative")
        };
        let bits = std::array::from_fn::<_, 8, _>(|index| temporary + 1 + index as isize);

        self.with_profile_site_infallible(
            "abi",
            "abi.portal.route.decompose",
            "abi.portal.route.decompose",
            |emitter| {
                emitter.clear(temporary);
                for bit in bits {
                    emitter.clear(bit);
                }

                emitter.move_to(source);
                let decompose = emitter.capture_infallible(|emitter| {
                    emitter.adjust(255);
                    emitter.increment_bits(&bits, temporary, 0, source);
                });
                emitter.emit_loop(decompose);
                emitter.move_to(0);
            },
        );
        self.with_profile_site_infallible(
            "abi",
            "abi.portal.route.pack",
            "abi.portal.route.pack",
            |emitter| {
                for index in 1..4 {
                    emitter.move_static_value(bits[index], bits[0], 1_u8 << index);
                }
                for index in 5..8 {
                    emitter.move_static_value(bits[index], bits[4], 1_u8 << (index - 4));
                }
            },
        );
        let context_chunks = self.config.portal_chunks();
        for (digit, contribution) in [(bits[0], 1_u8), (bits[4], 16_u8)] {
            self.with_profile_site_infallible(
                "abi",
                format!("abi.portal.route.transport.nibble.{contribution}"),
                "nibble transport",
                |emitter| {
                    emitter.move_to(digit);
                    let transport = emitter.capture_infallible(|emitter| {
                        emitter.adjust(255);
                        emitter.move_to(0);
                        emitter.emit_context_to_global(destination, context_chunks);
                        emitter.adjust(contribution);
                        emitter.emit_global_to_context(destination, context_chunks);
                        emitter.move_to(digit);
                    });
                    emitter.emit_loop(transport);
                    emitter.move_to(0);
                },
            );
        }
        Ok(())
    }

    fn move_static_value(&mut self, source: isize, destination: isize, factor: u8) {
        self.move_to(source);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(destination);
            emitter.adjust(factor);
            emitter.move_to(source);
        });
        self.emit_loop(body);
        self.move_to(0);
    }

    /// Increment a little-endian Boolean bit vector. The temporary is zero on
    /// entry and exit; `return_to` is the pointer offset required by the
    /// surrounding BF loop.
    fn increment_bits(
        &mut self,
        bits: &[isize; 8],
        temporary: isize,
        index: usize,
        return_to: isize,
    ) {
        let bit = bits[index];
        self.move_to(bit);
        let was_set = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(temporary);
            emitter.adjust(1);
            emitter.move_to(bit);
        });
        self.emit_loop(was_set);
        self.adjust(1);

        self.move_to(temporary);
        let carry = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(bit);
            emitter.adjust(255);
            if index + 1 < bits.len() {
                emitter.increment_bits(bits, temporary, index + 1, temporary);
            } else {
                emitter.move_to(temporary);
            }
        });
        self.emit_loop(carry);
        self.move_to(return_to);
    }

    fn move_location(&mut self, src: Location, dst: Location) {
        if src == dst {
            return;
        }
        self.clear_location(dst);
        self.move_context_to_location(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            emitter.move_context_to_location(dst);
            emitter.adjust(1);
            emitter.move_location_to_context(dst);
            emitter.move_context_to_location(src);
        });
        self.emit_loop(body);
        self.move_location_to_context(src);
    }

    fn move_location_to_zero(&mut self, src: Location, dst: Location) {
        if src == dst {
            return;
        }
        self.move_context_to_location(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_location_to_context(src);
            emitter.move_context_to_location(dst);
            emitter.adjust(1);
            emitter.move_location_to_context(dst);
            emitter.move_context_to_location(src);
        });
        self.emit_loop(body);
        self.move_location_to_context(src);
    }

    fn move_context_to_location(&mut self, location: Location) {
        match location {
            Location::Relative(offset) => self.move_to(offset),
            Location::Global(position) => {
                debug_assert_eq!(self.position, 0);
                self.emit_context_to_global(position, self.config.portal_chunks());
            }
        }
    }

    fn move_location_to_context(&mut self, location: Location) {
        match location {
            Location::Relative(_) => self.move_to(0),
            Location::Global(position) => {
                debug_assert_eq!(self.position, 0);
                self.emit_global_to_context(position, self.config.portal_chunks());
            }
        }
    }

    /// Move from a function context base to one absolute static cell and
    /// rebase the compile-time origin at that cell. Stack flags are preserved.
    fn emit_context_to_global(&mut self, position: usize, context_chunks: usize) {
        self.with_profile_site_infallible(
            "abi",
            "abi.navigation.global",
            "context to global",
            |emitter| emitter.emit_context_to_global_inner(position, context_chunks),
        );
    }

    fn emit_context_to_global_inner(&mut self, position: usize, context_chunks: usize) {
        debug_assert_eq!(self.position, 0);
        let stride = self.config.stride() as isize;
        self.push_move(context_chunks as isize * stride);
        self.push_move(-stride);
        self.emit_loop(vec![AnnotatedBfInstruction::new(
            self.current_profile_site(),
            AnnotatedBfOperation::Move(-stride),
        )]);
        self.push_move(-((self.static_layout.anchor_head() - position) as isize));
        self.position = 0;
    }

    /// Move from one absolute static cell through the anchor to the current
    /// frame context and rebase the compile-time origin there.
    fn emit_global_to_context(&mut self, position: usize, context_chunks: usize) {
        self.with_profile_site_infallible(
            "abi",
            "abi.navigation.global",
            "global to context",
            |emitter| emitter.emit_global_to_context_inner(position, context_chunks),
        );
    }

    fn emit_global_to_context_inner(&mut self, position: usize, context_chunks: usize) {
        debug_assert_eq!(self.position, 0);
        let stride = self.config.stride() as isize;
        self.push_move((self.static_layout.anchor_head() - position) as isize);
        self.push_move(stride);
        self.emit_loop(vec![AnnotatedBfInstruction::new(
            self.current_profile_site(),
            AnnotatedBfOperation::Move(stride),
        )]);
        self.push_move(-(context_chunks as isize * stride));
        self.position = 0;
    }

    fn push_move(&mut self, distance: isize) {
        if distance != 0 {
            self.emit_operation(AnnotatedBfOperation::Move(distance));
        }
    }

    fn copy(&mut self, src: isize, dst: isize, restore: isize) {
        self.clear(dst);
        self.clear(restore);
        self.move_to(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(dst);
            emitter.adjust(1);
            emitter.move_to(restore);
            emitter.adjust(1);
            emitter.move_to(src);
        });
        self.emit_loop(body);
        self.move_value(restore, src);
        self.move_to(0);
    }

    fn move_value(&mut self, src: isize, dst: isize) {
        self.clear(dst);
        self.move_to(src);
        let body = self.capture_infallible(|emitter| {
            emitter.adjust(255);
            emitter.move_to(dst);
            emitter.adjust(1);
            emitter.move_to(src);
        });
        self.emit_loop(body);
        self.move_to(0);
    }

    fn set(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.clear_current();
        self.adjust(value);
        self.move_to(0);
    }

    fn set_raw(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.clear_current();
        self.adjust(value);
    }

    fn clear(&mut self, offset: isize) {
        self.move_to(offset);
        self.clear_current();
        self.move_to(0);
    }

    fn clear_current(&mut self) {
        let site = self.current_profile_site();
        self.emit_loop(vec![AnnotatedBfInstruction::new(
            site,
            AnnotatedBfOperation::Add(255),
        )]);
    }

    fn adjust(&mut self, value: u8) {
        self.emit_operation(AnnotatedBfOperation::Add(value));
    }

    fn move_to(&mut self, destination: isize) {
        self.emit_operation(AnnotatedBfOperation::Move(destination - self.position));
        self.position = destination;
    }

    /// Move to another frame's context base and make it the new offset origin.
    fn migrate_context(&mut self, delta: isize) {
        self.move_to(delta);
        self.position = 0;
    }

    fn capture<T>(
        &mut self,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<Vec<AnnotatedBfInstruction>, AbiCodegenError> {
        let outer = std::mem::take(&mut self.output);
        emit(self)?;
        Ok(std::mem::replace(&mut self.output, outer))
    }

    fn capture_infallible(&mut self, emit: impl FnOnce(&mut Self)) -> Vec<AnnotatedBfInstruction> {
        let outer = std::mem::take(&mut self.output);
        emit(self);
        std::mem::replace(&mut self.output, outer)
    }
}

#[cfg(test)]
mod tests {
    use bf_interpreter::{RunResult, run, run_with_stats};

    use super::*;
    use crate::continuation_adapter::adapt_flat_program;
    use crate::{CellId, Instruction, Program};

    fn execute_flat(instructions: Vec<Instruction>, cells: usize, config: AbiConfig) -> Vec<u8> {
        let flat = Program::new(cells, instructions).unwrap();
        let continuations = adapt_flat_program(&flat).unwrap();
        let bf = lower_continuations_with_config(&continuations, config)
            .unwrap()
            .to_source();
        run(bf.as_bytes(), b"").unwrap()
    }

    fn id(value: u16) -> ContinuationId {
        ContinuationId::new(value).unwrap()
    }

    fn execute_continuations(program: &ContinuationProgram, chunk_cells: usize) -> Vec<u8> {
        execute_continuations_with_stats(program, chunk_cells).output
    }

    fn execute_continuations_with_stats(
        program: &ContinuationProgram,
        chunk_cells: usize,
    ) -> RunResult {
        let source = lower_continuations_with_config(program, AbiConfig::new(chunk_cells).unwrap())
            .unwrap()
            .to_source();
        run_with_stats(source.as_bytes(), b"").unwrap()
    }

    fn recursive_countdown_program() -> ContinuationProgram {
        let main = FunctionId::new(0);
        let countdown = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let countdown_entry = id(3);
        let countdown_recurse = id(4);
        let countdown_base = id(5);
        let countdown_resume = id(6);

        let functions = vec![
            FunctionDescriptor::new(main, vec![], 2, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                countdown,
                vec![FrameSlot::new(0)],
                3,
                crate::ValueType::Cell,
                countdown_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: 4,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(1)),
                        value: b'L',
                    },
                ],
                Terminator::Call {
                    callee: countdown,
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![
                    FrameInstruction::Output {
                        src: Address::AbiValue,
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(1)),
                    },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                countdown_entry,
                countdown,
                vec![FrameInstruction::Transfer {
                    src: Address::Frame(FrameSlot::new(0)),
                    targets: vec![
                        FrameTransferTarget {
                            dst: Address::Frame(FrameSlot::new(1)),
                            factor: 1,
                        },
                        FrameTransferTarget {
                            dst: Address::Frame(FrameSlot::new(2)),
                            factor: 1,
                        },
                    ],
                }],
                Terminator::Branch {
                    condition: Address::Frame(FrameSlot::new(1)),
                    then_target: countdown_recurse,
                    else_target: countdown_base,
                },
            ),
            Continuation::new(
                countdown_recurse,
                countdown,
                vec![FrameInstruction::AddConst {
                    dst: Address::Frame(FrameSlot::new(2)),
                    value: 255,
                }],
                Terminator::Call {
                    callee: countdown,
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))],
                    return_to: countdown_resume,
                },
            ),
            Continuation::new(
                countdown_base,
                countdown,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))),
                },
            ),
            Continuation::new(
                countdown_resume,
                countdown,
                vec![FrameInstruction::AddConst {
                    dst: Address::AbiValue,
                    value: 1,
                }],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::AbiValue)),
                },
            ),
        ];
        ContinuationProgram::new(main, functions, continuations).unwrap()
    }

    #[test]
    fn main_only_adapter_runs_with_sixteen_cell_chunks() {
        let cell = CellId::new(0);
        let instructions = vec![
            Instruction::Set {
                dst: cell,
                value: b'A',
            },
            Instruction::Output { src: cell },
        ];
        {
            let chunk_cells = 16;
            assert_eq!(
                execute_flat(
                    instructions.clone(),
                    1,
                    AbiConfig::new(chunk_cells).unwrap()
                ),
                b"A"
            );
        }
    }

    #[test]
    fn dispatcher_matches_nonzero_high_byte_ids() {
        let main = FunctionId::new(0);
        let entry = ContinuationId::new(0x0101).unwrap();
        let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, entry);
        let continuation = Continuation::new(
            entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: b'H',
                },
                FrameInstruction::Output {
                    src: Address::Frame(FrameSlot::new(0)),
                },
            ],
            Terminator::Halt,
        );
        let program = ContinuationProgram::new(main, vec![function], vec![continuation]).unwrap();
        let source = compile_continuations(&program).unwrap();
        assert_eq!(run(source.as_bytes(), b"").unwrap(), b"H");
    }

    #[test]
    fn dispatcher_selects_low_zero_and_crosses_high_byte_pages() {
        let main = FunctionId::new(0);
        let first = id(1);
        let second = id(0x0100);
        let third = id(0x0201);
        let fourth = id(u16::MAX);
        let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
        let output = |continuation, byte, target| {
            Continuation::new(
                continuation,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: byte,
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                ],
                target,
            )
        };
        let program = ContinuationProgram::new(
            main,
            vec![function],
            vec![
                output(first, b'A', Terminator::Goto { target: second }),
                output(second, b'B', Terminator::Goto { target: third }),
                output(third, b'C', Terminator::Goto { target: fourth }),
                output(fourth, b'D', Terminator::Halt),
            ],
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"ABCD");
        }

        let artifact = compile_continuations_with_profile(&program, ProfileGranularity::Abi)
            .expect("profiled dispatcher should compile");
        assert_eq!(artifact.source, compile_continuations(&program).unwrap());
        artifact
            .map
            .validate_for_source(artifact.source.as_bytes())
            .unwrap();
        let page_keys = artifact
            .map
            .sites
            .iter()
            .filter(|site| {
                site.stable_key
                    .strip_prefix("abi.dispatch.page.")
                    .is_some_and(|suffix| suffix.parse::<u8>().is_ok())
            })
            .map(|site| site.stable_key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            page_keys,
            [
                "abi.dispatch.page.0",
                "abi.dispatch.page.1",
                "abi.dispatch.page.2",
                "abi.dispatch.page.255"
            ]
        );
        for key in ["abi.dispatch.page.select", "abi.dispatch.page.countdown"] {
            assert_eq!(
                artifact
                    .map
                    .sites
                    .iter()
                    .filter(|site| site.stable_key == key)
                    .count(),
                4,
                "each dispatch page should expose {key}",
            );
        }
    }

    #[test]
    fn balanced_dispatch_visits_every_dense_entry_across_pages() {
        let main = FunctionId::new(0);
        let cell = Address::Frame(FrameSlot::new(0));
        let order = (1..=512_u16)
            .flat_map(|i| [i, 1025 - i])
            .collect::<Vec<_>>();
        let continuations = order
            .iter()
            .enumerate()
            .map(|(index, &value)| {
                Continuation::new(
                    id(value),
                    main,
                    vec![
                        FrameInstruction::Set {
                            dst: cell,
                            value: (value % 251) as u8,
                        },
                        FrameInstruction::Output { src: cell },
                    ],
                    order
                        .get(index + 1)
                        .map_or(Terminator::Halt, |&next| Terminator::Goto {
                            target: id(next),
                        }),
                )
            })
            .collect();
        let program = ContinuationProgram::new(
            main,
            vec![FunctionDescriptor::new(
                main,
                vec![],
                1,
                ValueType::Void,
                id(1),
            )],
            continuations,
        )
        .unwrap();
        let encoding = DispatchEncoding::new(&program, &PortalPlan::new(&program).unwrap());
        assert_eq!(encoding.page_width, 33);
        let expected = order.iter().map(|v| (v % 251) as u8).collect::<Vec<_>>();
        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), expected);
        }
    }

    #[test]
    fn balanced_encoding_is_injective_at_width_and_id_limits() {
        let main = FunctionId::new(0);
        for count in [256, 257, 288, 289, 65025, 65535] {
            let program = ContinuationProgram::new(
                main,
                vec![FunctionDescriptor::new(
                    main,
                    vec![],
                    0,
                    ValueType::Void,
                    id(1),
                )],
                (1..=count)
                    .map(|value| Continuation::new(id(value), main, vec![], Terminator::Halt))
                    .collect(),
            )
            .unwrap();
            let encoding = DispatchEncoding::new(&program, &PortalPlan::new(&program).unwrap());
            let encoded = (1..=count)
                .map(|value| encoding.encode(id(value)))
                .collect::<HashSet<_>>();
            assert_eq!(encoded.len(), usize::from(count));
            assert!(!encoded.contains(&0));
        }
    }

    #[test]
    fn balanced_dispatch_preserves_recursive_call_and_return() {
        let original = recursive_countdown_program();
        // Width 33 makes every live entry xx00 on a different page. Calls
        // and returns must migrate contexts without selecting a second case.
        let remap = |value: ContinuationId| id(value.get() * 33);
        let functions = original
            .functions()
            .iter()
            .map(|f| {
                FunctionDescriptor::new(
                    f.id(),
                    f.parameters().to_vec(),
                    f.frame_slots(),
                    f.return_type(),
                    remap(f.entry()),
                )
            })
            .collect();
        let mut continuations = original
            .continuations()
            .iter()
            .map(|c| {
                let mut terminator = c.terminator().clone();
                match &mut terminator {
                    Terminator::Call { return_to, .. } => *return_to = remap(*return_to),
                    Terminator::Branch {
                        then_target,
                        else_target,
                        ..
                    } => {
                        *then_target = remap(*then_target);
                        *else_target = remap(*else_target);
                    }
                    Terminator::Return { .. } | Terminator::Halt => (),
                    _ => panic!("unexpected recursive fixture terminator"),
                }
                Continuation::new(remap(c.id()), c.function(), c.body().to_vec(), terminator)
            })
            .collect::<Vec<_>>();
        let live = continuations.iter().map(|c| c.id()).collect::<HashSet<_>>();
        continuations.extend(
            (1..=1024)
                .filter(|&value| !live.contains(&id(value)))
                .map(|value| {
                    Continuation::new(id(value), original.main(), vec![], Terminator::Halt)
                }),
        );
        let program = ContinuationProgram::new(original.main(), functions, continuations).unwrap();
        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"\x04L");
        }
    }

    #[test]
    fn dispatcher_does_not_run_low_zero_after_call_migrates_context() {
        let main = FunctionId::new(0);
        let helper = FunctionId::new(1);
        let main_entry = id(0x0101);
        let decoy = id(0x0100);
        let main_resume = id(2);
        let helper_entry = id(3);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(helper, vec![], 0, crate::ValueType::Void, helper_entry),
        ];
        // Keep the xx00 decoy after the entry to exercise the dispatcher's
        // ordering rather than relying on ContinuationProgram input order.
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![],
                Terminator::Call {
                    callee: helper,
                    arguments: vec![],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                decoy,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: b'X',
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                main_resume,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: b'A',
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                helper_entry,
                helper,
                vec![],
                Terminator::Return { value: None },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"A");
        }
    }

    #[test]
    fn dispatcher_uses_equality_scan_for_a_sparse_page() {
        let main = FunctionId::new(0);
        let first = id(0x0101);
        let last = id(0x01ff);
        let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
        let program = ContinuationProgram::new(
            main,
            vec![function],
            vec![
                Continuation::new(first, main, vec![], Terminator::Goto { target: last }),
                Continuation::new(
                    last,
                    main,
                    vec![
                        FrameInstruction::Set {
                            dst: Address::Frame(FrameSlot::new(0)),
                            value: b'S',
                        },
                        FrameInstruction::Output {
                            src: Address::Frame(FrameSlot::new(0)),
                        },
                    ],
                    Terminator::Halt,
                ),
            ],
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"S");
        }
        let artifact = compile_continuations_with_profile(&program, ProfileGranularity::Abi)
            .expect("sparse profiled dispatcher should compile");
        assert!(
            artifact
                .map
                .sites
                .iter()
                .any(|site| site.stable_key == "abi.dispatch.page.compare")
        );
        assert!(
            artifact
                .map
                .sites
                .iter()
                .all(|site| site.stable_key != "abi.dispatch.page.countdown")
        );
    }

    #[test]
    fn dispatcher_counts_down_contiguous_high_byte_pages() {
        let main = FunctionId::new(0);
        let first = id(0x0101);
        let second = id(0x0201);
        let third = id(0x0301);
        let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
        let output = |continuation, byte, target| {
            Continuation::new(
                continuation,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(FrameSlot::new(0)),
                        value: byte,
                    },
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                ],
                target,
            )
        };
        let program = ContinuationProgram::new(
            main,
            vec![function],
            vec![
                output(first, b'A', Terminator::Goto { target: second }),
                output(second, b'B', Terminator::Goto { target: third }),
                output(third, b'C', Terminator::Halt),
            ],
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"ABC");
        }
        let artifact = compile_continuations_with_profile(&program, ProfileGranularity::Abi)
            .expect("profiled high-byte countdown should compile");
        let keys = artifact
            .map
            .sites
            .iter()
            .map(|site| site.stable_key.as_str())
            .collect::<Vec<_>>();
        assert!(keys.contains(&"abi.dispatch.pages.countdown"));
        assert!(!keys.contains(&"abi.dispatch.pages.compare"));
        assert!(!keys.contains(&"abi.dispatch.page.select"));
    }

    #[test]
    fn dispatcher_uses_equality_scan_for_sparse_high_byte_pages() {
        let main = FunctionId::new(0);
        let first = id(0x0101);
        let last = id(0xff01);
        let function = FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, first);
        let program = ContinuationProgram::new(
            main,
            vec![function],
            vec![
                Continuation::new(first, main, vec![], Terminator::Goto { target: last }),
                Continuation::new(
                    last,
                    main,
                    vec![
                        FrameInstruction::Set {
                            dst: Address::Frame(FrameSlot::new(0)),
                            value: b'H',
                        },
                        FrameInstruction::Output {
                            src: Address::Frame(FrameSlot::new(0)),
                        },
                    ],
                    Terminator::Halt,
                ),
            ],
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"H");
        }
        let artifact = compile_continuations_with_profile(&program, ProfileGranularity::Abi)
            .expect("profiled sparse high-byte dispatcher should compile");
        let keys = artifact
            .map
            .sites
            .iter()
            .map(|site| site.stable_key.as_str())
            .collect::<Vec<_>>();
        assert!(keys.contains(&"abi.dispatch.pages.compare"));
        assert!(!keys.contains(&"abi.dispatch.pages.countdown"));
        assert_eq!(
            keys.iter()
                .filter(|key| **key == "abi.dispatch.page.select")
                .count(),
            2
        );
    }

    #[test]
    fn structured_branches_use_frame_temporaries() {
        let condition = CellId::new(0);
        let nested_condition = CellId::new(1);
        let value = CellId::new(2);
        let instructions = vec![
            Instruction::Set {
                dst: condition,
                value: 1,
            },
            Instruction::Branch {
                condition,
                then_body: vec![Instruction::Branch {
                    condition: nested_condition,
                    then_body: vec![],
                    else_body: vec![Instruction::Set {
                        dst: value,
                        value: b'B',
                    }],
                }],
                else_body: vec![],
            },
            Instruction::Output { src: value },
        ];
        assert_eq!(execute_flat(instructions, 3, AbiConfig::default()), b"B");
    }

    #[test]
    fn empty_else_branches_need_no_flag_and_still_consume_the_condition() {
        let condition = CellId::new(0);
        let value = CellId::new(1);
        let instructions = vec![
            Instruction::Set {
                dst: value,
                value: b'Z',
            },
            Instruction::Branch {
                condition,
                then_body: vec![Instruction::Set {
                    dst: value,
                    value: b'X',
                }],
                else_body: vec![],
            },
            Instruction::Output { src: value },
            Instruction::Set {
                dst: condition,
                value: 2,
            },
            Instruction::Branch {
                condition,
                then_body: vec![
                    Instruction::Set {
                        dst: condition,
                        value: 9,
                    },
                    Instruction::Set {
                        dst: value,
                        value: b'T',
                    },
                ],
                else_body: vec![],
            },
            Instruction::Output { src: value },
            Instruction::Output { src: condition },
        ];

        let empty_else = FrameInstruction::Branch {
            condition: Address::Frame(FrameSlot::new(0)),
            then_body: vec![],
            else_body: vec![],
        };
        assert_eq!(maximum_branch_depth(&[empty_else]), 0);

        {
            let chunk_cells = 16;
            assert_eq!(
                execute_flat(
                    instructions.clone(),
                    2,
                    AbiConfig::new(chunk_cells).unwrap()
                ),
                &[b'Z', b'T', 0]
            );
        }
    }

    #[test]
    fn direct_recursion_and_caller_locals_survive_with_sixteen_cell_chunks() {
        let program = recursive_countdown_program();
        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), &[4, b'L']);
        }
    }

    #[test]
    fn mutual_recursion_supports_different_frame_sizes() {
        let main = FunctionId::new(0);
        let small = FunctionId::new(1);
        let large = FunctionId::new(2);
        let main_entry = id(1);
        let main_resume = id(2);
        let small_entry = id(3);
        let small_call = id(4);
        let small_base = id(5);
        let small_resume = id(6);
        let large_entry = id(7);
        let large_call = id(8);
        let large_base = id(9);
        let large_resume = id(10);

        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                small,
                vec![FrameSlot::new(0)],
                3,
                crate::ValueType::Cell,
                small_entry,
            ),
            FunctionDescriptor::new(
                large,
                vec![FrameSlot::new(0)],
                12,
                crate::ValueType::Cell,
                large_entry,
            ),
        ];

        let split_and_branch = |function, entry, then_target, else_target, condition, argument| {
            Continuation::new(
                entry,
                function,
                vec![FrameInstruction::Transfer {
                    src: Address::Frame(FrameSlot::new(0)),
                    targets: vec![
                        FrameTransferTarget {
                            dst: Address::Frame(condition),
                            factor: 1,
                        },
                        FrameTransferTarget {
                            dst: Address::Frame(argument),
                            factor: 1,
                        },
                    ],
                }],
                Terminator::Branch {
                    condition: Address::Frame(condition),
                    then_target,
                    else_target,
                },
            )
        };
        let decrement_and_call = |function, continuation, argument, callee, return_to| {
            Continuation::new(
                continuation,
                function,
                vec![FrameInstruction::AddConst {
                    dst: Address::Frame(argument),
                    value: 255,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![ValueOperand::Cell(Address::Frame(argument))],
                    return_to,
                },
            )
        };
        let increment_and_return = |function, continuation| {
            Continuation::new(
                continuation,
                function,
                vec![FrameInstruction::AddConst {
                    dst: Address::AbiValue,
                    value: 1,
                }],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::AbiValue)),
                },
            )
        };

        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 5,
                }],
                Terminator::Call {
                    callee: small,
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            split_and_branch(
                small,
                small_entry,
                small_call,
                small_base,
                FrameSlot::new(1),
                FrameSlot::new(2),
            ),
            decrement_and_call(small, small_call, FrameSlot::new(2), large, small_resume),
            Continuation::new(
                small_base,
                small,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(2)))),
                },
            ),
            increment_and_return(small, small_resume),
            split_and_branch(
                large,
                large_entry,
                large_call,
                large_base,
                FrameSlot::new(10),
                FrameSlot::new(11),
            ),
            decrement_and_call(large, large_call, FrameSlot::new(11), small, large_resume),
            Continuation::new(
                large_base,
                large,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(FrameSlot::new(11)))),
                },
            ),
            increment_and_return(large, large_resume),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), &[5]);
        }
    }

    #[test]
    fn zero_slot_void_call_clears_the_callers_stale_abi_value() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 0, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(callee, vec![], 0, crate::ValueType::Void, callee_entry),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::AbiValue,
                    value: b'X',
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Return { value: None },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), &[0]);
        }
    }

    #[test]
    fn scalar_parameter_and_return_cross_the_d16_value_chunk_boundary() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let boundary_slot = FrameSlot::new(17);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                callee,
                vec![boundary_slot],
                18,
                crate::ValueType::Cell,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: b'Q',
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(boundary_slot))),
                },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        assert_eq!(execute_continuations(&program, 16), b"Q");
    }

    #[test]
    fn returned_frame_data_is_zero_when_the_same_frame_is_reused() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_after_first_call = id(2);
        let main_after_second_call = id(3);
        let callee_entry = id(4);
        let callee_dirty = id(5);
        let callee_return = id(6);
        let parameter = FrameSlot::new(0);
        let high_local = FrameSlot::new(16);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                callee,
                vec![parameter],
                17,
                crate::ValueType::Cell,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 1,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                    return_to: main_after_first_call,
                },
            ),
            Continuation::new(
                main_after_first_call,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 0,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![ValueOperand::Cell(Address::Frame(FrameSlot::new(0)))],
                    return_to: main_after_second_call,
                },
            ),
            Continuation::new(
                main_after_second_call,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Branch {
                    condition: Address::Frame(parameter),
                    then_target: callee_dirty,
                    else_target: callee_return,
                },
            ),
            Continuation::new(
                callee_dirty,
                callee,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(high_local),
                    value: b'X',
                }],
                Terminator::Goto {
                    target: callee_return,
                },
            ),
            Continuation::new(
                callee_return,
                callee,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(high_local))),
                },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), &[0]);
        }
    }

    #[test]
    fn one_caller_address_can_be_copied_to_multiple_parameters() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let lhs = FrameSlot::new(0);
        let rhs = FrameSlot::new(1);
        let sum = FrameSlot::new(2);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 1, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(
                callee,
                vec![lhs, rhs],
                3,
                crate::ValueType::Cell,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 21,
                }],
                Terminator::Call {
                    callee,
                    arguments: vec![
                        ValueOperand::Cell(Address::Frame(FrameSlot::new(0))),
                        ValueOperand::Cell(Address::Frame(FrameSlot::new(0))),
                    ],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![FrameInstruction::Output {
                    src: Address::AbiValue,
                }],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![
                    FrameInstruction::Transfer {
                        src: Address::Frame(lhs),
                        targets: vec![FrameTransferTarget {
                            dst: Address::Frame(sum),
                            factor: 1,
                        }],
                    },
                    FrameInstruction::Transfer {
                        src: Address::Frame(rhs),
                        targets: vec![FrameTransferTarget {
                            dst: Address::Frame(sum),
                            factor: 1,
                        }],
                    },
                ],
                Terminator::Return {
                    value: Some(ValueOperand::Cell(Address::Frame(sum))),
                },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), &[42]);
        }
    }

    #[test]
    fn return_dispatches_to_a_continuation_with_a_nonzero_high_byte() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let main_entry = id(1);
        let main_resume = id(0x0101);
        let callee_entry = id(2);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 0, crate::ValueType::Void, main_entry),
            FunctionDescriptor::new(callee, vec![], 0, crate::ValueType::Void, callee_entry),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![],
                Terminator::Call {
                    callee,
                    arguments: vec![],
                    return_to: main_resume,
                },
            ),
            Continuation::new(
                main_resume,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::AbiValue,
                        value: b'H',
                    },
                    FrameInstruction::Output {
                        src: Address::AbiValue,
                    },
                ],
                Terminator::Halt,
            ),
            Continuation::new(
                callee_entry,
                callee,
                vec![],
                Terminator::Return { value: None },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"H");
        }
    }

    #[test]
    fn global_addresses_work_in_transfer_loop_and_branch_for_sixteen_cell_chunks() {
        let main = FunctionId::new(0);
        let global = crate::GlobalId::new(0);
        let entry = id(1);
        let slot = FrameSlot::new(0);
        let function = FunctionDescriptor::new(main, vec![], 1, ValueType::Void, entry);
        let continuation = Continuation::new(
            entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Global(global),
                    value: 2,
                },
                FrameInstruction::Loop {
                    condition: Address::Global(global),
                    body: vec![
                        FrameInstruction::Output {
                            src: Address::Global(global),
                        },
                        FrameInstruction::AddConst {
                            dst: Address::Global(global),
                            value: 255,
                        },
                    ],
                },
                FrameInstruction::Set {
                    dst: Address::Frame(slot),
                    value: 3,
                },
                FrameInstruction::Transfer {
                    src: Address::Frame(slot),
                    targets: vec![FrameTransferTarget {
                        dst: Address::Global(global),
                        factor: 1,
                    }],
                },
                FrameInstruction::Transfer {
                    src: Address::Global(global),
                    targets: vec![FrameTransferTarget {
                        dst: Address::Frame(slot),
                        factor: 1,
                    }],
                },
                FrameInstruction::Output {
                    src: Address::Frame(slot),
                },
                FrameInstruction::Set {
                    dst: Address::Global(global),
                    value: 1,
                },
                FrameInstruction::Branch {
                    condition: Address::Global(global),
                    then_body: vec![FrameInstruction::Set {
                        dst: Address::Frame(slot),
                        value: b'B',
                    }],
                    else_body: vec![FrameInstruction::Set {
                        dst: Address::Frame(slot),
                        value: b'X',
                    }],
                },
                FrameInstruction::Output {
                    src: Address::Frame(slot),
                },
            ],
            Terminator::Halt,
        );
        let program = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::cell(global)],
            vec![function],
            vec![continuation],
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(
                execute_continuations(&program, chunk_cells),
                &[2, 1, 3, b'B']
            );
        }
    }

    #[test]
    fn global_to_frame_copy_preserves_every_value_in_sixteen_cell_chunks() {
        let main = FunctionId::new(0);
        let global = crate::GlobalId::new(0);
        let entry = id(1);
        let slot = FrameSlot::new(0);
        let function = FunctionDescriptor::new(main, vec![], 1, ValueType::Void, entry);
        let mut body = Vec::new();
        for value in 0..=u8::MAX {
            body.extend([
                FrameInstruction::Set {
                    dst: Address::Global(global),
                    value,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(slot),
                    value: 99,
                },
                FrameInstruction::Copy {
                    src: Address::Global(global),
                    dst: Address::Frame(slot),
                },
                FrameInstruction::Output {
                    src: Address::Frame(slot),
                },
                FrameInstruction::Output {
                    src: Address::Global(global),
                },
            ]);
        }
        let program = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::cell(global)],
            vec![function],
            vec![Continuation::new(entry, main, body, Terminator::Halt)],
        )
        .unwrap();

        let expected = (0..=u8::MAX)
            .flat_map(|value| [value, value])
            .collect::<Vec<_>>();
        {
            let chunk_cells = 16;
            assert_eq!(
                execute_continuations(&program, chunk_cells),
                expected,
                "D={chunk_cells}"
            );
        }
    }

    #[test]
    fn portal_ids_avoid_user_ids_and_report_exhaustion() {
        let source =
            "void main() { cell[2] values; cell i = 1; values[i] = 7; output(values[i]); }";
        let program = crate::lower_source(source).unwrap();
        let user_ids = program
            .continuations()
            .iter()
            .map(|continuation| continuation.id().get())
            .collect::<HashSet<_>>();
        let plan = PortalPlan::new(&program).unwrap();
        let mut hidden = Vec::new();
        hidden.extend(plan.accessors.iter().map(|accessor| accessor.id.get()));
        hidden.extend(plan.ordered_sites.iter().map(|site| site.resume.get()));
        assert!(hidden.iter().all(|id| !user_ids.contains(id)));
        assert_eq!(
            hidden.iter().copied().collect::<HashSet<_>>().len(),
            hidden.len()
        );

        let mut exhausted = (1..=u16::MAX).collect::<HashSet<_>>();
        assert_eq!(
            allocate_hidden_id(&mut exhausted),
            Err(AbiCodegenError::ContinuationIdsExhausted)
        );
    }

    #[test]
    fn version_one_dynamic_multi_cell_access_uses_16_bit_offsets() {
        let main = FunctionId::new(0);
        let root = crate::FrameAggregateId::new(0);
        let low = FrameSlot::new(0);
        let high = FrameSlot::new(1);
        let function = FunctionDescriptor::new_aggregates(
            main,
            vec![],
            2,
            vec![crate::FrameAggregateDescriptor::new(root, 300)],
            0,
            ValueType::Void,
            id(1),
        );
        let offset = LogicalOffset::new(Address::Frame(low), Address::Frame(high));
        let continuations = vec![
            Continuation::new(
                id(1),
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: AggregateRegion::Frame(root),
                            index: 254,
                        },
                        value: 10,
                    },
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: AggregateRegion::Frame(root),
                            index: 255,
                        },
                        value: 20,
                    },
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: AggregateRegion::Frame(root),
                            index: 256,
                        },
                        value: 30,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(low),
                        value: 254,
                    },
                ],
                Terminator::AggregateLoad {
                    source: AggregateRegion::Frame(root),
                    offset,
                    destination: ValueOperand::Aggregate {
                        region: AggregateRegion::Frame(root),
                        offset: 255,
                        cells: 3,
                    },
                    cells: 3,
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(low),
                        value: 0,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(high),
                        value: 1,
                    },
                ],
                Terminator::AggregateStore {
                    destination: AggregateRegion::Frame(root),
                    offset,
                    source: ValueOperand::Aggregate {
                        region: AggregateRegion::Frame(root),
                        offset: 255,
                        cells: 3,
                    },
                    cells: 3,
                    return_to: id(3),
                },
            ),
            Continuation::new(
                id(3),
                main,
                [254, 255, 256, 257, 258]
                    .into_iter()
                    .map(|index| FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: AggregateRegion::Frame(root),
                            index,
                        },
                    })
                    .collect(),
                Terminator::Halt,
            ),
        ];
        let program = ContinuationProgram::new(main, vec![function], continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(
                execute_continuations(&program, chunk_cells),
                &[10, 10, 10, 20, 30]
            );
        }
    }

    #[test]
    fn nibble_countdown_matches_binary_decomposition_for_every_byte() {
        let program = adapt_flat_program(&Program::new(1, vec![]).unwrap()).unwrap();
        let config = AbiConfig::new(16).unwrap();
        let layouts = build_layouts(&program, config).unwrap();
        let static_layout = StaticLayout::new(config, program.globals()).unwrap();
        let portal = PortalPlan::new(&program).unwrap();
        let make_source = |countdown| {
            let mut emitter = AbiEmitter::new(
                &program,
                &layouts,
                &static_layout,
                &portal,
                config,
                ProfileGranularity::Abi,
            );
            let bits = emitter
                .abi_field_offsets([
                    AbiField::Index,
                    AbiField::Condition,
                    AbiField::Restore,
                    AbiField::Scratch0,
                    AbiField::Scratch1,
                    AbiField::Scratch2,
                    AbiField::Scratch3,
                    AbiField::NextPcLow,
                ])
                .unwrap();
            // Dirty destinations check that the operation replaces prior values.
            emitter.set_abi_field(AbiField::Index, 173).unwrap();
            emitter.set_abi_field(AbiField::Scratch1, 219).unwrap();
            let source = emitter.current_abi_offset(AbiField::PcLow).unwrap();
            emitter.move_to(source);
            emitter.emit_operation(AnnotatedBfOperation::Input);
            if countdown {
                emitter
                    .split_abi_nibbles(AbiField::PcLow, AbiField::Index, AbiField::Scratch1)
                    .unwrap();
            } else {
                let temporary = emitter.current_abi_offset(AbiField::Branch).unwrap();
                emitter
                    .decompose_abi_byte(AbiField::PcLow, &bits, temporary)
                    .unwrap();
                for index in 1..4 {
                    emitter.move_static_value(bits[index], bits[0], 1 << index);
                }
                for index in 5..8 {
                    emitter.move_static_value(bits[index], bits[4], 1 << (index - 4));
                }
            }
            for offset in [bits[0], bits[4], source] {
                emitter.move_to(offset);
                emitter.emit_operation(AnnotatedBfOperation::Output);
            }
            optimize_annotated_bf(&AnnotatedBfProgram::new(emitter.output, emitter.sites))
                .to_source()
        };
        let old = make_source(false);
        let new = make_source(true);
        let mut before = 0;
        let mut after = 0;
        for value in 0..=u8::MAX {
            let reference = run_with_stats(old.as_bytes(), &[value]).unwrap();
            let actual = run_with_stats(new.as_bytes(), &[value]).unwrap();
            assert_eq!(actual.output, [value % 16, value / 16, 0], "{value}");
            assert_eq!(actual.output, reference.output);
            before += reference.stats.optimization.executed_native_operations;
            after += actual.stats.optimization.executed_native_operations;
        }
        eprintln!(
            "nibble split: native operations {before} -> {after}; BF bytes {} -> {}",
            old.len(),
            new.len()
        );
        assert!(after * 2 < before);
    }

    #[test]
    fn dynamic_portal_offset_preserves_high_quotient_bits() {
        let main = FunctionId::new(0);
        let root = crate::FrameAggregateId::new(0);
        let low = FrameSlot::new(0);
        let high = FrameSlot::new(1);
        let destination = FrameSlot::new(2);
        let function = FunctionDescriptor::new_aggregates(
            main,
            vec![],
            3,
            vec![crate::FrameAggregateDescriptor::new(root, 8192)],
            0,
            ValueType::Void,
            id(1),
        );
        let continuations = vec![
            Continuation::new(
                id(1),
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: AggregateRegion::Frame(root),
                            index: 8191,
                        },
                        value: b'Q',
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(low),
                        value: 255,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(high),
                        value: 31,
                    },
                ],
                Terminator::AggregateLoad {
                    source: AggregateRegion::Frame(root),
                    offset: LogicalOffset::new(Address::Frame(low), Address::Frame(high)),
                    destination: ValueOperand::Cell(Address::Frame(destination)),
                    cells: 1,
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                vec![FrameInstruction::Output {
                    src: Address::Frame(destination),
                }],
                Terminator::Halt,
            ),
        ];
        let program = ContinuationProgram::new(main, vec![function], continuations).unwrap();

        let result = execute_continuations_with_stats(&program, 16);
        assert_eq!(result.output, b"Q");
        assert!(result.stats.optimization.executed_native_operations < 40_000);
    }

    #[test]
    fn jumping_global_portal_restores_intermediate_payload_chunks() {
        let main = FunctionId::new(0);
        let global = crate::GlobalId::new(0);
        let low = FrameSlot::new(0);
        let high = FrameSlot::new(1);
        let value = FrameSlot::new(2);
        let destination = FrameSlot::new(3);
        let function = FunctionDescriptor::new(main, vec![], 4, ValueType::Void, id(1));
        let region = AggregateRegion::Global(global);
        let offset = LogicalOffset::new(Address::Frame(low), Address::Frame(high));
        let sentinels = [(0, b'A'), (15, b'B'), (16, b'C'), (255, b'D'), (4095, b'E')];
        let mut setup = sentinels
            .into_iter()
            .map(|(index, value)| FrameInstruction::Set {
                dst: Address::ArrayElement {
                    array: region,
                    index,
                },
                value,
            })
            .collect::<Vec<_>>();
        setup.extend([
            FrameInstruction::Set {
                dst: Address::Frame(low),
                value: 255,
            },
            FrameInstruction::Set {
                dst: Address::Frame(high),
                value: 31,
            },
            FrameInstruction::Set {
                dst: Address::Frame(value),
                value: b'Q',
            },
        ]);
        let continuations = vec![
            Continuation::new(
                id(1),
                main,
                setup,
                Terminator::AggregateStore {
                    destination: region,
                    offset,
                    source: ValueOperand::Cell(Address::Frame(value)),
                    cells: 1,
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(low),
                        value: 255,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(high),
                        value: 31,
                    },
                ],
                Terminator::AggregateLoad {
                    source: region,
                    offset,
                    destination: ValueOperand::Cell(Address::Frame(destination)),
                    cells: 1,
                    return_to: id(3),
                },
            ),
            Continuation::new(
                id(3),
                main,
                sentinels
                    .into_iter()
                    .map(|(index, _)| FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: region,
                            index,
                        },
                    })
                    .chain(std::iter::once(FrameInstruction::Output {
                        src: Address::Frame(destination),
                    }))
                    .collect(),
                Terminator::Halt,
            ),
        ];
        let program = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::aggregate(global, 8192)],
            vec![function],
            continuations,
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(
                execute_continuations(&program, chunk_cells),
                b"ABCDEQ",
                "D={chunk_cells}"
            );
        }
    }

    #[test]
    fn version_zero_global_portal_reaches_index_255() {
        let main = FunctionId::new(0);
        let index = FrameSlot::new(0);
        let value = FrameSlot::new(1);
        let function = FunctionDescriptor::new(main, vec![], 2, ValueType::Void, id(1));
        let continuations = vec![
            Continuation::new(
                id(1),
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Frame(index),
                        value: 255,
                    },
                    FrameInstruction::Set {
                        dst: Address::Frame(value),
                        value: b'Z',
                    },
                ],
                Terminator::ArrayStore {
                    array: AggregateRegion::Global(crate::GlobalId::new(0)),
                    index: Address::Frame(index),
                    value: Address::Frame(value),
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                vec![FrameInstruction::Output {
                    src: Address::ArrayElement {
                        array: AggregateRegion::Global(crate::GlobalId::new(0)),
                        index: 255,
                    },
                }],
                Terminator::Halt,
            ),
        ];
        let program = ContinuationProgram::new_with_globals(
            main,
            vec![crate::GlobalDescriptor::array(crate::GlobalId::new(0), 256)],
            vec![function],
            continuations,
        )
        .unwrap();
        let portal = PortalPlan::new(&program).unwrap();
        let encoding = DispatchEncoding::new(&program, &portal);
        assert_eq!(portal.ordered_sites[0].resume.get(), 5);
        assert_eq!(encoding.encode(id(1)), 5);
        assert_eq!(encoding.encode(portal.ordered_sites[0].resume), 1);
        {
            let chunk_cells = 16;
            let layouts = build_layouts(&program, AbiConfig::new(chunk_cells).unwrap()).unwrap();
            assert_eq!(layouts[&main].frame.route_chunks(), 1, "D={chunk_cells}");
            assert_eq!(execute_continuations(&program, chunk_cells), b"Z");
            let mut dense = program.continuations().to_vec();
            dense.extend(
                (3..=1024)
                    .map(|value| Continuation::new(id(value), main, vec![], Terminator::Halt)),
            );
            let dense = ContinuationProgram::new_with_globals(
                main,
                program.globals().to_vec(),
                program.functions().to_vec(),
                dense,
            )
            .unwrap();
            assert_eq!(execute_continuations(&dense, chunk_cells), b"Z");
        }
    }

    #[test]
    fn version_one_source_accessor_plan_stays_compact() {
        let program = crate::lower_source(
            "struct Triple { cell a; cell b; cell c; } Triple[100] global_values; void main() { Triple[100] local_values; cell index = 85; global_values[index].b = 'G'; local_values[index] = global_values[index]; output(local_values[index].a); output(local_values[index].b); output(local_values[index].c); }",
        )
        .unwrap();
        let plan = PortalPlan::new(&program).unwrap();
        assert_eq!(plan.accessors.len(), 2);
        assert!(plan.ordered_sites.len() < 16);
        {
            let chunk_cells = 16;
            let generated =
                lower_continuations_with_config(&program, AbiConfig::new(chunk_cells).unwrap())
                    .unwrap()
                    .to_source();
            assert!(
                generated.len() < 5_000_000,
                "D={chunk_cells}: {} bytes, {} portal leaves",
                generated.len(),
                plan.ordered_sites.len()
            );
        }
    }

    #[test]
    fn aggregate_subranges_cross_call_and_return_in_sixteen_cell_chunks() {
        let main = FunctionId::new(0);
        let helper = FunctionId::new(1);
        let source = crate::FrameAggregateId::new(0);
        let parameter = crate::FrameAggregateId::new(0);
        let functions = vec![
            FunctionDescriptor::new_aggregates(
                main,
                vec![],
                0,
                vec![crate::FrameAggregateDescriptor::new(source, 5)],
                3,
                ValueType::Void,
                id(1),
            ),
            FunctionDescriptor::new_aggregates(
                helper,
                vec![ParameterLocation::Aggregate(parameter)],
                0,
                vec![crate::FrameAggregateDescriptor::new(parameter, 3)],
                0,
                ValueType::Aggregate { cells: 3 },
                id(3),
            ),
        ];
        let continuations = vec![
            Continuation::new(
                id(1),
                main,
                (*b"ABC")
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: AggregateRegion::Frame(source),
                            index: index + 1,
                        },
                        value,
                    })
                    .collect(),
                Terminator::Call {
                    callee: helper,
                    arguments: vec![ValueOperand::Aggregate {
                        region: AggregateRegion::Frame(source),
                        offset: 1,
                        cells: 3,
                    }],
                    return_to: id(2),
                },
            ),
            Continuation::new(
                id(2),
                main,
                (0..3)
                    .map(|index| FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: AggregateRegion::Outbox,
                            index,
                        },
                    })
                    .collect(),
                Terminator::Abort,
            ),
            Continuation::new(
                id(3),
                helper,
                vec![],
                Terminator::Return {
                    value: Some(ValueOperand::aggregate(
                        AggregateRegion::Frame(parameter),
                        3,
                    )),
                },
            ),
        ];
        let program = ContinuationProgram::new(main, functions, continuations).unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"ABC");
        }
    }

    #[test]
    fn direct_global_scalar_and_array_arguments_normalize_to_the_caller() {
        let main = FunctionId::new(0);
        let callee = FunctionId::new(1);
        let scalar = crate::GlobalId::new(0);
        let array = crate::GlobalId::new(1);
        let parameter_array = crate::FrameArrayId::new(0);
        let main_entry = id(1);
        let main_resume = id(2);
        let callee_entry = id(3);
        let functions = vec![
            FunctionDescriptor::new(main, vec![], 0, ValueType::Void, main_entry),
            FunctionDescriptor::new_typed(
                callee,
                vec![
                    ParameterLocation::Cell(FrameSlot::new(0)),
                    ParameterLocation::Array(parameter_array),
                ],
                1,
                vec![crate::FrameArrayDescriptor::new(parameter_array, 2)],
                0,
                ValueType::Void,
                callee_entry,
            ),
        ];
        let continuations = vec![
            Continuation::new(
                main_entry,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::Global(scalar),
                        value: b'Q',
                    },
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: ArrayRegion::Global(array),
                            index: 0,
                        },
                        value: b'A',
                    },
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: ArrayRegion::Global(array),
                            index: 1,
                        },
                        value: b'B',
                    },
                ],
                Terminator::Call {
                    callee,
                    arguments: vec![
                        ValueOperand::Cell(Address::Global(scalar)),
                        ValueOperand::Array(ArrayRegion::Global(array)),
                    ],
                    return_to: main_resume,
                },
            ),
            Continuation::new(main_resume, main, vec![], Terminator::Halt),
            Continuation::new(
                callee_entry,
                callee,
                vec![
                    FrameInstruction::Output {
                        src: Address::Frame(FrameSlot::new(0)),
                    },
                    FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: ArrayRegion::Frame(parameter_array),
                            index: 0,
                        },
                    },
                    FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: ArrayRegion::Frame(parameter_array),
                            index: 1,
                        },
                    },
                ],
                Terminator::Return { value: None },
            ),
        ];
        let program = ContinuationProgram::new_with_globals(
            main,
            vec![
                crate::GlobalDescriptor::cell(scalar),
                crate::GlobalDescriptor::array(array, 2),
            ],
            functions,
            continuations,
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"QAB");
        }
    }

    #[test]
    fn aggregate_self_copy_is_a_no_op() {
        let main = FunctionId::new(0);
        let entry = id(1);
        let array = crate::FrameArrayId::new(0);
        let region = ArrayRegion::Frame(array);
        let program = ContinuationProgram::new(
            main,
            vec![FunctionDescriptor::new_typed(
                main,
                vec![],
                0,
                vec![crate::FrameArrayDescriptor::new(array, 2)],
                0,
                ValueType::Void,
                entry,
            )],
            vec![Continuation::new(
                entry,
                main,
                vec![
                    FrameInstruction::Set {
                        dst: Address::ArrayElement {
                            array: region,
                            index: 0,
                        },
                        value: b'A',
                    },
                    FrameInstruction::AggregateCopy {
                        src: region,
                        dst: region,
                        cells: 2,
                    },
                    FrameInstruction::Output {
                        src: Address::ArrayElement {
                            array: region,
                            index: 0,
                        },
                    },
                ],
                Terminator::Halt,
            )],
        )
        .unwrap();

        {
            let chunk_cells = 16;
            assert_eq!(execute_continuations(&program, chunk_cells), b"A");
        }
    }
}

#[cfg(test)]
mod portal_probe;

#[cfg(test)]
mod region_probe;
