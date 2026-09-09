//! Interpreter for the Brainfuck dialect specified in the workspace's
//! `SPEC.md`.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub use bf_profiling::{ProfileMap, ProfileSiteId};

/// Number of cells in the standard tape.
pub const TAPE_LEN: usize = 30_000;

/// Execution measurements useful for comparing generated Brainfuck programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunStats {
    /// Number of parsed Brainfuck instructions executed, including jumps.
    pub executed_instructions: u64,
    /// Estimated instructions executed by a target that groups adjacent,
    /// identical `+`, `-`, `<`, and `>` instructions into one operation.
    pub executed_rle_instructions: u64,
    /// Largest tape index reached by the data pointer.
    pub max_pointer: usize,
    /// Work handled by the optimized interpreter rather than the raw BF VM.
    pub optimization: OptimizationRunStats,
}

/// Dynamic counters for the native operations selected by the interpreter.
///
/// These counters are diagnostic only. `RunStats::executed_instructions` and
/// `RunStats::executed_rle_instructions` retain the exact counts of the
/// unoptimized bytecode execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OptimizationRunStats {
    /// Fast-IR instructions dispatched by Rust.
    pub executed_native_operations: u64,
    /// Coalesced `+`, `-`, `<`, or `>` runs executed.
    pub rle_operations: u64,
    /// Native clear loops executed, including loops skipped for an initial 0.
    pub clear_loops: u64,
    /// Native scanning loops executed.
    pub scan_loops: u64,
    /// Pointer advances performed by native scanning loops.
    pub scan_steps: u64,
    /// Native linear-transfer loops executed.
    pub transfer_loops: u64,
    /// Source-cell units transferred by native linear-transfer loops.
    pub transfer_iterations: u64,
}

/// Binary output and measurements from one interpreter run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub output: Vec<u8>,
    pub stats: RunStats,
    /// Internal phase timings when requested through [`RunOptions`].
    pub timings: Option<Timings>,
    /// Site attribution collected when a profile map was supplied.
    pub profile: Option<ProfileResult>,
}

/// Options for the configurable interpreter entry point.
#[derive(Clone, Default)]
pub struct RunOptions {
    pub unbounded_tape: bool,
    pub collect_stats: bool,
    pub collect_timings: bool,
    pub profile: Option<ProfileOptions>,
    pub progress: Option<ProgressOptions>,
}

impl fmt::Debug for RunOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunOptions")
            .field("unbounded_tape", &self.unbounded_tape)
            .field("collect_stats", &self.collect_stats)
            .field("collect_timings", &self.collect_timings)
            .field("profile", &self.profile)
            .field("progress", &self.progress.as_ref().map(|_| "enabled"))
            .finish()
    }
}

/// Low-frequency execution snapshots without enabling full profiling counters.
#[derive(Clone)]
pub struct ProgressOptions {
    /// Emit periodic snapshots when set. Interrupt snapshots are independent.
    pub interval: Option<Duration>,
    /// Polled in batches by the VM; should remain very cheap.
    pub interrupted: fn() -> bool,
    /// Called from the execution thread, never from the signal handler.
    pub callback: Arc<dyn Fn(&ProgressSnapshot) + Send + Sync>,
}

/// One point-in-time view of a running VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub interrupted: bool,
    pub elapsed: Duration,
    pub current_site: Option<ProfileSiteId>,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub pointer: usize,
    pub stats: RunStats,
    pub hot_sites: Vec<ProgressSiteSnapshot>,
}

/// Per-site data retained in a progress snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSiteSnapshot {
    pub site: ProfileSiteId,
    pub counters: SiteCounters,
    pub samples: u64,
}

#[derive(Debug, Clone)]
pub struct ProfileOptions {
    pub map: ProfileMap,
    pub mode: ProfileMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileMode {
    Counters,
    Sample { interval: Duration },
    Exact,
}

/// Durations measured inside the interpreter library.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    pub parse: Duration,
    pub fast_ir_build: Duration,
    pub execute: Duration,
}

/// Dynamic counters attributed to one profile site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SiteCounters {
    pub fast_operations: u64,
    pub raw_bf_instructions: u64,
    pub rle_instructions: u64,
    pub loop_entries: u64,
    pub loop_iterations: u64,
    pub rle_operations: u64,
    pub clear_loops: u64,
    pub scan_loops: u64,
    pub scan_steps: u64,
    pub transfer_loops: u64,
    pub transfer_iterations: u64,
    pub input_operations: u64,
    pub output_operations: u64,
    pub pointer_distance: u64,
    pub maximum_pointer_observed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteProfile {
    pub site: ProfileSiteId,
    pub counters: SiteCounters,
    pub samples: u64,
    pub exclusive_time: Duration,
    pub profile_block_executions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileResult {
    pub sites: Vec<SiteProfile>,
    pub total_samples: u64,
    pub sampling_interval: Option<Duration>,
    pub clock_reads: u64,
    pub profile_block_executions: u64,
    pub measured_execute_time: Duration,
    pub mixed_provenance_native_operations: u64,
}

/// An error encountered while parsing or running a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidEncoding(String),
    UnmatchedOpeningBracket { offset: usize },
    UnmatchedClosingBracket { offset: usize },
    TapeUnderflow { instruction_offset: usize },
    TapeOverflow { instruction_offset: usize },
    ProfileArtifact(String),
    Interrupted,
    InvalidUtf8Output,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding(message) => write!(f, "invalid BF encoding: {message}"),
            Self::UnmatchedOpeningBracket { offset } => {
                write!(f, "unmatched '[' at byte offset {offset}")
            }
            Self::UnmatchedClosingBracket { offset } => {
                write!(f, "unmatched ']' at byte offset {offset}")
            }
            Self::TapeUnderflow { instruction_offset } => write!(
                f,
                "data pointer moved left of the tape at byte offset {instruction_offset}"
            ),
            Self::TapeOverflow { instruction_offset } => write!(
                f,
                "data pointer moved right of the tape at byte offset {instruction_offset}"
            ),
            Self::ProfileArtifact(message) => write!(f, "profile artifact error: {message}"),
            Self::Interrupted => write!(f, "interrupted"),
            Self::InvalidUtf8Output => write!(f, "program output is not valid UTF-8"),
        }
    }
}

impl StdError for Error {}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Right,
    Left,
    Increment,
    Decrement,
    Output,
    Input,
    JumpIfZero(usize),
    JumpIfNonZero(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RleOp {
    Right,
    Left,
    Increment,
    Decrement,
}

#[cfg(test)]
impl Op {
    const fn rle_op(self) -> Option<RleOp> {
        match self {
            Self::Right => Some(RleOp::Right),
            Self::Left => Some(RleOp::Left),
            Self::Increment => Some(RleOp::Increment),
            Self::Decrement => Some(RleOp::Decrement),
            Self::Output | Self::Input | Self::JumpIfZero(_) | Self::JumpIfNonZero(_) => None,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct Instruction {
    op: Op,
    source_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedProfileSite {
    id: ProfileSiteId,
    slot: usize,
}

impl ResolvedProfileSite {
    const ROOT: Self = Self {
        id: ProfileSiteId(0),
        slot: 0,
    };
}

/// Runs a Brainfuck program with byte-oriented input and output.
///
/// Non-instruction bytes in `source` are ignored. See the workspace's
/// `SPEC.md` for the exact dialect.
pub fn run(source: &[u8], input: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(run_with_stats(source, input)?.output)
}

/// Runs a program on a tape that grows to the right when necessary.
pub fn run_unbounded(source: &[u8], input: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(run_unbounded_with_stats(source, input)?.output)
}

/// Runs a Brainfuck program and returns its output and execution measurements.
pub fn run_with_stats(source: &[u8], input: &[u8]) -> Result<RunResult, Error> {
    run_with_options(
        source,
        input,
        RunOptions {
            collect_stats: true,
            ..RunOptions::default()
        },
    )
}

/// Runs a program on a tape that grows to the right when necessary.
///
/// This development mode is intended for the self-host compiler. Moving left
/// of cell zero remains an error, and instruction accounting is identical to
/// [`run_with_stats`].
pub fn run_unbounded_with_stats(source: &[u8], input: &[u8]) -> Result<RunResult, Error> {
    run_with_options(
        source,
        input,
        RunOptions {
            unbounded_tape: true,
            collect_stats: true,
            ..RunOptions::default()
        },
    )
}

/// Runs a program with optional timings and site attribution.
pub fn run_with_options(
    source: &[u8],
    input: &[u8],
    options: RunOptions,
) -> Result<RunResult, Error> {
    if matches!(options.progress.as_ref().and_then(|progress| progress.interval), Some(interval) if interval.is_zero())
    {
        return Err(Error::ProfileArtifact(
            "progress interval must be greater than zero".into(),
        ));
    }
    if let Some(profile) = &options.profile {
        if matches!(profile.mode, ProfileMode::Sample { interval } if interval.is_zero()) {
            return Err(Error::ProfileArtifact(
                "sampling interval must be greater than zero".into(),
            ));
        }
        profile
            .map
            .validate_for_source(source)
            .map_err(|error| Error::ProfileArtifact(error.to_string()))?;
    }

    let parse_started = options.collect_timings.then(Instant::now);
    // Generated self-host programs contain very long pointer runs. Building one
    // 40-byte `Instruction` per BF byte before immediately coalescing those runs
    // multiplies a large source into tens of gigabytes. Build the fast representation
    // directly in both profiled and unprofiled modes.
    let optimized = parse_optimized(source, options.profile.as_ref().map(|profile| &profile.map))?;
    let parse_elapsed = parse_started.map_or(Duration::ZERO, |started| started.elapsed());
    let build_elapsed = Duration::ZERO;
    let execute_started = (options.collect_timings || options.profile.is_some()).then(Instant::now);
    let mut result = execute(
        &optimized,
        input,
        options.unbounded_tape,
        options.profile.as_ref(),
        options.progress.as_ref(),
    )?;
    let execute_elapsed = execute_started.map_or(Duration::ZERO, |started| started.elapsed());
    if options.collect_timings {
        result.timings = Some(Timings {
            parse: parse_elapsed,
            fast_ir_build: build_elapsed,
            execute: execute_elapsed,
        });
    }
    if let Some(profile) = &mut result.profile {
        profile.measured_execute_time = execute_elapsed;
        if profile.total_samples != 0 {
            for site in &mut profile.sites {
                site.exclusive_time =
                    execute_elapsed.mul_f64(site.samples as f64 / profile.total_samples as f64);
            }
        }
    }
    Ok(result)
}

/// String convenience wrapper around [`run`].
///
/// This returns [`Error::InvalidUtf8Output`] if the program emits bytes that
/// are not valid UTF-8. Use [`run`] when arbitrary binary output is expected.
pub fn run_str(source: &str, input: &str) -> Result<String, Error> {
    let output = run(source.as_bytes(), input.as_bytes())?;
    String::from_utf8(output).map_err(|_| Error::InvalidUtf8Output)
}

#[cfg(test)]
fn parse(source: &[u8]) -> Result<Vec<Instruction>, Error> {
    let mut instructions: Vec<Instruction> = Vec::new();
    let mut openings = Vec::new();

    for (source_offset, byte) in source.iter().copied().enumerate() {
        let op = match byte {
            b'>' => Op::Right,
            b'<' => Op::Left,
            b'+' => Op::Increment,
            b'-' => Op::Decrement,
            b'.' => Op::Output,
            b',' => Op::Input,
            b'[' => {
                openings.push(instructions.len());
                Op::JumpIfZero(usize::MAX)
            }
            b']' => {
                let Some(opening_index) = openings.pop() else {
                    return Err(Error::UnmatchedClosingBracket {
                        offset: source_offset,
                    });
                };
                let closing_index = instructions.len();
                instructions[opening_index].op = Op::JumpIfZero(closing_index + 1);
                Op::JumpIfNonZero(opening_index + 1)
            }
            _ => continue,
        };
        instructions.push(Instruction { op, source_offset });
    }

    if let Some(opening_index) = openings.first().copied() {
        return Err(Error::UnmatchedOpeningBracket {
            offset: instructions[opening_index].source_offset,
        });
    }

    Ok(instructions)
}

#[derive(Debug, Clone)]
enum FastInstruction {
    Countdown {
        site: ResolvedProfileSite,
        mixed: bool,
        chain: Box<CountdownChain>,
    },
    Move {
        site: ResolvedProfileSite,
        mixed: bool,
        amount: isize,
        source_offsets: SourceOffsets,
    },
    Add {
        site: ResolvedProfileSite,
        mixed: bool,
        amount: u8,
        raw_count: u64,
    },
    Input {
        site: ResolvedProfileSite,
        mixed: bool,
    },
    Output {
        site: ResolvedProfileSite,
        mixed: bool,
    },
    Loop {
        site: ResolvedProfileSite,
        mixed: bool,
        body: Vec<FastInstruction>,
        optimization: Option<LoopOptimization>,
    },
}

impl FastInstruction {
    const fn site(&self) -> ResolvedProfileSite {
        match self {
            Self::Countdown { site, .. }
            | Self::Move { site, .. }
            | Self::Add { site, .. }
            | Self::Input { site, .. }
            | Self::Output { site, .. }
            | Self::Loop { site, .. } => *site,
        }
    }

    const fn mixed_provenance(&self) -> bool {
        match self {
            Self::Countdown { mixed, .. }
            | Self::Move { mixed, .. }
            | Self::Add { mixed, .. }
            | Self::Input { mixed, .. }
            | Self::Output { mixed, .. }
            | Self::Loop { mixed, .. } => *mixed,
        }
    }
}

/// Nested `[- CHILD tail]` loops, stored innermost first. Only the initial
/// decrements are folded: tails and subsequent loop conditions remain dynamic.
#[derive(Debug, Clone)]
struct CountdownChain {
    levels: Vec<CountdownLevel>,
    leaf: Box<FastInstruction>,
}

#[derive(Debug, Clone)]
struct CountdownLevel {
    tail: Vec<FastInstruction>,
    /// Sum of raw decrement counts from the innermost level through this level.
    decrement_prefix: u64,
}

fn fold_countdown(instruction: FastInstruction) -> FastInstruction {
    let FastInstruction::Loop {
        site,
        mixed,
        body,
        optimization: None,
    } = &instruction
    else {
        return instruction;
    };
    let [
        FastInstruction::Add {
            site: add_site,
            amount: 255,
            raw_count,
            mixed: false,
        },
        child,
        ..,
    ] = body.as_slice()
    else {
        return instruction;
    };
    if add_site.id != site.id
        || !matches!(
            child,
            FastInstruction::Loop { .. } | FastInstruction::Countdown { .. }
        )
    {
        return instruction;
    }
    let site = *site;
    let mixed = *mixed;
    let raw_count = *raw_count;
    let FastInstruction::Loop { body, .. } = instruction else {
        unreachable!()
    };
    let mut body = body.into_iter();
    body.next();
    let child = body.next().unwrap();
    let mut chain = match child {
        FastInstruction::Countdown {
            site: child_site,
            mixed: false,
            chain,
        } if child_site.id == site.id => chain,
        child => Box::new(CountdownChain {
            levels: Vec::new(),
            leaf: Box::new(child),
        }),
    };
    let decrement_prefix = chain
        .levels
        .last()
        .map_or(0, |level| level.decrement_prefix)
        + raw_count;
    chain.levels.push(CountdownLevel {
        tail: body.collect(),
        decrement_prefix,
    });
    FastInstruction::Countdown { site, mixed, chain }
}

#[derive(Debug, Clone)]
enum LoopOptimization {
    Clear {
        delta: u8,
        body_raw_count: u64,
        body_rle_count: u64,
    },
    Scan {
        amount: isize,
        source_offsets: SourceOffsets,
    },
    Transfer {
        source_delta: u8,
        updates: Box<[(isize, u8)]>,
        min_offset: isize,
        max_offset: isize,
        body_raw_count: u64,
        body_rle_count: u64,
        body_pointer_distance: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceOffsetRange {
    start: usize,
    len: usize,
    repeated: bool,
}

#[derive(Debug, Clone, Default)]
struct SourceOffsets {
    ranges: Vec<SourceOffsetRange>,
    len: usize,
}

impl SourceOffsets {
    fn push(&mut self, offset: usize) {
        if let Some(last) = self.ranges.last_mut()
            && !last.repeated
            && last.start.checked_add(last.len) == Some(offset)
        {
            last.len += 1;
        } else {
            self.ranges.push(SourceOffsetRange {
                start: offset,
                len: 1,
                repeated: false,
            });
        }
        self.len += 1;
    }

    fn push_repeated(&mut self, offset: usize, count: usize) {
        self.ranges.push(SourceOffsetRange {
            start: offset,
            len: count,
            repeated: true,
        });
        self.len += count;
    }

    const fn len(&self) -> usize {
        self.len
    }

    fn get(&self, mut index: usize) -> usize {
        assert!(index < self.len);
        for range in &self.ranges {
            if index < range.len {
                return range.start + if range.repeated { 0 } else { index };
            }
            index -= range.len;
        }
        unreachable!("validated source offset index has a containing range")
    }
}

struct FastBlock {
    instructions: Vec<FastInstruction>,
    last_rle: Option<RleOp>,
    opening_offset: Option<usize>,
    opening_site: Option<ResolvedProfileSite>,
}

impl FastBlock {
    fn root() -> Self {
        Self {
            instructions: Vec::new(),
            last_rle: None,
            opening_offset: None,
            opening_site: None,
        }
    }

    fn nested(opening_offset: usize, opening_site: ResolvedProfileSite) -> Self {
        Self {
            instructions: Vec::new(),
            last_rle: None,
            opening_offset: Some(opening_offset),
            opening_site: Some(opening_site),
        }
    }

    fn push_move(
        &mut self,
        op: RleOp,
        source_offset: usize,
        site: ResolvedProfileSite,
        profile_map: Option<&ProfileMap>,
        count: usize,
        compressed: bool,
    ) {
        let amount = if op == RleOp::Right {
            count as isize
        } else {
            -(count as isize)
        };
        if self.last_rle == Some(op)
            && let Some(FastInstruction::Move {
                site: current_site,
                mixed,
                amount: current,
                source_offsets,
            }) = self.instructions.last_mut()
        {
            *mixed |= current_site.id != site.id;
            *current_site = lowest_common_ancestor(profile_map, *current_site, site);
            *current += amount;
            if compressed {
                source_offsets.push_repeated(source_offset, count);
            } else {
                source_offsets.push(source_offset);
            }
        } else {
            let mut source_offsets = SourceOffsets::default();
            if compressed {
                source_offsets.push_repeated(source_offset, count);
            } else {
                source_offsets.push(source_offset);
            }
            self.instructions.push(FastInstruction::Move {
                site,
                mixed: false,
                amount,
                source_offsets,
            });
        }
        self.last_rle = Some(op);
    }

    fn push_add(
        &mut self,
        op: RleOp,
        site: ResolvedProfileSite,
        profile_map: Option<&ProfileMap>,
        count: usize,
    ) {
        let amount = if op == RleOp::Increment {
            count as u8
        } else {
            0u8.wrapping_sub(count as u8)
        };
        if self.last_rle == Some(op)
            && let Some(FastInstruction::Add {
                site: current_site,
                mixed,
                amount: current,
                raw_count,
            }) = self.instructions.last_mut()
        {
            *mixed |= current_site.id != site.id;
            *current_site = lowest_common_ancestor(profile_map, *current_site, site);
            *current = current.wrapping_add(amount);
            *raw_count += count as u64;
        } else {
            self.instructions.push(FastInstruction::Add {
                site,
                mixed: false,
                amount,
                raw_count: count as u64,
            });
        }
        self.last_rle = Some(op);
    }

    fn push_non_rle(&mut self, instruction: FastInstruction) {
        self.instructions.push(instruction);
        self.last_rle = None;
    }
}

fn parse_optimized(
    source: &[u8],
    profile_map: Option<&ProfileMap>,
) -> Result<Vec<FastInstruction>, Error> {
    let mut blocks = vec![FastBlock::root()];
    let mut ordinal = 0_u64;
    let mut range_index = 0_usize;
    let range_slots = profile_map.map(profile_range_slots);
    let compressed = bf_profiling::rle::compressed(source);
    for run in bf_profiling::rle::Runs::new(source) {
        let run = run.map_err(|e| Error::InvalidEncoding(e.to_string()))?;
        let source_offset = run.offset;
        let byte = run.byte;
        let mut remaining = run.count;
        while remaining > 0 {
            let site = resolved_profile_site(
                profile_map,
                range_slots.as_deref(),
                &mut range_index,
                ordinal,
            );
            let count = if let Some(map) = profile_map {
                remaining.min((map.ranges[range_index].end - ordinal) as usize)
            } else {
                remaining
            };
            ordinal += count as u64;
            remaining -= count;
            let current = blocks.last_mut().unwrap();
            match byte {
                b'>' => current.push_move(
                    RleOp::Right,
                    source_offset,
                    site,
                    profile_map,
                    count,
                    compressed,
                ),
                b'<' => current.push_move(
                    RleOp::Left,
                    source_offset,
                    site,
                    profile_map,
                    count,
                    compressed,
                ),
                b'+' => current.push_add(RleOp::Increment, site, profile_map, count),
                b'-' => current.push_add(RleOp::Decrement, site, profile_map, count),
                b'.' => current.push_non_rle(FastInstruction::Output { site, mixed: false }),
                b',' => current.push_non_rle(FastInstruction::Input { site, mixed: false }),
                b'[' => {
                    current.last_rle = None;
                    blocks.push(FastBlock::nested(source_offset, site));
                }
                b']' => {
                    if blocks.len() == 1 {
                        return Err(Error::UnmatchedClosingBracket {
                            offset: source_offset,
                        });
                    }
                    let block = blocks.pop().unwrap();
                    let body = block.instructions;
                    let optimization = recognize_fast_loop(&body);
                    let mut loop_site = block.opening_site.unwrap();
                    let mut mixed = false;
                    if optimization.is_some() {
                        merge_provenance(&mut loop_site, &mut mixed, site, false, profile_map);
                        for instruction in &body {
                            merge_provenance(
                                &mut loop_site,
                                &mut mixed,
                                instruction.site(),
                                instruction.mixed_provenance(),
                                profile_map,
                            );
                        }
                    }
                    blocks.last_mut().unwrap().push_non_rle(fold_countdown(
                        FastInstruction::Loop {
                            site: loop_site,
                            mixed,
                            body,
                            optimization,
                        },
                    ));
                }
                _ => {}
            }
        }
    }

    if blocks.len() != 1 {
        return Err(Error::UnmatchedOpeningBracket {
            offset: blocks[1].opening_offset.unwrap(),
        });
    }
    Ok(blocks.pop().unwrap().instructions)
}

fn profile_range_slots(map: &ProfileMap) -> Vec<usize> {
    map.ranges
        .iter()
        .map(|range| {
            map.sites
                .iter()
                .position(|site| site.id == range.site)
                .expect("validated profile range references a known site")
        })
        .collect()
}

fn resolved_profile_site(
    profile_map: Option<&ProfileMap>,
    range_slots: Option<&[usize]>,
    range_index: &mut usize,
    ordinal: u64,
) -> ResolvedProfileSite {
    let Some(map) = profile_map else {
        return ResolvedProfileSite::ROOT;
    };
    while map.ranges[*range_index].end <= ordinal {
        *range_index += 1;
    }
    ResolvedProfileSite {
        id: map.ranges[*range_index].site,
        slot: range_slots.unwrap()[*range_index],
    }
}

fn merge_provenance(
    target: &mut ResolvedProfileSite,
    mixed: &mut bool,
    site: ResolvedProfileSite,
    site_is_mixed: bool,
    profile_map: Option<&ProfileMap>,
) {
    *mixed |= site_is_mixed || target.id != site.id;
    *target = lowest_common_ancestor(profile_map, *target, site);
}

fn lowest_common_ancestor(
    profile_map: Option<&ProfileMap>,
    left: ResolvedProfileSite,
    right: ResolvedProfileSite,
) -> ResolvedProfileSite {
    if left.id == right.id {
        return left;
    }
    let Some(profile_map) = profile_map else {
        return ResolvedProfileSite::ROOT;
    };
    let parent = |id: ProfileSiteId| {
        profile_map
            .sites
            .iter()
            .find(|site| site.id == id)
            .and_then(|site| site.parent)
    };
    let mut ancestors = std::collections::HashSet::new();
    let mut current = Some(left.id);
    while let Some(site) = current {
        ancestors.insert(site);
        current = parent(site);
    }
    let mut current = Some(right.id);
    while let Some(site) = current {
        if ancestors.contains(&site) {
            let slot = profile_map
                .sites
                .iter()
                .position(|metadata| metadata.id == site)
                .expect("validated parent references a known site");
            return ResolvedProfileSite { id: site, slot };
        }
        current = parent(site);
    }
    ResolvedProfileSite::ROOT
}

fn recognize_fast_loop(body: &[FastInstruction]) -> Option<LoopOptimization> {
    if body.is_empty() {
        return None;
    }

    if let [
        FastInstruction::Add {
            amount, raw_count, ..
        },
    ] = body
        && amount % 2 == 1
    {
        return Some(LoopOptimization::Clear {
            delta: *amount,
            body_raw_count: *raw_count,
            body_rle_count: 1,
        });
    }

    if let [
        FastInstruction::Move {
            amount,
            source_offsets,
            ..
        },
    ] = body
    {
        return Some(LoopOptimization::Scan {
            amount: *amount,
            source_offsets: source_offsets.clone(),
        });
    }

    let mut pointer = 0_isize;
    let mut min_offset = 0_isize;
    let mut max_offset = 0_isize;
    let mut updates = std::collections::BTreeMap::<isize, u8>::new();
    let mut body_raw_count = 0_u64;
    let mut body_pointer_distance = 0_u64;

    for instruction in body {
        match instruction {
            FastInstruction::Move {
                amount,
                source_offsets,
                ..
            } => {
                let raw_count = source_offsets.len() as u64;
                body_raw_count += raw_count;
                body_pointer_distance += raw_count;
                pointer = pointer.checked_add(*amount)?;
                min_offset = min_offset.min(pointer);
                max_offset = max_offset.max(pointer);
            }
            FastInstruction::Add {
                amount, raw_count, ..
            } => {
                body_raw_count += *raw_count;
                let value = updates.entry(pointer).or_default();
                *value = value.wrapping_add(*amount);
            }
            FastInstruction::Countdown { .. }
            | FastInstruction::Input { .. }
            | FastInstruction::Output { .. }
            | FastInstruction::Loop { .. } => return None,
        }
    }

    let source_delta = updates.remove(&0).unwrap_or(0);
    if pointer != 0 || !matches!(source_delta, 1 | 255) {
        return None;
    }
    updates.retain(|_, factor| *factor != 0);
    Some(LoopOptimization::Transfer {
        source_delta,
        updates: updates.into_iter().collect::<Vec<_>>().into_boxed_slice(),
        min_offset,
        max_offset,
        body_raw_count,
        body_rle_count: body.len() as u64,
        body_pointer_distance,
    })
}

const INACTIVE_PROFILE_SITE: u32 = u32::MAX;

struct ProfileCollector {
    sites: Vec<SiteProfile>,
    mode: ProfileMode,
    exact: bool,
}

impl ProfileCollector {
    fn new(options: &ProfileOptions) -> Self {
        let sites = options
            .map
            .sites
            .iter()
            .map(|site| SiteProfile {
                site: site.id,
                counters: SiteCounters::default(),
                samples: 0,
                exclusive_time: Duration::ZERO,
                profile_block_executions: 0,
            })
            .collect();
        Self {
            sites,
            mode: options.mode,
            exact: options.mode == ProfileMode::Exact,
        }
    }

    fn site_mut(&mut self, site: ResolvedProfileSite) -> &mut SiteProfile {
        &mut self.sites[site.slot]
    }
}

struct SamplingSession {
    current_site: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    samples: Arc<Vec<AtomicU64>>,
    interval: Duration,
    handle: Option<thread::JoinHandle<()>>,
}

impl SamplingSession {
    fn start(options: Option<&ProfileOptions>, site_slots: usize) -> Option<Self> {
        let ProfileMode::Sample { interval } = options?.mode else {
            return None;
        };
        let current_site = Arc::new(AtomicU32::new(INACTIVE_PROFILE_SITE));
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(
            (0..site_slots)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>(),
        );
        let thread_site = Arc::clone(&current_site);
        let thread_stop = Arc::clone(&stop);
        let thread_samples = Arc::clone(&samples);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                thread::park_timeout(interval);
                let site = thread_site.load(Ordering::Relaxed);
                if let Some(counter) = thread_samples.get(site as usize) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        Some(Self {
            current_site,
            stop,
            samples,
            interval,
            handle: Some(handle),
        })
    }

    fn finish(mut self) -> (Vec<u64>, Duration) {
        self.current_site
            .store(INACTIVE_PROFILE_SITE, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
        let samples = self
            .samples
            .iter()
            .map(|sample| sample.load(Ordering::Relaxed))
            .collect();
        (samples, self.interval)
    }

    fn shared_samples(&self) -> Arc<Vec<AtomicU64>> {
        Arc::clone(&self.samples)
    }
}

const PROGRESS_POLL_OPERATIONS: u32 = 16_384;

struct Machine<'a> {
    tape: Vec<u8>,
    pointer: usize,
    max_pointer: usize,
    input: &'a [u8],
    input_position: usize,
    output: Vec<u8>,
    executed_instructions: u64,
    executed_rle_instructions: u64,
    optimization: OptimizationRunStats,
    grow_tape: bool,
    profile: Option<ProfileCollector>,
    sampled_site: Option<Arc<AtomicU32>>,
    sampled_counts: Option<Arc<Vec<AtomicU64>>>,
    published_sample_site: u32,
    clock_reads: u64,
    profile_block_executions: u64,
    mixed_provenance_native_operations: u64,
    progress: Option<ProgressOptions>,
    progress_started: Instant,
    next_progress: Option<Instant>,
    progress_countdown: u32,
}

impl<'a> Machine<'a> {
    fn new(
        input: &'a [u8],
        grow_tape: bool,
        profile_options: Option<&ProfileOptions>,
        sampled_site: Option<Arc<AtomicU32>>,
        sampled_counts: Option<Arc<Vec<AtomicU64>>>,
        progress: Option<&ProgressOptions>,
    ) -> Self {
        let progress_started = Instant::now();
        Self {
            tape: vec![0; TAPE_LEN],
            pointer: 0,
            max_pointer: 0,
            input,
            input_position: 0,
            output: Vec::new(),
            executed_instructions: 0,
            executed_rle_instructions: 0,
            optimization: OptimizationRunStats::default(),
            grow_tape,
            profile: profile_options.map(ProfileCollector::new),
            sampled_site,
            sampled_counts,
            published_sample_site: INACTIVE_PROFILE_SITE,
            clock_reads: 0,
            profile_block_executions: 0,
            mixed_provenance_native_operations: 0,
            progress: progress.cloned(),
            progress_started,
            next_progress: progress
                .and_then(|progress| progress.interval)
                .and_then(|interval| progress_started.checked_add(interval)),
            progress_countdown: PROGRESS_POLL_OPERATIONS,
        }
    }

    fn add_counts(&mut self, site: ResolvedProfileSite, raw: u64, rle: u64) {
        self.executed_instructions += raw;
        self.executed_rle_instructions += rle;
        if let Some(profile) = &mut self.profile {
            let counters = &mut profile.site_mut(site).counters;
            counters.raw_bf_instructions += raw;
            counters.rle_instructions += rle;
        }
    }

    fn record_maximum_pointer(&mut self, site: ResolvedProfileSite) {
        self.record_maximum_pointer_at(site, self.pointer);
    }

    fn record_maximum_pointer_at(&mut self, site: ResolvedProfileSite, pointer: usize) {
        if let Some(profile) = &mut self.profile {
            let counters = &mut profile.site_mut(site).counters;
            counters.maximum_pointer_observed = counters.maximum_pointer_observed.max(pointer);
        }
    }

    fn publish_sample_site(&mut self, site: ResolvedProfileSite) {
        let slot = u32::try_from(site.slot).expect("profile site count fits in u32");
        if self.published_sample_site != slot {
            if let Some(sampled_site) = &self.sampled_site {
                sampled_site.store(slot, Ordering::Release);
            }
            self.published_sample_site = slot;
        }
    }

    fn observe_progress(&mut self, site: ResolvedProfileSite) -> Result<(), Error> {
        if self.progress.is_none() {
            return Ok(());
        }
        self.progress_countdown -= 1;
        if self.progress_countdown != 0 {
            return Ok(());
        }
        self.progress_countdown = PROGRESS_POLL_OPERATIONS;

        let progress = self.progress.as_ref().unwrap();
        let interrupted = (progress.interrupted)();
        let now = Instant::now();
        let periodic = self.next_progress.is_some_and(|deadline| now >= deadline);
        if !interrupted && !periodic {
            return Ok(());
        }

        let snapshot = self.progress_snapshot(site, interrupted, now);
        let callback = Arc::clone(&progress.callback);
        callback(&snapshot);
        if periodic {
            self.next_progress = progress
                .interval
                .and_then(|interval| now.checked_add(interval));
        }
        if interrupted {
            return Err(Error::Interrupted);
        }
        Ok(())
    }

    fn progress_snapshot(
        &self,
        current_site: ResolvedProfileSite,
        interrupted: bool,
        now: Instant,
    ) -> ProgressSnapshot {
        let mut hot_sites = self
            .profile
            .as_ref()
            .map(|profile| {
                profile
                    .sites
                    .iter()
                    .enumerate()
                    .map(|(slot, site)| ProgressSiteSnapshot {
                        site: site.site,
                        counters: site.counters,
                        samples: self
                            .sampled_counts
                            .as_ref()
                            .and_then(|samples| samples.get(slot))
                            .map_or(0, |samples| samples.load(Ordering::Relaxed)),
                    })
                    .filter(|site| site.counters.fast_operations != 0 || site.samples != 0)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        hot_sites.sort_unstable_by_key(|site| {
            std::cmp::Reverse((site.samples, site.counters.fast_operations))
        });
        hot_sites.truncate(10);
        ProgressSnapshot {
            interrupted,
            elapsed: now.saturating_duration_since(self.progress_started),
            current_site: self.profile.as_ref().map(|_| current_site.id),
            input_bytes: self.input_position.min(self.input.len()),
            output_bytes: self.output.len(),
            pointer: self.pointer,
            stats: RunStats {
                executed_instructions: self.executed_instructions,
                executed_rle_instructions: self.executed_rle_instructions,
                max_pointer: self.max_pointer,
                optimization: self.optimization,
            },
            hot_sites,
        }
    }

    fn execute_block(&mut self, instructions: &[FastInstruction]) -> Result<(), Error> {
        match self.profile.as_ref().map(|profile| profile.mode) {
            None => self.execute_block_light::<false>(instructions),
            Some(ProfileMode::Sample { .. }) => self.execute_block_light::<true>(instructions),
            Some(ProfileMode::Counters | ProfileMode::Exact) => {
                self.execute_block_profiled(instructions)
            }
        }
    }

    fn add_counts_unprofiled(&mut self, raw: u64, rle: u64) {
        self.executed_instructions += raw;
        self.executed_rle_instructions += rle;
    }

    fn execute_block_light<const SAMPLE: bool>(
        &mut self,
        instructions: &[FastInstruction],
    ) -> Result<(), Error> {
        for instruction in instructions {
            self.observe_progress(instruction.site())?;
            if SAMPLE {
                self.publish_sample_site(instruction.site());
                if instruction.mixed_provenance() {
                    self.mixed_provenance_native_operations += 1;
                }
            }
            self.optimization.executed_native_operations += 1;
            match instruction {
                FastInstruction::Countdown { site, chain, .. } => {
                    self.execute_countdown::<false, SAMPLE>(*site, chain)?;
                }
                FastInstruction::Move {
                    amount,
                    source_offsets,
                    ..
                } => {
                    self.optimization.rle_operations += 1;
                    self.add_counts_unprofiled(source_offsets.len() as u64, 1);
                    self.move_pointer(*amount, source_offsets)?;
                }
                FastInstruction::Add {
                    amount, raw_count, ..
                } => {
                    self.optimization.rle_operations += 1;
                    self.add_counts_unprofiled(*raw_count, 1);
                    self.tape[self.pointer] = self.tape[self.pointer].wrapping_add(*amount);
                }
                FastInstruction::Input { .. } => {
                    self.add_counts_unprofiled(1, 1);
                    self.tape[self.pointer] =
                        self.input.get(self.input_position).copied().unwrap_or(0);
                    self.input_position = self.input_position.saturating_add(1);
                }
                FastInstruction::Output { .. } => {
                    self.add_counts_unprofiled(1, 1);
                    self.output.push(self.tape[self.pointer]);
                }
                FastInstruction::Loop {
                    body, optimization, ..
                } => self.execute_loop_light::<SAMPLE>(
                    instruction.site(),
                    body,
                    optimization.as_ref(),
                )?,
            }
        }
        Ok(())
    }

    /// Descend directly to the first zero condition, then unwind the original
    /// tails. A tail may change the guard or pointer, so re-entry is evaluated
    /// at its actual runtime location, without assuming any compiler ABI.
    fn execute_countdown<const PROFILE: bool, const SAMPLE: bool>(
        &mut self,
        site: ResolvedProfileSite,
        chain: &CountdownChain,
    ) -> Result<Duration, Error> {
        let mut remaining = chain.levels.len();
        let mut reentry = false;
        let mut nested_time = Duration::ZERO;
        loop {
            self.observe_progress(site)?;
            if SAMPLE {
                self.publish_sample_site(site);
            }
            let entered = usize::from(self.tape[self.pointer]).min(remaining);
            let skipped = remaining - entered;
            let decrement_raw = chain.levels[remaining - 1].decrement_prefix
                - if skipped == 0 {
                    0
                } else {
                    chain.levels[skipped - 1].decrement_prefix
                };
            let opens = entered as u64 + u64::from(skipped != 0) - u64::from(reentry);
            self.countdown_counts::<PROFILE>(site, decrement_raw + opens, entered as u64 + opens);
            self.optimization.rle_operations += entered as u64;
            if PROFILE && let Some(profile) = &mut self.profile {
                let counters = &mut profile.site_mut(site).counters;
                counters.loop_entries += opens;
                counters.loop_iterations += entered as u64;
                counters.rle_operations += entered as u64;
            }
            self.tape[self.pointer] -= entered as u8;
            if skipped == 0 {
                nested_time += self.execute_countdown_block::<PROFILE, SAMPLE>(
                    std::slice::from_ref(chain.leaf.as_ref()),
                )?;
            }
            let mut level = skipped;
            // A skipped level has no tail to execute. Its parent does.
            loop {
                if level == chain.levels.len() {
                    return Ok(nested_time);
                }
                nested_time +=
                    self.execute_countdown_block::<PROFILE, SAMPLE>(&chain.levels[level].tail)?;
                if SAMPLE {
                    self.publish_sample_site(site);
                }
                self.countdown_counts::<PROFILE>(site, 1, 1);
                if self.tape[self.pointer] != 0 {
                    remaining = level + 1;
                    reentry = true;
                    break;
                }
                level += 1;
            }
        }
    }

    fn countdown_counts<const PROFILE: bool>(
        &mut self,
        site: ResolvedProfileSite,
        raw: u64,
        rle: u64,
    ) {
        if PROFILE {
            self.add_counts(site, raw, rle);
        } else {
            self.add_counts_unprofiled(raw, rle);
        }
    }

    fn execute_countdown_block<const PROFILE: bool, const SAMPLE: bool>(
        &mut self,
        body: &[FastInstruction],
    ) -> Result<Duration, Error> {
        let exact = PROFILE && self.profile.as_ref().is_some_and(|profile| profile.exact);
        let started = exact.then(|| {
            self.clock_reads += 1;
            Instant::now()
        });
        if PROFILE {
            self.execute_block_profiled(body)?;
        } else {
            self.execute_block_light::<SAMPLE>(body)?;
        }
        Ok(if let Some(started) = started {
            self.clock_reads += 1;
            started.elapsed()
        } else {
            Duration::ZERO
        })
    }

    fn execute_loop_light<const SAMPLE: bool>(
        &mut self,
        site: ResolvedProfileSite,
        body: &[FastInstruction],
        optimization: Option<&LoopOptimization>,
    ) -> Result<(), Error> {
        match optimization {
            Some(LoopOptimization::Clear {
                delta,
                body_raw_count,
                body_rle_count,
            }) => {
                self.optimization.clear_loops += 1;
                let iterations = iterations_to_zero(self.tape[self.pointer], *delta);
                self.add_counts_unprofiled(
                    1 + iterations * (body_raw_count + 1),
                    1 + iterations * (body_rle_count + 1),
                );
                self.tape[self.pointer] = 0;
                Ok(())
            }
            Some(LoopOptimization::Scan {
                amount,
                source_offsets,
            }) => {
                self.optimization.scan_loops += 1;
                self.add_counts_unprofiled(1, 1);
                while self.tape[self.pointer] != 0 {
                    self.observe_progress(site)?;
                    let steps = self.scan_batch(*amount);
                    if steps == 0 {
                        // Preserve growth and the exact source offset on boundary errors.
                        self.move_pointer(*amount, source_offsets)?;
                    }
                    let steps = steps.max(1);
                    self.optimization.scan_steps += steps;
                    self.optimization.rle_operations += steps;
                    self.add_counts_unprofiled(
                        steps * (source_offsets.len() as u64 + 1),
                        2 * steps,
                    );
                }
                Ok(())
            }
            Some(LoopOptimization::Transfer {
                source_delta,
                updates,
                min_offset,
                max_offset,
                body_raw_count,
                body_rle_count,
                body_pointer_distance: _,
            }) => {
                let initial = self.tape[self.pointer];
                let iterations = if initial == 0 {
                    0
                } else if *source_delta == 255 {
                    u64::from(initial)
                } else {
                    u64::from(0_u8.wrapping_sub(initial))
                };
                if iterations != 0
                    && (!self.offset_is_valid(*min_offset) || !self.offset_is_valid(*max_offset))
                {
                    return self.execute_generic_loop_light::<SAMPLE>(body);
                }

                self.optimization.transfer_loops += 1;
                self.optimization.transfer_iterations += iterations;
                self.add_counts_unprofiled(
                    1 + iterations * (body_raw_count + 1),
                    1 + iterations * (body_rle_count + 1),
                );
                if iterations != 0 {
                    if self.grow_tape && *max_offset > 0 {
                        let reached = self.pointer.checked_add_signed(*max_offset).unwrap();
                        if reached >= self.tape.len() {
                            self.tape.resize(reached + 1, 0);
                        }
                    }
                    let factor = iterations as u8;
                    for &(offset, update) in updates.iter() {
                        let target = self.pointer.checked_add_signed(offset).unwrap();
                        self.tape[target] =
                            self.tape[target].wrapping_add(update.wrapping_mul(factor));
                    }
                    if *max_offset > 0 {
                        let reached = self.pointer.checked_add_signed(*max_offset).unwrap();
                        self.max_pointer = self.max_pointer.max(reached);
                    }
                    self.tape[self.pointer] = 0;
                }
                Ok(())
            }
            None => self.execute_generic_loop_light::<SAMPLE>(body),
        }
    }

    fn execute_generic_loop_light<const SAMPLE: bool>(
        &mut self,
        body: &[FastInstruction],
    ) -> Result<(), Error> {
        self.add_counts_unprofiled(1, 1);
        while self.tape[self.pointer] != 0 {
            self.execute_block_light::<SAMPLE>(body)?;
            self.add_counts_unprofiled(1, 1);
        }
        Ok(())
    }

    fn execute_block_profiled(&mut self, instructions: &[FastInstruction]) -> Result<(), Error> {
        for instruction in instructions {
            let site = instruction.site();
            self.observe_progress(site)?;
            if self.sampled_site.is_some() {
                self.publish_sample_site(site);
            }
            if instruction.mixed_provenance() {
                self.mixed_provenance_native_operations += 1;
            }
            self.optimization.executed_native_operations += 1;
            let exact = self.profile.as_ref().is_some_and(|profile| profile.exact);
            let started = exact.then(Instant::now);
            if exact {
                self.clock_reads += 1;
            }
            self.profile_block_executions += 1;
            if let Some(profile) = &mut self.profile {
                let site_profile = profile.site_mut(site);
                site_profile.counters.fast_operations += 1;
                site_profile.counters.maximum_pointer_observed = site_profile
                    .counters
                    .maximum_pointer_observed
                    .max(self.pointer);
                site_profile.profile_block_executions += 1;
            }
            let nested_time = match instruction {
                FastInstruction::Countdown { chain, .. } => {
                    self.execute_countdown::<true, false>(site, chain)?
                }
                FastInstruction::Move {
                    amount,
                    source_offsets,
                    ..
                } => {
                    self.optimization.rle_operations += 1;
                    self.add_counts(site, source_offsets.len() as u64, 1);
                    if let Some(profile) = &mut self.profile {
                        let counters = &mut profile.site_mut(site).counters;
                        counters.rle_operations += 1;
                        counters.pointer_distance += source_offsets.len() as u64;
                    }
                    self.move_pointer(*amount, source_offsets)?;
                    self.record_maximum_pointer(site);
                    Duration::ZERO
                }
                FastInstruction::Add {
                    amount, raw_count, ..
                } => {
                    self.optimization.rle_operations += 1;
                    self.add_counts(site, *raw_count, 1);
                    if let Some(profile) = &mut self.profile {
                        profile.site_mut(site).counters.rle_operations += 1;
                    }
                    self.tape[self.pointer] = self.tape[self.pointer].wrapping_add(*amount);
                    Duration::ZERO
                }
                FastInstruction::Input { .. } => {
                    self.add_counts(site, 1, 1);
                    if let Some(profile) = &mut self.profile {
                        profile.site_mut(site).counters.input_operations += 1;
                    }
                    self.tape[self.pointer] =
                        self.input.get(self.input_position).copied().unwrap_or(0);
                    self.input_position = self.input_position.saturating_add(1);
                    Duration::ZERO
                }
                FastInstruction::Output { .. } => {
                    self.add_counts(site, 1, 1);
                    if let Some(profile) = &mut self.profile {
                        profile.site_mut(site).counters.output_operations += 1;
                    }
                    self.output.push(self.tape[self.pointer]);
                    Duration::ZERO
                }
                FastInstruction::Loop {
                    body, optimization, ..
                } => {
                    if let Some(profile) = &mut self.profile {
                        profile.site_mut(site).counters.loop_entries += 1;
                    }
                    self.execute_loop(site, body, optimization.as_ref())?
                }
            };
            if let Some(started) = started {
                let elapsed = started.elapsed();
                self.clock_reads += 1;
                if let Some(profile) = &mut self.profile {
                    profile.site_mut(site).exclusive_time += elapsed.saturating_sub(nested_time);
                }
            }
        }
        Ok(())
    }

    fn execute_loop(
        &mut self,
        site: ResolvedProfileSite,
        body: &[FastInstruction],
        optimization: Option<&LoopOptimization>,
    ) -> Result<Duration, Error> {
        match optimization {
            Some(LoopOptimization::Clear {
                delta,
                body_raw_count,
                body_rle_count,
            }) => {
                self.optimization.clear_loops += 1;
                let iterations = iterations_to_zero(self.tape[self.pointer], *delta);
                self.add_counts(
                    site,
                    1 + iterations * (body_raw_count + 1),
                    1 + iterations * (body_rle_count + 1),
                );
                if let Some(profile) = &mut self.profile {
                    let counters = &mut profile.site_mut(site).counters;
                    counters.clear_loops += 1;
                    counters.loop_iterations += iterations;
                }
                self.tape[self.pointer] = 0;
                Ok(Duration::ZERO)
            }
            Some(LoopOptimization::Scan {
                amount,
                source_offsets,
            }) => {
                self.optimization.scan_loops += 1;
                self.add_counts(site, 1, 1);
                if let Some(profile) = &mut self.profile {
                    profile.site_mut(site).counters.scan_loops += 1;
                }
                let mut iterations = 0_u64;
                while self.tape[self.pointer] != 0 {
                    self.observe_progress(site)?;
                    let steps = self.scan_batch(*amount);
                    if steps == 0 {
                        self.move_pointer(*amount, source_offsets)?;
                    }
                    let steps = steps.max(1);
                    iterations += steps;
                    self.optimization.scan_steps += steps;
                    self.optimization.rle_operations += steps;
                    self.record_maximum_pointer(site);
                    self.add_counts(site, steps * (source_offsets.len() as u64 + 1), 2 * steps);
                }
                if let Some(profile) = &mut self.profile {
                    let counters = &mut profile.site_mut(site).counters;
                    counters.loop_iterations += iterations;
                    counters.scan_steps += iterations;
                    counters.rle_operations += iterations;
                    counters.pointer_distance += iterations * source_offsets.len() as u64;
                }
                Ok(Duration::ZERO)
            }
            Some(LoopOptimization::Transfer {
                source_delta,
                updates,
                min_offset,
                max_offset,
                body_raw_count,
                body_rle_count,
                body_pointer_distance,
            }) => {
                let initial = self.tape[self.pointer];
                let iterations = if initial == 0 {
                    0
                } else if *source_delta == 255 {
                    u64::from(initial)
                } else {
                    u64::from(0_u8.wrapping_sub(initial))
                };
                if iterations != 0
                    && (!self.offset_is_valid(*min_offset) || !self.offset_is_valid(*max_offset))
                {
                    return self.execute_generic_loop(site, body);
                }

                self.optimization.transfer_loops += 1;
                self.optimization.transfer_iterations += iterations;
                self.add_counts(
                    site,
                    1 + iterations * (body_raw_count + 1),
                    1 + iterations * (body_rle_count + 1),
                );
                if let Some(profile) = &mut self.profile {
                    let counters = &mut profile.site_mut(site).counters;
                    counters.transfer_loops += 1;
                    counters.transfer_iterations += iterations;
                    counters.loop_iterations += iterations;
                    counters.pointer_distance += iterations * body_pointer_distance;
                }
                if iterations != 0 {
                    if self.grow_tape && *max_offset > 0 {
                        let reached = self.pointer.checked_add_signed(*max_offset).unwrap();
                        if reached >= self.tape.len() {
                            self.tape.resize(reached + 1, 0);
                        }
                    }
                    let factor = iterations as u8;
                    for &(offset, update) in updates.iter() {
                        let target = self.pointer.checked_add_signed(offset).unwrap();
                        self.tape[target] =
                            self.tape[target].wrapping_add(update.wrapping_mul(factor));
                    }
                    if *max_offset > 0 {
                        let reached = self.pointer.checked_add_signed(*max_offset).unwrap();
                        self.max_pointer = self.max_pointer.max(reached);
                    }
                    self.tape[self.pointer] = 0;
                    let reached = self
                        .pointer
                        .checked_add_signed((*max_offset).max(0))
                        .unwrap();
                    self.record_maximum_pointer_at(site, reached);
                }
                Ok(Duration::ZERO)
            }
            None => self.execute_generic_loop(site, body),
        }
    }

    fn execute_generic_loop(
        &mut self,
        site: ResolvedProfileSite,
        body: &[FastInstruction],
    ) -> Result<Duration, Error> {
        self.add_counts(site, 1, 1);
        let mut nested_time = Duration::ZERO;
        let mut iterations = 0_u64;
        let exact = self.profile.as_ref().is_some_and(|profile| profile.exact);
        while self.tape[self.pointer] != 0 {
            iterations += 1;
            let body_started = exact.then(|| {
                self.clock_reads += 1;
                Instant::now()
            });
            self.execute_block(body)?;
            if let Some(body_started) = body_started {
                nested_time += body_started.elapsed();
                self.clock_reads += 1;
            }
            self.publish_sample_site(site);
            self.add_counts(site, 1, 1);
        }
        if let Some(profile) = &mut self.profile {
            profile.site_mut(site).counters.loop_iterations += iterations;
        }
        Ok(nested_time)
    }

    /// Scan only within allocated tape, amortizing bookkeeping over bounded batches.
    /// The caller handles the next move at a boundary with normal BF error/growth semantics.
    fn scan_batch(&mut self, amount: isize) -> u64 {
        let stride = amount.unsigned_abs();
        if stride == 0 {
            return 0;
        }
        let available = if amount > 0 {
            (self.tape.len() - 1 - self.pointer) / stride
        } else {
            self.pointer / stride
        };
        // Return regularly to the progress hook even for very long scans.
        let limit = available.min(1024);
        let mut pointer = self.pointer;
        let mut steps = 0;
        while steps < limit && self.tape[pointer] != 0 {
            pointer = pointer.wrapping_add_signed(amount);
            steps += 1;
        }
        self.pointer = pointer;
        self.max_pointer = self.max_pointer.max(pointer);
        steps as u64
    }

    fn offset_is_valid(&self, offset: isize) -> bool {
        self.pointer
            .checked_add_signed(offset)
            .is_some_and(|position| self.grow_tape || position < self.tape.len())
    }

    fn move_pointer(&mut self, amount: isize, source_offsets: &SourceOffsets) -> Result<(), Error> {
        if amount > 0 {
            let count = amount as usize;
            let Some(destination) = self.pointer.checked_add(count) else {
                let valid_steps = self.tape.len() - 1 - self.pointer;
                return Err(Error::TapeOverflow {
                    instruction_offset: source_offsets.get(valid_steps),
                });
            };
            if destination >= self.tape.len() {
                if self.grow_tape {
                    self.tape.resize(destination + 1, 0);
                } else {
                    let valid_steps = self.tape.len() - 1 - self.pointer;
                    return Err(Error::TapeOverflow {
                        instruction_offset: source_offsets.get(valid_steps),
                    });
                }
            }
            self.pointer = destination;
            self.max_pointer = self.max_pointer.max(self.pointer);
        } else {
            let count = amount.unsigned_abs();
            if count > self.pointer {
                return Err(Error::TapeUnderflow {
                    instruction_offset: source_offsets.get(self.pointer),
                });
            }
            self.pointer -= count;
        }
        Ok(())
    }
}

fn iterations_to_zero(initial: u8, delta: u8) -> u64 {
    let mut value = initial;
    let mut iterations = 0;
    while value != 0 {
        value = value.wrapping_add(delta);
        iterations += 1;
    }
    iterations
}

fn execute(
    instructions: &[FastInstruction],
    input: &[u8],
    grow_tape: bool,
    profile_options: Option<&ProfileOptions>,
    progress: Option<&ProgressOptions>,
) -> Result<RunResult, Error> {
    let site_slots = profile_options.map_or(0, |options| options.map.sites.len());
    let sampling = SamplingSession::start(profile_options, site_slots);
    let sampled_site = sampling
        .as_ref()
        .map(|session| Arc::clone(&session.current_site));
    let sampled_counts = sampling.as_ref().map(SamplingSession::shared_samples);
    let mut machine = Machine::new(
        input,
        grow_tape,
        profile_options,
        sampled_site,
        sampled_counts,
        progress,
    );
    let execution = machine.execute_block(instructions);
    if let Some(sampled_site) = &machine.sampled_site {
        sampled_site.store(INACTIVE_PROFILE_SITE, Ordering::Release);
    }
    let sample_result = sampling.map(SamplingSession::finish);
    execution?;

    let profile = machine.profile.take().map(|mut collector| {
        let (sample_counts, sampling_interval) = match sample_result {
            Some((samples, interval)) => (Some(samples), Some(interval)),
            None => (None, None),
        };
        let mut total_samples = 0_u64;
        if let Some(sample_counts) = sample_counts {
            for (slot, site) in collector.sites.iter_mut().enumerate() {
                site.samples = sample_counts[slot];
                total_samples += site.samples;
            }
        }
        ProfileResult {
            sites: collector.sites,
            total_samples,
            sampling_interval,
            clock_reads: machine.clock_reads,
            profile_block_executions: machine.profile_block_executions,
            measured_execute_time: Duration::ZERO,
            mixed_provenance_native_operations: machine.mixed_provenance_native_operations,
        }
    });
    Ok(RunResult {
        output: machine.output,
        stats: RunStats {
            executed_instructions: machine.executed_instructions,
            executed_rle_instructions: machine.executed_rle_instructions,
            max_pointer: machine.max_pointer,
            optimization: machine.optimization,
        },
        timings: None,
        profile,
    })
}

#[cfg(test)]
fn execute_reference(instructions: &[Instruction], input: &[u8]) -> Result<RunResult, Error> {
    let mut tape = vec![0_u8; TAPE_LEN];
    let mut pointer = 0_usize;
    let mut max_pointer = 0_usize;
    let mut program_counter = 0_usize;
    let mut input_position = 0_usize;
    let mut output = Vec::new();
    let mut executed_instructions = 0_u64;
    let mut executed_rle_instructions = 0_u64;
    let mut previous_rle_op: Option<(usize, RleOp)> = None;

    while let Some(instruction) = instructions.get(program_counter) {
        executed_instructions += 1;
        let rle_op = instruction.op.rle_op();
        let continues_previous_run = matches!(
            (previous_rle_op, rle_op),
            (Some((previous_counter, previous_op)), Some(current_op))
                if previous_counter.checked_add(1) == Some(program_counter)
                    && previous_op == current_op
        );
        if !continues_previous_run {
            executed_rle_instructions += 1;
        }
        previous_rle_op = rle_op.map(|op| (program_counter, op));

        match instruction.op {
            Op::Right => {
                if pointer + 1 == TAPE_LEN {
                    return Err(Error::TapeOverflow {
                        instruction_offset: instruction.source_offset,
                    });
                }
                pointer += 1;
                max_pointer = max_pointer.max(pointer);
                program_counter += 1;
            }
            Op::Left => {
                if pointer == 0 {
                    return Err(Error::TapeUnderflow {
                        instruction_offset: instruction.source_offset,
                    });
                }
                pointer -= 1;
                program_counter += 1;
            }
            Op::Increment => {
                tape[pointer] = tape[pointer].wrapping_add(1);
                program_counter += 1;
            }
            Op::Decrement => {
                tape[pointer] = tape[pointer].wrapping_sub(1);
                program_counter += 1;
            }
            Op::Output => {
                output.push(tape[pointer]);
                program_counter += 1;
            }
            Op::Input => {
                tape[pointer] = input.get(input_position).copied().unwrap_or(0);
                input_position = input_position.saturating_add(1);
                program_counter += 1;
            }
            Op::JumpIfZero(target) => {
                program_counter = if tape[pointer] == 0 {
                    target
                } else {
                    program_counter + 1
                };
            }
            Op::JumpIfNonZero(target) => {
                program_counter = if tape[pointer] != 0 {
                    target
                } else {
                    program_counter + 1
                };
            }
        }
    }

    Ok(RunResult {
        output,
        stats: RunStats {
            executed_instructions,
            executed_rle_instructions,
            max_pointer,
            optimization: OptimizationRunStats::default(),
        },
        timings: None,
        profile: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bf_profiling::{
        PROFILE_MAP_FORMAT, PROFILE_MAP_VERSION, ProfileRange, ProfileSite, bf_identity,
    };
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    static TEST_INTERRUPTED: AtomicBool = AtomicBool::new(false);

    fn test_interrupted() -> bool {
        TEST_INTERRUPTED.load(Ordering::Relaxed)
    }

    fn run_reference(source: &[u8], input: &[u8]) -> Result<RunResult, Error> {
        let instructions = parse(source)?;
        execute_reference(&instructions, input)
    }

    fn assert_matches_reference(source: &[u8], input: &[u8]) -> RunResult {
        let optimized = run_with_stats(source, input).unwrap();
        let reference = run_reference(source, input).unwrap();
        assert_eq!(optimized.output, reference.output);
        assert_eq!(
            optimized.stats.executed_instructions,
            reference.stats.executed_instructions,
        );
        assert_eq!(
            optimized.stats.executed_rle_instructions,
            reference.stats.executed_rle_instructions,
        );
        assert_eq!(optimized.stats.max_pointer, reference.stats.max_pointer);
        optimized
    }

    #[test]
    fn progress_callback_snapshots_before_interrupting() {
        let source = b"+.".repeat(PROGRESS_POLL_OPERATIONS as usize);
        let snapshot = Arc::new(Mutex::new(None));
        let callback_snapshot = Arc::clone(&snapshot);
        TEST_INTERRUPTED.store(true, Ordering::Relaxed);
        let result = run_with_options(
            &source,
            b"",
            RunOptions {
                progress: Some(ProgressOptions {
                    interval: None,
                    interrupted: test_interrupted,
                    callback: Arc::new(move |progress| {
                        *callback_snapshot.lock().unwrap() = Some(progress.clone());
                    }),
                }),
                ..RunOptions::default()
            },
        );
        TEST_INTERRUPTED.store(false, Ordering::Relaxed);

        assert_eq!(result, Err(Error::Interrupted));
        let snapshot = snapshot.lock().unwrap().clone().unwrap();
        assert!(snapshot.interrupted);
        assert_eq!(
            snapshot.stats.optimization.executed_native_operations,
            u64::from(PROGRESS_POLL_OPERATIONS - 1),
        );
        assert_eq!(
            snapshot.output_bytes,
            ((PROGRESS_POLL_OPERATIONS - 1) / 2) as usize,
        );
    }

    fn test_profile_map(source: &[u8], ranges: Vec<ProfileRange>) -> ProfileMap {
        ProfileMap {
            format: PROFILE_MAP_FORMAT.into(),
            version: PROFILE_MAP_VERSION,
            bf: bf_identity(source),
            files: Vec::new(),
            sites: vec![
                ProfileSite {
                    id: ProfileSiteId(0),
                    parent: None,
                    kind: "artifact".into(),
                    stable_key: "artifact.root".into(),
                    label: "test".into(),
                    source: None,
                    attributes: BTreeMap::new(),
                },
                ProfileSite {
                    id: ProfileSiteId(1),
                    parent: Some(ProfileSiteId(0)),
                    kind: "test".into(),
                    stable_key: "test.one".into(),
                    label: "one".into(),
                    source: None,
                    attributes: BTreeMap::new(),
                },
                ProfileSite {
                    id: ProfileSiteId(2),
                    parent: Some(ProfileSiteId(0)),
                    kind: "test".into(),
                    stable_key: "test.two".into(),
                    label: "two".into(),
                    source: None,
                    attributes: BTreeMap::new(),
                },
            ],
            ranges,
        }
    }

    #[test]
    fn ignores_comments_and_runs_a_loop() {
        assert_eq!(run_str("+++ add three [>++<-] >.", ""), Ok("\u{6}".into()));
    }

    #[test]
    fn compressed_runs_match_plain_execution_and_profiles() {
        let compressed = b"@BFCRLE1;,+257[->3+2<3]>3.[-]+>+>+<[>]+[<]>.[-]";
        let mut plain = Vec::new();
        for run in bf_profiling::rle::Runs::new(compressed) {
            let run = run.unwrap();
            plain.extend(std::iter::repeat_n(run.byte, run.count));
        }
        // Deliberately place profile boundaries *inside* compressed runs.
        let map = test_profile_map(
            &plain,
            (0..plain.len())
                .map(|i| ProfileRange {
                    start: i as u64,
                    end: i as u64 + 1,
                    site: ProfileSiteId((i % 2 + 1) as u32),
                })
                .collect(),
        );
        let embedded =
            bf_profiling::embed_profile_markers(std::str::from_utf8(compressed).unwrap(), &map)
                .unwrap();
        let embedded_map = bf_profiling::embedded_profile_map(embedded.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(embedded_map.bf, map.bf);
        assert_eq!(embedded_map.ranges, map.ranges);
        for input in [0, 1, 127, 254, 255] {
            let baseline = run_with_stats(&plain, &[input]).unwrap();
            for source in [compressed.as_slice(), embedded.as_bytes()] {
                assert_eq!(run_with_stats(source, &[input]).unwrap(), baseline);
                for mode in [
                    ProfileMode::Counters,
                    ProfileMode::Exact,
                    ProfileMode::Sample {
                        interval: Duration::from_millis(1),
                    },
                ] {
                    let options = RunOptions {
                        collect_stats: true,
                        profile: Some(ProfileOptions {
                            map: map.clone(),
                            mode,
                        }),
                        ..RunOptions::default()
                    };
                    let expected = run_with_options(&plain, &[input], options.clone()).unwrap();
                    let actual = run_with_options(source, &[input], options).unwrap();
                    assert_eq!(actual.output, expected.output);
                    assert_eq!(actual.stats, expected.stats);
                    let actual = actual.profile.unwrap();
                    let expected = expected.profile.unwrap();
                    assert_eq!(
                        actual.mixed_provenance_native_operations,
                        expected.mixed_provenance_native_operations
                    );
                    for (a, b) in actual.sites.iter().zip(&expected.sites) {
                        assert_eq!(a.counters, b.counters);
                    }
                }
            }
        }
    }

    #[test]
    fn compressed_counts_are_opt_in_and_errors_use_physical_offsets() {
        assert_eq!(run(b"+163.", b"").unwrap(), [1]);
        assert_eq!(run(b"@P163;+163.", b"").unwrap(), [1]);
        assert_eq!(run(b"@BFCRLE1;+163.", b"").unwrap(), [163]);
        assert_eq!(run(b"@BFCRLE1;+513.", b"").unwrap(), [1]);
        for source in [
            "@BFCRLE1;+0",
            "@BFCRLE1;>999999999999999999999999999",
            "@BFCRLE2;+",
        ] {
            assert!(matches!(
                run(source.as_bytes(), b""),
                Err(Error::InvalidEncoding(_))
            ));
        }
        let offset = bf_profiling::rle::HEADER.len();
        assert_eq!(
            run(b"@BFCRLE1;<16", b""),
            Err(Error::TapeUnderflow {
                instruction_offset: offset
            })
        );
        assert_eq!(
            run(b"@BFCRLE1;>30000", b""),
            Err(Error::TapeOverflow {
                instruction_offset: offset
            })
        );
        assert_eq!(run(b"@BFCRLE1;[<16]", b"").unwrap(), b"");
        assert_eq!(
            run(b"@BFCRLE1;>16[", b""),
            Err(Error::UnmatchedOpeningBracket { offset: offset + 3 })
        );
        let grown = run_unbounded_with_stats(b"@BFCRLE1;>30000+.<30000.", b"").unwrap();
        assert_eq!(grown.output, [1, 0]);
        assert_eq!(grown.stats.max_pointer, 30000);
        // Parsing a billion moves must allocate one run, not a billion instructions/offsets.
        let parsed = parse_optimized(b"@BFCRLE1;>1000000000", None).unwrap();
        let [
            FastInstruction::Move {
                source_offsets,
                amount,
                ..
            },
        ] = parsed.as_slice()
        else {
            panic!()
        };
        assert_eq!(*amount, 1_000_000_000);
        assert_eq!(source_offsets.ranges.len(), 1);
        assert_eq!(source_offsets.get(999_999_999), offset);
    }

    #[test]
    fn increment_and_decrement_wrap() {
        assert_eq!(run(b"-.", b""), Ok(vec![255]));
        assert_eq!(run(b"-.+.", b""), Ok(vec![255, 0]));
    }

    #[test]
    fn input_and_output_are_binary() {
        assert_eq!(run(b",.,.,.", &[0, 128, 255]), Ok(vec![0, 128, 255]));
    }

    #[test]
    fn reports_execution_measurements() {
        let result = run_with_stats(b"++[>++<-]>.", b"").unwrap();
        assert_eq!(result.output, vec![4]);
        assert_eq!(result.stats.executed_instructions, 17);
        assert_eq!(result.stats.executed_rle_instructions, 14);
        assert_eq!(result.stats.max_pointer, 1);
    }

    #[test]
    fn eof_sets_the_current_cell_to_zero() {
        assert_eq!(run(b"+,.", b""), Ok(vec![0]));
    }

    #[test]
    fn zero_skips_nested_loop() {
        assert_eq!(run(b"[+[.-]].", b""), Ok(vec![0]));
    }

    #[test]
    fn reports_unmatched_brackets_using_source_offsets() {
        assert_eq!(
            run(b"comment [ +", b""),
            Err(Error::UnmatchedOpeningBracket { offset: 8 })
        );
        assert_eq!(
            run(b"abc]", b""),
            Err(Error::UnmatchedClosingBracket { offset: 3 })
        );
    }

    #[test]
    fn checks_both_tape_boundaries() {
        assert_eq!(
            run(b"comment <", b""),
            Err(Error::TapeUnderflow {
                instruction_offset: 8
            })
        );

        let source = vec![b'>'; TAPE_LEN];
        assert_eq!(
            run(&source, b""),
            Err(Error::TapeOverflow {
                instruction_offset: TAPE_LEN - 1
            })
        );
    }

    #[test]
    fn unbounded_mode_grows_right_without_changing_standard_mode() {
        let mut source = vec![b'>'; TAPE_LEN + 7];
        source.extend_from_slice(b"+.");
        assert!(matches!(run(&source, b""), Err(Error::TapeOverflow { .. })));

        let result = run_unbounded_with_stats(&source, b"").unwrap();
        assert_eq!(result.output, vec![1]);
        assert_eq!(result.stats.max_pointer, TAPE_LEN + 7);
        assert_eq!(result.stats.executed_instructions, (TAPE_LEN + 9) as u64);
    }

    #[test]
    fn string_wrapper_rejects_non_utf8_output() {
        assert_eq!(run_str("-.", ""), Err(Error::InvalidUtf8Output));
    }

    #[test]
    fn optimized_rle_clear_scan_and_transfer_match_the_raw_vm_stats() {
        let clear = assert_matches_reference(b"+++++[-].", b"");
        assert_eq!(clear.stats.optimization.clear_loops, 1);

        // Scan over three nonzero cells and stop at the first zero cell.
        let scan = assert_matches_reference(b"+>+>+<<[>].", b"");
        assert_eq!(scan.stats.optimization.scan_loops, 1);
        assert_eq!(scan.stats.optimization.scan_steps, 3);

        let transfer = assert_matches_reference(b"++++[->>>++>+<<<<]>>>.>.", b"");
        assert_eq!(transfer.output, vec![8, 4]);
        assert_eq!(transfer.stats.optimization.transfer_loops, 1);
        assert_eq!(transfer.stats.optimization.transfer_iterations, 4);

        let wrapping_rle = assert_matches_reference(
            b"++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++\
              ---------------------------------------------------------------->><<.",
            b"",
        );
        assert!(wrapping_rle.stats.optimization.rle_operations >= 4);
    }

    #[test]
    fn batched_scans_preserve_stride_stops_counts_and_boundaries() {
        for stride in [1, 3, 17] {
            for cells in [0, 1, 1023, 1024, 1025, 1500] {
                let right = ">".repeat(stride);
                let left = "<".repeat(stride);
                // Scan both directions across multiple batch boundaries.
                let mut source = right.clone();
                source.push_str(&format!("+{right}").repeat(cells));
                source.push_str(&left.repeat(cells));
                source.push_str(&format!("[{right}].{left}[{left}]."));
                let baseline = assert_matches_reference(source.as_bytes(), b"");
                let map = test_profile_map(
                    source.as_bytes(),
                    vec![ProfileRange {
                        start: 0,
                        end: bf_identity(source.as_bytes()).instruction_count,
                        site: ProfileSiteId(1),
                    }],
                );
                let profiled = run_with_options(
                    source.as_bytes(),
                    b"",
                    RunOptions {
                        collect_stats: true,
                        profile: Some(ProfileOptions {
                            map,
                            mode: ProfileMode::Counters,
                        }),
                        ..RunOptions::default()
                    },
                )
                .unwrap();
                assert_eq!(profiled.output, baseline.output);
                assert_eq!(profiled.stats, baseline.stats);
            }
        }
        for source in [b"+ [< ignored <<]".to_vec(), {
            let mut source = vec![b'>'; TAPE_LEN - 2];
            source.extend_from_slice(b"+[> ignored >>]");
            source
        }] {
            assert_eq!(run_with_stats(&source, b""), run_reference(&source, b""));
        }
        let mut source = vec![b'>'; TAPE_LEN - 2];
        source.extend_from_slice(b"+[>>>].");
        let result = run_unbounded_with_stats(&source, b"").unwrap();
        assert_eq!(result.output, vec![0]);
        assert_eq!(result.stats.max_pointer, TAPE_LEN + 1);
        assert_eq!(result.stats.optimization.scan_steps, 1);
    }

    fn countdown_test_program(depth: usize) -> String {
        let mut dispatch = "[[-]>[-]<]".to_owned();
        for level in (0..depth).rev() {
            dispatch = format!("[-{dispatch}>[->{}.<]<]", "+".repeat(level + 1));
        }
        format!(">+<,{dispatch}>.>.")
    }

    #[test]
    fn countdown_chains_match_reference_for_every_guard_value() {
        for depth in [1, 2, 7, 79, 255, 256] {
            let source = countdown_test_program(depth);
            for initial in 0..=255_u8 {
                assert_matches_reference(source.as_bytes(), &[initial]);
            }
            let parsed = parse_optimized(source.as_bytes(), None).unwrap();
            assert!(parsed.iter().any(|instruction| matches!(instruction,
                FastInstruction::Countdown { chain, .. } if chain.levels.len() == depth)));
        }
    }

    #[test]
    fn countdown_reentry_uses_current_guard_and_pointer() {
        for (source, input) in [
            (b"++[-[-],].".as_slice(), &[3, 1, 0][..]),
            (b"++>+++<[-[-]>].".as_slice(), &[][..]),
            (b"+++[-[-[-],],].".as_slice(), &[4, 2, 0, 3, 0, 0][..]),
            (b"++[--[-]].".as_slice(), &[][..]),
            (b"++[- >[-]<].".as_slice(), &[][..]),
        ] {
            assert_matches_reference(source, input);
        }
        // RLE's modulo-256 delta, rather than the spelling or run length,
        // determines whether the prefix is a decrement.
        for decrement in ["-".repeat(257), "+".repeat(255)] {
            let source = format!("+++[{decrement}[-]].");
            assert_matches_reference(source.as_bytes(), b"");
        }
        let source = b"++[-[-]<]";
        assert_eq!(run_with_stats(source, b""), run_reference(source, b""));
        let mut source = vec![b'>'; TAPE_LEN - 1];
        source.extend_from_slice(b"++[-[-]>].");
        assert_eq!(run_with_stats(&source, b""), run_reference(&source, b""));
        let grown = run_unbounded_with_stats(&source, b"").unwrap();
        assert_eq!(grown.output, vec![0]);
        assert_eq!(grown.stats.max_pointer, TAPE_LEN);
    }

    #[test]
    fn countdown_profiles_preserve_counts_and_case_attribution() {
        let source = countdown_test_program(7);
        // Give each output its own site, leaving the chain on site one.
        let ranges = source
            .bytes()
            .enumerate()
            .map(|(index, byte)| ProfileRange {
                start: index as u64,
                end: index as u64 + 1,
                site: ProfileSiteId(if byte == b'.' { 2 } else { 1 }),
            })
            .collect();
        let map = test_profile_map(source.as_bytes(), ranges);
        for initial in [0, 1, 3, 7, 8, 255] {
            let baseline = run_with_stats(source.as_bytes(), &[initial]).unwrap();
            for mode in [
                ProfileMode::Counters,
                ProfileMode::Exact,
                ProfileMode::Sample {
                    interval: Duration::from_millis(1),
                },
            ] {
                let result = run_with_options(
                    source.as_bytes(),
                    &[initial],
                    RunOptions {
                        collect_stats: true,
                        profile: Some(ProfileOptions {
                            map: map.clone(),
                            mode,
                        }),
                        ..RunOptions::default()
                    },
                )
                .unwrap();
                assert_eq!(result.output, baseline.output);
                assert_eq!(result.stats, baseline.stats);
                let profile = result.profile.unwrap();
                if !matches!(mode, ProfileMode::Sample { .. }) {
                    assert_eq!(
                        profile
                            .sites
                            .iter()
                            .map(|s| s.counters.raw_bf_instructions)
                            .sum::<u64>(),
                        baseline.stats.executed_instructions
                    );
                    assert_eq!(
                        profile
                            .sites
                            .iter()
                            .map(|s| s.counters.rle_instructions)
                            .sum::<u64>(),
                        baseline.stats.executed_rle_instructions
                    );
                    assert_eq!(
                        profile
                            .sites
                            .iter()
                            .map(|s| s.counters.rle_operations)
                            .sum::<u64>(),
                        baseline.stats.optimization.rle_operations
                    );
                    assert_eq!(
                        profile
                            .sites
                            .iter()
                            .map(|s| s.counters.fast_operations)
                            .sum::<u64>(),
                        baseline.stats.optimization.executed_native_operations
                    );
                    assert_eq!(profile.sites[1].counters.output_operations, 0);
                    assert_eq!(
                        profile.sites[2].counters.output_operations,
                        baseline.output.len() as u64
                    );
                }
            }
        }
    }

    #[test]
    fn generic_nested_loops_and_io_match_the_raw_vm_stats() {
        assert_matches_reference(b",[>+.<-]", &[3]);
        assert_matches_reference(b"++[>++[>+<-]<-]>>.", b"");
        assert_matches_reference(b"[+[.-]].", b"");
    }

    #[test]
    fn optimized_boundary_errors_keep_raw_source_offsets() {
        let underflow = b"comments <  <<";
        assert_eq!(
            run_with_stats(underflow, b""),
            run_reference(underflow, b"")
        );

        let mut overflow = vec![b'>'; TAPE_LEN + 8];
        overflow.splice(10..10, b" ignored ".iter().copied());
        assert_eq!(
            run_with_stats(&overflow, b""),
            run_reference(&overflow, b""),
        );
    }

    #[test]
    fn unprofiled_parser_compacts_pointer_source_offsets_into_ranges() {
        let optimized = parse_optimized(b">>> comment >>>", None).unwrap();
        let [
            FastInstruction::Move {
                amount,
                source_offsets,
                ..
            },
        ] = optimized.as_slice()
        else {
            panic!("expected one coalesced pointer run");
        };
        assert_eq!(*amount, 6);
        assert_eq!(source_offsets.len(), 6);
        assert_eq!(source_offsets.ranges.len(), 2);
        assert_eq!(source_offsets.get(0), 0);
        assert_eq!(source_offsets.get(2), 2);
        assert_eq!(source_offsets.get(3), 12);
        assert_eq!(source_offsets.get(5), 14);
    }

    #[test]
    fn optimized_loop_counts_match_the_raw_vm_for_every_cell_value() {
        for initial in 0_u16..=255 {
            let mut clear = "+".repeat(usize::from(initial));
            clear.push_str("[-].");
            assert_matches_reference(clear.as_bytes(), b"");

            let mut incrementing_clear = "+".repeat(usize::from(initial));
            incrementing_clear.push_str("[+].");
            assert_matches_reference(incrementing_clear.as_bytes(), b"");

            let mut transfer = "+".repeat(usize::from(initial));
            transfer.push_str("[->>++>+<<<]>>.>.");
            assert_matches_reference(transfer.as_bytes(), b"");

            let mut incrementing_transfer = "+".repeat(usize::from(initial));
            incrementing_transfer.push_str("[+>+<]>.");
            assert_matches_reference(incrementing_transfer.as_bytes(), b"");
        }

        for distance in 1..=32 {
            let mut scan = String::new();
            for _ in 0..distance {
                scan.push_str("+>");
            }
            scan.push_str(&"<".repeat(distance));
            scan.push_str("[>].");
            assert_matches_reference(scan.as_bytes(), b"");
        }
    }

    #[test]
    fn profile_counters_preserve_semantics_and_global_counts() {
        let source = b"+++.>";
        let map = test_profile_map(
            source,
            vec![
                ProfileRange {
                    start: 0,
                    end: 2,
                    site: ProfileSiteId(1),
                },
                ProfileRange {
                    start: 2,
                    end: 5,
                    site: ProfileSiteId(2),
                },
            ],
        );
        let baseline = run_with_stats(source, b"").unwrap();
        let result = run_with_options(
            source,
            b"",
            RunOptions {
                collect_stats: true,
                collect_timings: true,
                profile: Some(ProfileOptions {
                    map,
                    mode: ProfileMode::Counters,
                }),
                ..RunOptions::default()
            },
        )
        .unwrap();

        assert_eq!(result.output, baseline.output);
        assert_eq!(result.stats, baseline.stats);
        assert!(result.timings.is_some());
        let profile = result.profile.unwrap();
        assert_eq!(profile.mixed_provenance_native_operations, 1);
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|site| site.counters.raw_bf_instructions)
                .sum::<u64>(),
            result.stats.executed_instructions
        );
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|site| site.counters.rle_instructions)
                .sum::<u64>(),
            result.stats.executed_rle_instructions
        );
        assert_eq!(profile.sites[0].counters.raw_bf_instructions, 3);
    }

    #[test]
    fn native_loops_are_attributed_to_the_lca_of_their_provenance() {
        for (source, expected_counter) in [
            (b"+[-]".as_slice(), "clear"),
            (b"+>+>+<<[>]".as_slice(), "scan"),
            (b"+[->+<]".as_slice(), "transfer"),
        ] {
            let instruction_count = bf_identity(source).instruction_count;
            let map = test_profile_map(
                source,
                vec![
                    ProfileRange {
                        start: 0,
                        end: instruction_count - 2,
                        site: ProfileSiteId(1),
                    },
                    ProfileRange {
                        start: instruction_count - 2,
                        end: instruction_count,
                        site: ProfileSiteId(2),
                    },
                ],
            );
            let profile = run_with_options(
                source,
                b"",
                RunOptions {
                    profile: Some(ProfileOptions {
                        map,
                        mode: ProfileMode::Counters,
                    }),
                    ..RunOptions::default()
                },
            )
            .unwrap()
            .profile
            .unwrap();

            let counters = &profile.sites[0].counters;
            match expected_counter {
                "clear" => assert_eq!(counters.clear_loops, 1),
                "scan" => assert_eq!(counters.scan_loops, 1),
                "transfer" => assert_eq!(counters.transfer_loops, 1),
                _ => unreachable!(),
            }
            assert_eq!(profile.mixed_provenance_native_operations, 1);
        }
    }

    #[test]
    fn profile_maximum_pointer_includes_native_operation_destinations() {
        for (source, expected_maximum) in [
            (b">>".as_slice(), 2),
            (b"+>+>+<<[>]".as_slice(), 3),
            (b"+[->>+<<]".as_slice(), 2),
        ] {
            let map = test_profile_map(
                source,
                vec![ProfileRange {
                    start: 0,
                    end: bf_identity(source).instruction_count,
                    site: ProfileSiteId(1),
                }],
            );
            let result = run_with_options(
                source,
                b"",
                RunOptions {
                    profile: Some(ProfileOptions {
                        map,
                        mode: ProfileMode::Counters,
                    }),
                    ..RunOptions::default()
                },
            )
            .unwrap();
            assert_eq!(result.stats.max_pointer, expected_maximum);
            assert_eq!(
                result.profile.unwrap().sites[1]
                    .counters
                    .maximum_pointer_observed,
                expected_maximum,
            );
        }
    }

    #[test]
    fn exact_and_sample_modes_return_overhead_metadata() {
        let source = b"++++[->+<]>.";
        let map = test_profile_map(
            source,
            vec![ProfileRange {
                start: 0,
                end: bf_identity(source).instruction_count,
                site: ProfileSiteId(1),
            }],
        );
        let exact = run_with_options(
            source,
            b"",
            RunOptions {
                profile: Some(ProfileOptions {
                    map: map.clone(),
                    mode: ProfileMode::Exact,
                }),
                ..RunOptions::default()
            },
        )
        .unwrap();
        let exact_profile = exact.profile.unwrap();
        assert_eq!(
            exact_profile.clock_reads,
            exact_profile.profile_block_executions * 2
        );

        let sampled = run_with_options(
            source,
            b"",
            RunOptions {
                profile: Some(ProfileOptions {
                    map,
                    mode: ProfileMode::Sample {
                        interval: Duration::from_millis(1),
                    },
                }),
                ..RunOptions::default()
            },
        )
        .unwrap();
        let sampled_profile = sampled.profile.unwrap();
        assert_eq!(
            sampled_profile.sampling_interval,
            Some(Duration::from_millis(1))
        );
        assert_eq!(sampled_profile.profile_block_executions, 0);
        assert!(
            sampled_profile
                .sites
                .iter()
                .all(|site| site.counters == SiteCounters::default())
        );

        let generic_source = b"++[.-]";
        let generic_map = test_profile_map(
            generic_source,
            vec![ProfileRange {
                start: 0,
                end: bf_identity(generic_source).instruction_count,
                site: ProfileSiteId(1),
            }],
        );
        let generic = run_with_options(
            generic_source,
            b"",
            RunOptions {
                profile: Some(ProfileOptions {
                    map: generic_map,
                    mode: ProfileMode::Exact,
                }),
                ..RunOptions::default()
            },
        )
        .unwrap()
        .profile
        .unwrap();
        assert!(generic.clock_reads > generic.profile_block_executions * 2);
    }

    #[test]
    fn sparse_site_ids_use_dense_runtime_storage() {
        let source = b"+.";
        let sparse = ProfileSiteId(u32::MAX - 1);
        let mut map = test_profile_map(
            source,
            vec![ProfileRange {
                start: 0,
                end: 2,
                site: ProfileSiteId(1),
            }],
        );
        map.sites.truncate(1);
        map.sites.push(ProfileSite {
            id: sparse,
            parent: Some(ProfileSiteId(0)),
            kind: "test".into(),
            stable_key: "test.sparse".into(),
            label: "sparse".into(),
            source: None,
            attributes: BTreeMap::new(),
        });
        map.ranges[0].site = sparse;
        let result = run_with_options(
            source,
            b"",
            RunOptions {
                profile: Some(ProfileOptions {
                    map,
                    mode: ProfileMode::Counters,
                }),
                ..RunOptions::default()
            },
        )
        .unwrap();
        assert_eq!(result.output, vec![1]);
        assert_eq!(result.profile.unwrap().sites[1].site, sparse);
    }

    #[test]
    fn optimized_profile_counter_totals_match_run_stats() {
        for source in [
            b"+++++[-].".as_slice(),
            b"+>+>+<<[>].".as_slice(),
            b"++++[->>>++>+<<<<]>>>.>.".as_slice(),
            b"++[>++[>+<-]<-]>>.".as_slice(),
        ] {
            let map = test_profile_map(
                source,
                vec![ProfileRange {
                    start: 0,
                    end: bf_identity(source).instruction_count,
                    site: ProfileSiteId(1),
                }],
            );
            let result = run_with_options(
                source,
                b"",
                RunOptions {
                    profile: Some(ProfileOptions {
                        map,
                        mode: ProfileMode::Counters,
                    }),
                    ..RunOptions::default()
                },
            )
            .unwrap();
            let sites = &result.profile.as_ref().unwrap().sites;
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.raw_bf_instructions)
                    .sum::<u64>(),
                result.stats.executed_instructions
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.rle_instructions)
                    .sum::<u64>(),
                result.stats.executed_rle_instructions
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.fast_operations)
                    .sum::<u64>(),
                result.stats.optimization.executed_native_operations
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.rle_operations)
                    .sum::<u64>(),
                result.stats.optimization.rle_operations
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.clear_loops)
                    .sum::<u64>(),
                result.stats.optimization.clear_loops
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.scan_loops)
                    .sum::<u64>(),
                result.stats.optimization.scan_loops
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.scan_steps)
                    .sum::<u64>(),
                result.stats.optimization.scan_steps
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.transfer_loops)
                    .sum::<u64>(),
                result.stats.optimization.transfer_loops
            );
            assert_eq!(
                sites
                    .iter()
                    .map(|site| site.counters.transfer_iterations)
                    .sum::<u64>(),
                result.stats.optimization.transfer_iterations
            );
        }
    }

    #[test]
    fn rejects_profile_map_for_another_artifact() {
        let source = b"+.";
        let map = test_profile_map(
            source,
            vec![ProfileRange {
                start: 0,
                end: 2,
                site: ProfileSiteId(1),
            }],
        );
        assert!(matches!(
            run_with_options(
                b"++.",
                b"",
                RunOptions {
                    profile: Some(ProfileOptions {
                        map,
                        mode: ProfileMode::Counters,
                    }),
                    ..RunOptions::default()
                }
            ),
            Err(Error::ProfileArtifact(_))
        ));
    }
}
