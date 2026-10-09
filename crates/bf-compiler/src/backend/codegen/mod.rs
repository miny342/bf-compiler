//! Public ABI code-generation entry points and shared emitter state.
//!
//! Planning, dispatch, instructions, calls, portals, transport, and provenance
//! live in child modules. Each emitter keeps a single pointer/context state.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;

use crate::backend::frame_layout::{
    AbiConfig, AbiField, FrameLayoutError, PROTOCOL_CELLS, aggregate_element_physical_offset,
    aggregate_page_portal_offset,
};
use crate::backend::regions::{Region, RegionFlow, RegionNode, RegionPlan};
use crate::backend::static_layout::{StaticLayout, StaticLayoutError};
use crate::bf::optimizer::{optimize_annotated_bf, optimize_bf};
use crate::cir::ir::{
    Address, AggregateRegion, ArrayRegion, Continuation, ContinuationId, ContinuationProgram,
    FrameInstruction, FrameSlot, FrameTransferTarget, FunctionDescriptor, FunctionId, GlobalId,
    LogicalOffset, ParameterLocation, SourceSpan, Terminator, ValueOperand, ValueType,
};
use crate::{
    AnnotatedBfInstruction, AnnotatedBfOperation, AnnotatedBfProgram, BfProgram, ProfileSiteTable,
};

mod control;
mod direct_regions;
use direct_regions::DirectRegionState;
mod dispatch;
mod instructions;
mod lifetime;
mod portal;
mod portal_page;
mod portal_plan;
mod provenance;
mod region_emission;
mod static_frames;
mod transport;
#[cfg(test)]
use crate::backend::layout_plan::build_layouts_with_regions;
use crate::backend::layout_plan::{
    FunctionLayout, GLOBAL_ROUTE_NIBBLE_CELLS, build_layouts_for_codegen,
};
use dispatch::DispatchEncoding;
use portal_plan::*;
use static_frames::{StaticFramePlan, StaticResume};

#[cfg(test)]
mod tests;

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

/// BF backend choices independent of source/CIR lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiCodegenOptions {
    /// Fixed global contexts for closed nonrecursive call graphs that
    /// access globals. Functions reaching recursion keep dynamic activations.
    /// Unrelated activation paths share storage; callers and descendants never
    /// overlap. Enabled by default; capacity limits fall back to dynamic
    /// activations. Does not change CIR or inlining decisions.
    pub static_frames: bool,
    /// Execute bounded soft CFG regions during one dispatcher visit. Enabled
    /// by default; oversized regions retain ordinary dispatcher edges.
    pub region_emission: bool,
    /// Skip the standard tape capacity check during code generation.
    pub unlimited_tape: bool,
    /// Bound byte-wise frame/global crossings using two nibble counters.
    /// Off by default: RemoteTransfer can execute unary loops directly.
    /// This does not change offset decomposition or portal window jumps.
    pub nibble_transfer: bool,
    /// Compare distinct frame operands in their own cells. Reserves private
    /// zero/flag cells only for slots used by Compare or SubWithBorrow.
    /// Reuse those guards for preserving copies consumed by a following Branch,
    /// including a constant adjustment used to test equality.
    /// Off by default; changes physical frame sizes, not CIR/inlining.
    /// Aggregate elements (including imported flat CIR) retain scratch staging.
    pub inplace_compare: bool,
    /// Use 16 static anchors and 272-cell stack scans for global crossings.
    /// Off by default; also supported at fixed/dynamic boundaries.
    pub anchor_bank: bool,
}

impl Default for AbiCodegenOptions {
    fn default() -> Self {
        Self {
            static_frames: true,
            region_emission: true,
            unlimited_tape: false,
            nibble_transfer: false,
            inplace_compare: false,
            anchor_bank: false,
        }
    }
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
    lower_continuations_annotated_with_options(program, AbiConfig::default(), granularity, options)
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
        granularity,
        AbiCodegenOptions::default(),
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
        granularity,
        AbiCodegenOptions {
            unlimited_tape: true,
            ..Default::default()
        },
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
        ProfileGranularity::Abi,
        AbiCodegenOptions {
            unlimited_tape: !check_capacity,
            ..Default::default()
        },
    )?
    .into_plain())
}

fn lower_continuations_annotated_with_options(
    program: &ContinuationProgram,
    config: AbiConfig,
    granularity: ProfileGranularity,
    options: AbiCodegenOptions,
) -> Result<AnnotatedBfProgram, AbiCodegenError> {
    match lower_continuations_annotated_with_plans(program, config, granularity, options) {
        Err(
            AbiCodegenError::ContinuationIdsExhausted
            | AbiCodegenError::Layout(
                FrameLayoutError::FrameTooLarge { .. }
                | FrameLayoutError::MainFrameDoesNotFit { .. },
            )
            | AbiCodegenError::StaticLayout(StaticLayoutError::StaticAreaTooLarge { .. }),
        ) if options.static_frames => lower_continuations_annotated_with_plans(
            program,
            config,
            granularity,
            AbiCodegenOptions {
                static_frames: false,
                ..options
            },
        ),
        result => result,
    }
}

fn lower_continuations_annotated_with_plans(
    program: &ContinuationProgram,
    config: AbiConfig,
    granularity: ProfileGranularity,
    options: AbiCodegenOptions,
) -> Result<AnnotatedBfProgram, AbiCodegenError> {
    let check_capacity = !options.unlimited_tape;
    let mut regions = options.region_emission.then(|| RegionPlan::new(program));
    let static_layout = if options.anchor_bank {
        StaticLayout::new_with_anchor_bank(config, program.globals(), check_capacity)?
    } else if check_capacity {
        StaticLayout::new(config, program.globals())?
    } else {
        StaticLayout::new_unbounded(config, program.globals())?
    };
    let fixed_functions = if options.static_frames {
        crate::cir::analysis::call_graph::global_context_functions(program)
    } else {
        HashSet::new()
    };
    // Scalar-global-only programs have no portal route. A mixed S -> G call
    // still needs nine private scratch lanes for nibble argument transport.
    let boundary_nibbles = options.nibble_transfer
        && config.chunk_cells() >= GLOBAL_ROUTE_NIBBLE_CELLS
        && program.continuations().iter().any(|continuation| {
            !fixed_functions.contains(&continuation.function())
                && continuation
                    .terminator()
                    .callee()
                    .is_some_and(|callee| fixed_functions.contains(&callee))
        });
    let make_layouts = |regions: Option<&RegionPlan>| {
        let layouts = build_layouts_for_codegen(
            program,
            config,
            regions,
            options.inplace_compare,
            boundary_nibbles,
            options.static_frames,
        )?;
        if check_capacity {
            layouts[&program.main()]
                .frame
                .validate_main_capacity(static_layout.anchor_head())?;
        }
        Ok::<_, AbiCodegenError>(layouts)
    };
    let layouts = match make_layouts(regions.as_ref()) {
        Ok(layouts) => layouts,
        Err(_) if regions.is_some() => {
            // Region scratch must not make previously valid programs fail to
            // fit. Use the ordinary backend and retain its precise errors.
            regions = None;
            make_layouts(None)?
        }
        Err(error) => return Err(error),
    };
    let make_plans = |shared: bool| {
        let portal = if shared {
            PortalPlan::with_fixed_contexts(program, true, &fixed_functions)?
        } else {
            PortalPlan::with_frame_returns(program, false)?
        };
        // Static planning reserves tape and may allocate caller return gates
        // after portal IDs. Retry from the original layout on ID exhaustion.
        let mut storage = static_layout.clone();
        let fixed = options
            .static_frames
            .then(|| {
                StaticFramePlan::new(
                    program,
                    &layouts,
                    &portal,
                    &mut storage,
                    check_capacity,
                    &fixed_functions,
                )
            })
            .transpose()?;
        Ok::<_, AbiCodegenError>((portal, fixed, storage))
    };
    let (portal, fixed, static_layout) = make_plans(true).or_else(|error| {
        if matches!(error, AbiCodegenError::ContinuationIdsExhausted) {
            // Shared transport must not reject a program whose generic portal
            // and fixed call/return gates fit the hidden-ID budget together.
            make_plans(false)
        } else {
            Err(error)
        }
    })?;
    let mut emitter = AbiEmitter::new(
        program,
        &layouts,
        &static_layout,
        &portal,
        config,
        granularity,
    );
    emitter.nibble_transfer = options.nibble_transfer;
    emitter.inplace_compare = options.inplace_compare;
    emitter.regions = regions.as_ref();
    emitter.fixed = fixed.as_ref();
    emitter.dispatch_encoding =
        DispatchEncoding::with_fixed_frames(program, &portal, regions.as_ref(), fixed.as_ref());
    emitter.with_profile_attributes(
        "abi",
        "abi.initialization",
        "ABI initialization",
        fixed
            .as_ref()
            .map_or_else(BTreeMap::new, StaticFramePlan::profile_attributes),
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
    MissingFunctionLayout {
        function: FunctionId,
    },
    ContinuationIdsExhausted,
    /// Legacy diagnostic retained for API compatibility; this combination is supported.
    AnchorBankWithStaticFrames,
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
                write!(f, "ABI helpers exhaust the 16-bit continuation ID space")
            }
            Self::AnchorBankWithStaticFrames => {
                write!(
                    f,
                    "anchor bank cannot be combined with experimental static frames"
                )
            }
        }
    }
}

impl Error for AbiCodegenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Layout(error) => Some(error),
            Self::StaticLayout(error) => Some(error),
            Self::MissingFunctionLayout { .. }
            | Self::ContinuationIdsExhausted
            | Self::AnchorBankWithStaticFrames => None,
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

const ROUTE_OFFSET_LOW: usize = 0;
// Separate lanes distinguish the canonical anchor from the return phase.
const ANCHOR_BREADCRUMB: isize = 1;
const ANCHOR_GUIDE: isize = 2;
const ROUTE_OFFSET_HIGH: usize = 1;
const ROUTE_VALUE: usize = 2;
const ROUTE_ACCESSOR_LOW: usize = 3;
const ROUTE_ACCESSOR_HIGH: usize = 4;
const ROUTE_RESUME_LOW: usize = 5;
const ROUTE_RESUME_HIGH: usize = 6;
const ROUTE_SCRATCH_START: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Location {
    /// Offset from the current function's dispatch-context base.
    Relative(isize),
    /// Absolute position in the static prefix.
    Global(usize),
}

struct AbiEmitter<'a> {
    program: &'a ContinuationProgram,
    lifetime: lifetime::Plan<'a>,
    layouts: &'a HashMap<FunctionId, FunctionLayout>,
    static_layout: &'a StaticLayout,
    portal: &'a PortalPlan,
    regions: Option<&'a RegionPlan>,
    fixed: Option<&'a StaticFramePlan>,
    /// Known absolute origin of function-relative operations in this entry.
    /// Dispatcher and dynamic recursive entries retain the common relative ABI.
    fixed_context: Option<usize>,
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
    /// Active native soft loops; all close before a terminal changes context.
    region_loops: Vec<(ContinuationId, Location)>,
    direct_regions: DirectRegionState,
    nibble_transfer: bool,
    inplace_compare: bool,
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
            lifetime: lifetime::Plan::new(program),
            layouts,
            static_layout,
            portal,
            regions: None,
            fixed: None,
            fixed_context: None,
            dispatch_encoding,
            config,
            output: Vec::new(),
            sites,
            site_stack: vec![bf_profiling::ProfileSiteId(0)],
            instruction_path: Vec::new(),
            granularity,
            position: 0,
            branch_temporary_depth: 0,
            region_loops: Vec::new(),
            direct_regions: DirectRegionState::default(),
            nibble_transfer: false,
            inplace_compare: false,
            current_source: None,
        }
    }
}
