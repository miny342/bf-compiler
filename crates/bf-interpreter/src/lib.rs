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
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub unbounded_tape: bool,
    pub collect_stats: bool,
    pub collect_timings: bool,
    pub profile: Option<ProfileOptions>,
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
    UnmatchedOpeningBracket { offset: usize },
    UnmatchedClosingBracket { offset: usize },
    TapeUnderflow { instruction_offset: usize },
    TapeOverflow { instruction_offset: usize },
    ProfileArtifact(String),
    InvalidUtf8Output,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Self::InvalidUtf8Output => write!(f, "program output is not valid UTF-8"),
        }
    }
}

impl StdError for Error {}

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

#[derive(Debug, Clone, Copy)]
struct Instruction {
    op: Op,
    source_offset: usize,
    site: ResolvedProfileSite,
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
    let (optimized, parse_elapsed, build_elapsed) = if let Some(profile) = &options.profile {
        let instructions = parse_with_profile(source, Some(&profile.map))?;
        let parse_elapsed = parse_started.map_or(Duration::ZERO, |started| started.elapsed());
        let build_started = options.collect_timings.then(Instant::now);
        let optimized = optimize(&instructions, Some(&profile.map));
        let build_elapsed = build_started.map_or(Duration::ZERO, |started| started.elapsed());
        (optimized, parse_elapsed, build_elapsed)
    } else {
        // Generated self-host programs contain very long pointer runs. Building one
        // 40-byte `Instruction` per BF byte before immediately coalescing those runs
        // multiplies a large source into tens of gigabytes. Build the unprofiled fast
        // representation directly instead.
        let optimized = parse_optimized_unprofiled(source)?;
        let parse_elapsed = parse_started.map_or(Duration::ZERO, |started| started.elapsed());
        (optimized, parse_elapsed, Duration::ZERO)
    };
    let execute_started = (options.collect_timings || options.profile.is_some()).then(Instant::now);
    let mut result = execute(
        &optimized,
        input,
        options.unbounded_tape,
        options.profile.as_ref(),
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
    parse_with_profile(source, None)
}

fn parse_with_profile(
    source: &[u8],
    profile_map: Option<&ProfileMap>,
) -> Result<Vec<Instruction>, Error> {
    let mut instructions: Vec<Instruction> = Vec::new();
    let mut openings = Vec::new();
    let mut range_index = 0_usize;
    let range_slots = profile_map.map(|map| {
        map.ranges
            .iter()
            .map(|range| {
                map.sites
                    .iter()
                    .position(|site| site.id == range.site)
                    .expect("validated profile range references a known site")
            })
            .collect::<Vec<_>>()
    });

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
        let ordinal = instructions.len() as u64;
        let site = profile_map
            .map(|map| {
                while map.ranges[range_index].end <= ordinal {
                    range_index += 1;
                }
                ResolvedProfileSite {
                    id: map.ranges[range_index].site,
                    slot: range_slots.as_ref().unwrap()[range_index],
                }
            })
            .unwrap_or(ResolvedProfileSite::ROOT);
        instructions.push(Instruction {
            op,
            source_offset,
            site,
        });
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
            Self::Move { site, .. }
            | Self::Add { site, .. }
            | Self::Input { site, .. }
            | Self::Output { site, .. }
            | Self::Loop { site, .. } => *site,
        }
    }

    const fn mixed_provenance(&self) -> bool {
        match self {
            Self::Move { mixed, .. }
            | Self::Add { mixed, .. }
            | Self::Input { mixed, .. }
            | Self::Output { mixed, .. }
            | Self::Loop { mixed, .. } => *mixed,
        }
    }
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
}

#[derive(Debug, Clone, Default)]
struct SourceOffsets {
    ranges: Vec<SourceOffsetRange>,
    len: usize,
}

impl SourceOffsets {
    fn push(&mut self, offset: usize) {
        if let Some(last) = self.ranges.last_mut()
            && last.start.checked_add(last.len) == Some(offset)
        {
            last.len += 1;
        } else {
            self.ranges.push(SourceOffsetRange {
                start: offset,
                len: 1,
            });
        }
        self.len += 1;
    }

    const fn len(&self) -> usize {
        self.len
    }

    fn get(&self, mut index: usize) -> usize {
        assert!(index < self.len);
        for range in &self.ranges {
            if index < range.len {
                return range.start + index;
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
}

impl FastBlock {
    fn root() -> Self {
        Self {
            instructions: Vec::new(),
            last_rle: None,
            opening_offset: None,
        }
    }

    fn nested(opening_offset: usize) -> Self {
        Self {
            instructions: Vec::new(),
            last_rle: None,
            opening_offset: Some(opening_offset),
        }
    }

    fn push_move(&mut self, op: RleOp, source_offset: usize) {
        let amount = if op == RleOp::Right { 1 } else { -1 };
        if self.last_rle == Some(op)
            && let Some(FastInstruction::Move {
                amount: current,
                source_offsets,
                ..
            }) = self.instructions.last_mut()
        {
            *current += amount;
            source_offsets.push(source_offset);
        } else {
            let mut source_offsets = SourceOffsets::default();
            source_offsets.push(source_offset);
            self.instructions.push(FastInstruction::Move {
                site: ResolvedProfileSite::ROOT,
                mixed: false,
                amount,
                source_offsets,
            });
        }
        self.last_rle = Some(op);
    }

    fn push_add(&mut self, op: RleOp) {
        let amount = if op == RleOp::Increment { 1 } else { 255 };
        if self.last_rle == Some(op)
            && let Some(FastInstruction::Add {
                amount: current,
                raw_count,
                ..
            }) = self.instructions.last_mut()
        {
            *current = current.wrapping_add(amount);
            *raw_count += 1;
        } else {
            self.instructions.push(FastInstruction::Add {
                site: ResolvedProfileSite::ROOT,
                mixed: false,
                amount,
                raw_count: 1,
            });
        }
        self.last_rle = Some(op);
    }

    fn push_non_rle(&mut self, instruction: FastInstruction) {
        self.instructions.push(instruction);
        self.last_rle = None;
    }
}

fn parse_optimized_unprofiled(source: &[u8]) -> Result<Vec<FastInstruction>, Error> {
    let mut blocks = vec![FastBlock::root()];
    for (source_offset, byte) in source.iter().copied().enumerate() {
        let current = blocks.last_mut().unwrap();
        match byte {
            b'>' => current.push_move(RleOp::Right, source_offset),
            b'<' => current.push_move(RleOp::Left, source_offset),
            b'+' => current.push_add(RleOp::Increment),
            b'-' => current.push_add(RleOp::Decrement),
            b'.' => current.push_non_rle(FastInstruction::Output {
                site: ResolvedProfileSite::ROOT,
                mixed: false,
            }),
            b',' => current.push_non_rle(FastInstruction::Input {
                site: ResolvedProfileSite::ROOT,
                mixed: false,
            }),
            b'[' => {
                current.last_rle = None;
                blocks.push(FastBlock::nested(source_offset));
            }
            b']' => {
                if blocks.len() == 1 {
                    return Err(Error::UnmatchedClosingBracket {
                        offset: source_offset,
                    });
                }
                let body = blocks.pop().unwrap().instructions;
                let optimization = recognize_fast_loop(&body);
                blocks
                    .last_mut()
                    .unwrap()
                    .push_non_rle(FastInstruction::Loop {
                        site: ResolvedProfileSite::ROOT,
                        mixed: false,
                        body,
                        optimization,
                    });
            }
            _ => {}
        }
    }

    if blocks.len() != 1 {
        return Err(Error::UnmatchedOpeningBracket {
            offset: blocks[1].opening_offset.unwrap(),
        });
    }
    Ok(blocks.pop().unwrap().instructions)
}

fn optimize(
    instructions: &[Instruction],
    profile_map: Option<&ProfileMap>,
) -> Vec<FastInstruction> {
    optimize_range(instructions, 0, instructions.len(), profile_map)
}

fn optimize_range(
    instructions: &[Instruction],
    start: usize,
    end: usize,
    profile_map: Option<&ProfileMap>,
) -> Vec<FastInstruction> {
    let mut result = Vec::new();
    let mut position = start;
    while position < end {
        match instructions[position].op {
            Op::Right | Op::Left => {
                let op = instructions[position].op;
                let mut site = instructions[position].site;
                let mut mixed = false;
                let run_start = position;
                while position < end && instructions[position].op == op {
                    if profile_map.is_some() {
                        mixed |= site.id != instructions[position].site.id;
                        site =
                            lowest_common_ancestor(profile_map, site, instructions[position].site);
                    }
                    position += 1;
                }
                let count = position - run_start;
                let magnitude = isize::try_from(count).expect("BF source fits in isize");
                let amount = if op == Op::Right {
                    magnitude
                } else {
                    -magnitude
                };
                let mut source_offsets = SourceOffsets::default();
                for instruction in &instructions[run_start..position] {
                    source_offsets.push(instruction.source_offset);
                }
                result.push(FastInstruction::Move {
                    site,
                    mixed,
                    amount,
                    source_offsets,
                });
            }
            Op::Increment | Op::Decrement => {
                let op = instructions[position].op;
                let mut site = instructions[position].site;
                let mut mixed = false;
                let run_start = position;
                while position < end && instructions[position].op == op {
                    if profile_map.is_some() {
                        mixed |= site.id != instructions[position].site.id;
                        site =
                            lowest_common_ancestor(profile_map, site, instructions[position].site);
                    }
                    position += 1;
                }
                let count = position - run_start;
                let magnitude = (count & 0xff) as u8;
                let amount = if op == Op::Increment {
                    magnitude
                } else {
                    0_u8.wrapping_sub(magnitude)
                };
                result.push(FastInstruction::Add {
                    site,
                    mixed,
                    amount,
                    raw_count: count as u64,
                });
            }
            Op::Input => {
                result.push(FastInstruction::Input {
                    site: instructions[position].site,
                    mixed: false,
                });
                position += 1;
            }
            Op::Output => {
                result.push(FastInstruction::Output {
                    site: instructions[position].site,
                    mixed: false,
                });
                position += 1;
            }
            Op::JumpIfZero(target) => {
                let closing = target - 1;
                let body = optimize_range(instructions, position + 1, closing, profile_map);
                let optimization = recognize_loop(instructions, position + 1, closing, &body);
                let mut site = instructions[position].site;
                let mut mixed = false;
                if optimization.is_some() && profile_map.is_some() {
                    // Native loop operations subsume the opening/closing brackets and all
                    // instructions in their body, so attribute them to the LCA of that
                    // complete provenance set rather than just the opening bracket.
                    for instruction in &instructions[position + 1..target] {
                        mixed |= site.id != instruction.site.id;
                        site = lowest_common_ancestor(profile_map, site, instruction.site);
                    }
                }
                result.push(FastInstruction::Loop {
                    site,
                    mixed,
                    body,
                    optimization,
                });
                position = target;
            }
            Op::JumpIfNonZero(_) => unreachable!("loop closing is consumed with its opening"),
        }
    }
    result
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

fn recognize_loop(
    instructions: &[Instruction],
    start: usize,
    end: usize,
    body: &[FastInstruction],
) -> Option<LoopOptimization> {
    if start == end {
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
    let mut previous = None;
    let mut rle_count = 0_u64;
    let mut pointer_distance = 0_u64;

    for instruction in &instructions[start..end] {
        let rle_op = instruction.op.rle_op()?;
        if previous != Some(rle_op) {
            rle_count += 1;
            previous = Some(rle_op);
        }
        match instruction.op {
            Op::Right => {
                pointer_distance += 1;
                pointer += 1;
                max_offset = max_offset.max(pointer);
            }
            Op::Left => {
                pointer_distance += 1;
                pointer -= 1;
                min_offset = min_offset.min(pointer);
            }
            Op::Increment => {
                let value = updates.entry(pointer).or_default();
                *value = value.wrapping_add(1);
            }
            Op::Decrement => {
                let value = updates.entry(pointer).or_default();
                *value = value.wrapping_sub(1);
            }
            Op::Output | Op::Input | Op::JumpIfZero(_) | Op::JumpIfNonZero(_) => return None,
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
        body_raw_count: (end - start) as u64,
        body_rle_count: rle_count,
        body_pointer_distance: pointer_distance,
    })
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
            FastInstruction::Input { .. }
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
}

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
    published_sample_site: u32,
    clock_reads: u64,
    profile_block_executions: u64,
    mixed_provenance_native_operations: u64,
}

impl<'a> Machine<'a> {
    fn new(
        input: &'a [u8],
        grow_tape: bool,
        profile_options: Option<&ProfileOptions>,
        sampled_site: Option<Arc<AtomicU32>>,
    ) -> Self {
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
            published_sample_site: INACTIVE_PROFILE_SITE,
            clock_reads: 0,
            profile_block_executions: 0,
            mixed_provenance_native_operations: 0,
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

    fn execute_block(&mut self, instructions: &[FastInstruction]) -> Result<(), Error> {
        if self.profile.is_none() {
            self.execute_block_unprofiled(instructions)
        } else {
            self.execute_block_profiled(instructions)
        }
    }

    fn add_counts_unprofiled(&mut self, raw: u64, rle: u64) {
        self.executed_instructions += raw;
        self.executed_rle_instructions += rle;
    }

    fn execute_block_unprofiled(&mut self, instructions: &[FastInstruction]) -> Result<(), Error> {
        for instruction in instructions {
            self.optimization.executed_native_operations += 1;
            match instruction {
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
                } => self.execute_loop_unprofiled(body, optimization.as_ref())?,
            }
        }
        Ok(())
    }

    fn execute_loop_unprofiled(
        &mut self,
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
                    self.optimization.scan_steps += 1;
                    self.optimization.rle_operations += 1;
                    self.add_counts_unprofiled(source_offsets.len() as u64, 1);
                    self.move_pointer(*amount, source_offsets)?;
                    self.add_counts_unprofiled(1, 1);
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
                    return self.execute_generic_loop_unprofiled(body);
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
            None => self.execute_generic_loop_unprofiled(body),
        }
    }

    fn execute_generic_loop_unprofiled(&mut self, body: &[FastInstruction]) -> Result<(), Error> {
        self.add_counts_unprofiled(1, 1);
        while self.tape[self.pointer] != 0 {
            self.execute_block_unprofiled(body)?;
            self.add_counts_unprofiled(1, 1);
        }
        Ok(())
    }

    fn execute_block_profiled(&mut self, instructions: &[FastInstruction]) -> Result<(), Error> {
        for instruction in instructions {
            let site = instruction.site();
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
                    iterations += 1;
                    self.optimization.scan_steps += 1;
                    self.optimization.rle_operations += 1;
                    self.add_counts(site, source_offsets.len() as u64, 1);
                    self.move_pointer(*amount, source_offsets)?;
                    self.record_maximum_pointer(site);
                    self.add_counts(site, 1, 1);
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
) -> Result<RunResult, Error> {
    let site_slots = profile_options.map_or(0, |options| options.map.sites.len());
    let sampling = SamplingSession::start(profile_options, site_slots);
    let sampled_site = sampling
        .as_ref()
        .map(|session| Arc::clone(&session.current_site));
    let mut machine = Machine::new(input, grow_tape, profile_options, sampled_site);
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
        let optimized = parse_optimized_unprofiled(b">>> comment >>>").unwrap();
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
        assert_eq!(
            sampled.profile.unwrap().sampling_interval,
            Some(Duration::from_millis(1))
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
