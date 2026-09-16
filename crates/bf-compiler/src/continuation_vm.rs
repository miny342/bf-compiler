//! Direct execution of validated continuation IR.
//!
//! This VM is primarily a development tool. It preserves the cell and frame
//! semantics of the Brainfuck ABI without first expanding them into a very
//! large Brainfuck program, and writes output through a bounded buffer.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::{
    Address, AggregateRegion, Continuation, ContinuationId, ContinuationProgram, FrameAggregateId,
    FrameInstruction, FunctionDescriptor, FunctionId, GlobalDescriptor, GlobalId, LogicalOffset,
    ParameterLocation, Terminator, ValueOperand, ValueType,
};

const OUTPUT_BUFFER_CELLS: usize = 64 * 1024;
const PROGRESS_CHECK_MASK: u64 = (1 << 20) - 1;

/// Configuration for direct continuation-IR execution.
#[derive(Debug, Clone, Default)]
pub struct ContinuationRunOptions {
    /// Minimum wall-clock interval between progress callbacks.
    pub progress_interval: Option<Duration>,
    /// Collect dynamic continuation-to-continuation transitions.
    pub collect_transitions: bool,
    /// Collect phase and portal request aggregates using explicit function
    /// entry boundaries.  This is deliberately opt-in because it maintains
    /// additional maps on every continuation and portal event.
    pub phase_config: Option<ContinuationPhaseConfig>,
}

/// One logical phase boundary.  The phase becomes active when an activation
/// of `function` enters its function entry continuation and is left when that
/// activation returns.  A function can have at most one boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationPhaseBoundary {
    pub phase: String,
    pub function: FunctionId,
}

/// Explicit phase/portal measurement configuration resolved against one IR
/// artifact.  The CLI performs the source-name/CIR-ID resolution and checks
/// the artifact identity before passing this to the VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationPhaseConfig {
    pub artifact_kind: String,
    pub artifact_id: String,
    pub boundaries: Vec<ContinuationPhaseBoundary>,
    pub chunk_cells: Vec<usize>,
}

/// Terminator categories used for dynamic continuation transition metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContinuationTerminatorKind {
    Goto,
    Branch,
    Call,
    Return,
    ArrayLoad,
    ArrayStore,
    AggregateLoad,
    AggregateStore,
    Halt,
    Abort,
}

impl ContinuationTerminatorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Goto => "goto",
            Self::Branch => "branch",
            Self::Call => "call",
            Self::Return => "return",
            Self::ArrayLoad => "array_load",
            Self::ArrayStore => "array_store",
            Self::AggregateLoad => "aggregate_load",
            Self::AggregateStore => "aggregate_store",
            Self::Halt => "halt",
            Self::Abort => "abort",
        }
    }

    pub const fn from_terminator(terminator: &Terminator) -> Self {
        match terminator {
            Terminator::Goto { .. } => Self::Goto,
            Terminator::Branch { .. } => Self::Branch,
            Terminator::Call { .. } => Self::Call,
            Terminator::Return { .. } => Self::Return,
            Terminator::ArrayLoad { .. } => Self::ArrayLoad,
            Terminator::ArrayStore { .. } => Self::ArrayStore,
            Terminator::AggregateLoad { .. } => Self::AggregateLoad,
            Terminator::AggregateStore { .. } => Self::AggregateStore,
            Terminator::Halt => Self::Halt,
            Terminator::Abort => Self::Abort,
        }
    }
}

/// Cumulative counters from a direct continuation-IR run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContinuationRunStats {
    pub executed_continuations: u64,
    pub executed_frame_instructions: u64,
    pub loop_iterations: u64,
    pub calls: u64,
    pub returns: u64,
    pub array_loads: u64,
    pub array_stores: u64,
    pub aggregate_loads: u64,
    pub aggregate_stores: u64,
    pub input_operations: u64,
    pub output_bytes: u64,
    pub max_call_depth: usize,
    pub aborted: bool,
    pub final_continuation: Option<ContinuationId>,
    pub final_function_stack: Vec<FunctionId>,
    continuation_counts: Vec<u64>,
    transition_counts: HashMap<(ContinuationId, ContinuationId), u64>,
    terminal_counts: HashMap<(ContinuationId, ContinuationTerminatorKind), u64>,
    transitions_collected: bool,
    phase_metrics: Option<PhaseMetrics>,
}

impl ContinuationRunStats {
    /// Continuations ordered from most to least frequently dispatched.
    pub fn hottest_continuations(&self, limit: usize) -> Vec<(ContinuationId, u64)> {
        let mut counts = self
            .continuation_counts
            .iter()
            .enumerate()
            .filter_map(|(id, &count)| {
                (count != 0)
                    .then(|| ContinuationId::new(id as u16).map(|id| (id, count)))
                    .flatten()
            })
            .collect::<Vec<_>>();
        counts.sort_unstable_by_key(|&(id, count)| (std::cmp::Reverse(count), id));
        counts.truncate(limit);
        counts
    }

    /// Continuation transitions ordered from most to least frequently.
    pub fn hottest_transitions(&self, limit: usize) -> Vec<(ContinuationId, ContinuationId, u64)> {
        let mut transitions = self
            .transition_counts
            .iter()
            .map(|(&(from, to), &count)| (from, to, count))
            .collect::<Vec<_>>();
        transitions.sort_unstable_by_key(|&(from, to, count)| (std::cmp::Reverse(count), from, to));
        transitions.truncate(limit);
        transitions
    }

    /// Return all collected dynamic transitions in deterministic order.
    pub fn transition_counts(&self) -> Vec<(ContinuationId, ContinuationId, u64)> {
        let mut transitions = self
            .transition_counts
            .iter()
            .map(|(&(from, to), &count)| (from, to, count))
            .collect::<Vec<_>>();
        transitions.sort_unstable_by_key(|&(from, to, _)| (from, to));
        transitions
    }

    /// Return all collected terminal events in deterministic order.
    pub fn terminal_counts(&self) -> Vec<(ContinuationId, ContinuationTerminatorKind, u64)> {
        let mut terminals = self
            .terminal_counts
            .iter()
            .map(|(&(from, kind), &count)| (from, kind, count))
            .collect::<Vec<_>>();
        terminals.sort_unstable_by_key(|&(from, kind, _)| (from, kind));
        terminals
    }

    pub fn continuation_count(&self, id: ContinuationId) -> u64 {
        self.continuation_counts
            .get(usize::from(id.get()))
            .copied()
            .unwrap_or(0)
    }

    pub const fn transitions_collected(&self) -> bool {
        self.transitions_collected
    }

    /// Return opt-in phase and portal aggregates as a stable JSON object.
    pub fn phase_metrics_json(&self, program: &ContinuationProgram) -> Option<serde_json::Value> {
        self.phase_metrics
            .as_ref()
            .map(|metrics| metrics.to_json(program))
    }

    /// Check dynamic in/out accounting for every continuation in the program.
    pub fn validate_transition_accounting(
        &self,
        program: &ContinuationProgram,
    ) -> Result<(), String> {
        if !self.transitions_collected {
            return Err("transition collection was disabled".into());
        }
        let mut incoming = HashMap::<ContinuationId, u64>::new();
        let mut outgoing = HashMap::<ContinuationId, u64>::new();
        for (from, to, count) in self.transition_counts() {
            *incoming.entry(to).or_default() += count;
            *outgoing.entry(from).or_default() += count;
        }
        for (from, _, count) in self.terminal_counts() {
            *outgoing.entry(from).or_default() += count;
        }
        for continuation in program.continuations() {
            let id = continuation.id();
            let executed = self.continuation_count(id);
            let expected_incoming = incoming.get(&id).copied().unwrap_or(0)
                + u64::from(
                    program
                        .function(program.main())
                        .is_some_and(|function| function.entry() == id),
                );
            let actual_outgoing = outgoing.get(&id).copied().unwrap_or(0);
            if expected_incoming != executed || actual_outgoing != executed {
                return Err(format!(
                    "continuation {} accounting mismatch: executions={} incoming={} outgoing={}",
                    id.get(),
                    executed,
                    expected_incoming,
                    actual_outgoing
                ));
            }
        }
        let transition_total = self.transition_counts.values().copied().sum::<u64>();
        if transition_total + 1 != self.executed_continuations {
            return Err(format!(
                "transition total mismatch: transitions={} executions={}",
                transition_total, self.executed_continuations
            ));
        }
        Ok(())
    }
}

/// A cheap snapshot passed to periodic progress observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationRunProgress {
    pub elapsed: Duration,
    pub current_continuation: ContinuationId,
    pub executed_continuations: u64,
    pub executed_frame_instructions: u64,
    pub loop_iterations: u64,
    pub calls: u64,
    pub output_bytes: u64,
    pub call_depth: usize,
    pub max_call_depth: usize,
}

/// A runtime failure while directly executing continuation IR.
#[derive(Debug)]
pub enum ContinuationVmError {
    Io(io::Error),
    Runtime(String),
}

impl fmt::Display for ContinuationVmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Runtime(message) => f.write_str(message),
        }
    }
}

impl Error for ContinuationVmError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Runtime(_) => None,
        }
    }
}

impl From<io::Error> for ContinuationVmError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Execute validated continuation IR with streaming input and output.
pub fn run_continuations_with_io<R, W, F>(
    program: &ContinuationProgram,
    input: &mut R,
    output: &mut W,
    options: ContinuationRunOptions,
    mut progress: F,
) -> Result<ContinuationRunStats, ContinuationVmError>
where
    R: Read,
    W: Write,
    F: FnMut(ContinuationRunProgress),
{
    let mut machine = Machine::new(program, input, output, options)?;
    machine.run(&mut progress)?;
    machine.flush_output()?;
    Ok(machine.stats)
}

#[derive(Debug)]
enum RuntimeValue {
    Cell(u8),
    Aggregate(Vec<u8>),
}

#[derive(Debug)]
struct Frame {
    function: FunctionId,
    activation: u64,
    phase: Option<usize>,
    slots: Vec<u8>,
    aggregates: Vec<Option<Vec<u8>>>,
    outbox: Vec<u8>,
    abi_value: u8,
    return_to: Option<ContinuationId>,
}

#[derive(Debug)]
enum OwnedOperand {
    Cell(u8),
    Aggregate(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum PortalOperation {
    ArrayLoad,
    ArrayStore,
    AggregateLoad,
    AggregateStore,
}

impl PortalOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ArrayLoad => "array_load",
            Self::ArrayStore => "array_store",
            Self::AggregateLoad => "aggregate_load",
            Self::AggregateStore => "aggregate_store",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum PortalRegion {
    Frame {
        function: FunctionId,
        activation: u64,
        aggregate: FrameAggregateId,
    },
    Global {
        global: GlobalId,
    },
    Outbox {
        function: FunctionId,
        activation: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PortalRequestKey {
    phase: usize,
    continuation: ContinuationId,
    function: FunctionId,
    region: PortalRegion,
    operation: PortalOperation,
    cells: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviousPortalRequest {
    phase: usize,
    region: PortalRegion,
    offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PortalRequestAggregate {
    requests: u64,
    offset_histogram: HashMap<usize, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PortalPhaseAggregate {
    requests: u64,
    adjacent_pairs: u64,
    adjacent_same_region: u64,
    offset_delta_histogram: HashMap<i64, u64>,
    chunk_requests: HashMap<usize, u64>,
    chunk_unique: HashMap<usize, HashMap<(PortalRegion, usize), u64>>,
    chunk_revisits: HashMap<usize, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhaseMetrics {
    config: ContinuationPhaseConfig,
    phase_names: Vec<String>,
    function_phases: HashMap<FunctionId, usize>,
    continuation_counts: HashMap<(usize, ContinuationId), u64>,
    transition_counts: HashMap<(usize, ContinuationId, ContinuationId), u64>,
    terminal_counts: HashMap<(usize, ContinuationId, ContinuationTerminatorKind), u64>,
    portal_requests: HashMap<PortalRequestKey, PortalRequestAggregate>,
    portal_phases: Vec<PortalPhaseAggregate>,
    previous_portal: Option<PreviousPortalRequest>,
    last_phase: Option<usize>,
}

impl PhaseMetrics {
    fn new(program: &ContinuationProgram, config: ContinuationPhaseConfig) -> Result<Self, String> {
        if config.chunk_cells.is_empty() || config.chunk_cells.iter().any(|&cells| cells != 16) {
            return Err("phase config chunk_cells must contain only 16".into());
        }
        let mut phase_names = vec!["unknown".to_owned()];
        let mut function_phases = HashMap::new();
        let mut entry_phases = HashMap::<ContinuationId, &str>::new();
        for boundary in &config.boundaries {
            let function = program.function(boundary.function).ok_or_else(|| {
                format!(
                    "phase boundary references unknown function {}",
                    boundary.function.index()
                )
            })?;
            if boundary.phase.is_empty() || boundary.phase == "unknown" {
                return Err("phase boundary name must be nonempty and not 'unknown'".into());
            }
            if function_phases.contains_key(&boundary.function) {
                return Err(format!(
                    "function {} has more than one phase boundary",
                    boundary.function.index()
                ));
            }
            if let Some(previous) = entry_phases.insert(function.entry(), &boundary.phase)
                && previous != boundary.phase
            {
                return Err(format!(
                    "continuation {} is used by multiple phase entries",
                    function.entry().get()
                ));
            }
            let phase = match phase_names.iter().position(|name| name == &boundary.phase) {
                Some(phase) => phase,
                None => {
                    phase_names.push(boundary.phase.clone());
                    phase_names.len() - 1
                }
            };
            function_phases.insert(boundary.function, phase);
        }
        let portal_phases = (0..phase_names.len())
            .map(|_| PortalPhaseAggregate::default())
            .collect();
        Ok(Self {
            config,
            phase_names,
            function_phases,
            continuation_counts: HashMap::new(),
            transition_counts: HashMap::new(),
            terminal_counts: HashMap::new(),
            portal_requests: HashMap::new(),
            portal_phases,
            previous_portal: None,
            last_phase: None,
        })
    }

    fn phase_for_function(&self, function: FunctionId) -> Option<usize> {
        self.function_phases.get(&function).copied()
    }

    fn record_continuation(&mut self, phase: usize, continuation: ContinuationId) {
        if self.last_phase != Some(phase) {
            self.previous_portal = None;
            self.last_phase = Some(phase);
        }
        *self
            .continuation_counts
            .entry((phase, continuation))
            .or_default() += 1;
    }

    fn record_transition(&mut self, phase: usize, from: ContinuationId, to: ContinuationId) {
        *self.transition_counts.entry((phase, from, to)).or_default() += 1;
    }

    fn record_terminal(
        &mut self,
        phase: usize,
        from: ContinuationId,
        kind: ContinuationTerminatorKind,
    ) {
        *self.terminal_counts.entry((phase, from, kind)).or_default() += 1;
    }

    fn record_portal(&mut self, key: PortalRequestKey, offset: usize) {
        let PortalRequestKey { phase, region, .. } = key;
        let aggregate = self.portal_requests.entry(key).or_default();
        aggregate.requests += 1;
        *aggregate.offset_histogram.entry(offset).or_default() += 1;

        let phase_aggregate = &mut self.portal_phases[phase];
        phase_aggregate.requests += 1;
        if let Some(previous) = self.previous_portal
            && previous.phase == phase
        {
            phase_aggregate.adjacent_pairs += 1;
            if previous.region == region {
                phase_aggregate.adjacent_same_region += 1;
                let delta = offset as i64 - previous.offset as i64;
                *phase_aggregate
                    .offset_delta_histogram
                    .entry(delta)
                    .or_default() += 1;
            }
        }
        for &chunk_cells in &self.config.chunk_cells {
            let chunk = offset / chunk_cells;
            *phase_aggregate
                .chunk_requests
                .entry(chunk_cells)
                .or_default() += 1;
            let seen = phase_aggregate.chunk_unique.entry(chunk_cells).or_default();
            if seen.insert((region, chunk), 1).is_some() {
                *phase_aggregate
                    .chunk_revisits
                    .entry(chunk_cells)
                    .or_default() += 1;
            }
        }
        self.previous_portal = Some(PreviousPortalRequest {
            phase,
            region,
            offset,
        });
    }

    fn to_json(&self, program: &ContinuationProgram) -> serde_json::Value {
        let phase_name = |phase: usize| {
            self.phase_names
                .get(phase)
                .map(String::as_str)
                .unwrap_or("unknown")
        };
        let mut continuations = self
            .continuation_counts
            .iter()
            .map(|(&(phase, continuation), &count)| {
                let function = program.continuation(continuation).map(|c| c.function());
                let function_name = function
                    .and_then(|id| program.function(id))
                    .and_then(|function| function.name());
                json!({
                    "phase": phase_name(phase),
                    "continuation": continuation.get(),
                    "function_id": function.map(|id| id.index()),
                    "function_name": function_name,
                    "executions": count,
                })
            })
            .collect::<Vec<_>>();
        continuations.sort_by_key(|row| {
            (
                row["phase"].as_str().unwrap_or_default().to_owned(),
                row["continuation"].as_u64().unwrap_or_default(),
            )
        });

        let mut transitions = self
            .transition_counts
            .iter()
            .map(|(&(phase, from, to), &count)| {
                let kind = program.continuation(from).map(|continuation| {
                    ContinuationTerminatorKind::from_terminator(continuation.terminator())
                });
                json!({
                    "phase": phase_name(phase),
                    "from": from.get(),
                    "to": to.get(),
                    "kind": kind.map_or("unknown", ContinuationTerminatorKind::as_str),
                    "count": count,
                })
            })
            .collect::<Vec<_>>();
        transitions.sort_by_key(|row| {
            (
                row["phase"].as_str().unwrap_or_default().to_owned(),
                row["from"].as_u64().unwrap_or_default(),
                row["to"].as_u64().unwrap_or_default(),
            )
        });

        let mut terminals = self
            .terminal_counts
            .iter()
            .map(|(&(phase, from, kind), &count)| {
                json!({
                    "phase": phase_name(phase),
                    "from": from.get(),
                    "kind": kind.as_str(),
                    "count": count,
                })
            })
            .collect::<Vec<_>>();
        terminals.sort_by_key(|row| {
            (
                row["phase"].as_str().unwrap_or_default().to_owned(),
                row["from"].as_u64().unwrap_or_default(),
                row["kind"].as_str().unwrap_or_default().to_owned(),
            )
        });

        let mut requests = self
            .portal_requests
            .iter()
            .map(|(key, aggregate)| {
                let function_name = program
                    .function(key.function)
                    .and_then(|function| function.name());
                let region = portal_region_json(key.region);
                let offset_histogram = aggregate
                    .offset_histogram
                    .iter()
                    .map(|(&offset, &count)| (offset.to_string(), json!(count)))
                    .collect::<serde_json::Map<_, _>>();
                json!({
                    "phase": phase_name(key.phase),
                    "continuation": key.continuation.get(),
                    "function_id": key.function.index(),
                    "function_name": function_name,
                    "region": region,
                    "operation": key.operation.as_str(),
                    "cells": key.cells,
                    "requests": aggregate.requests,
                    "offset_histogram": offset_histogram,
                })
            })
            .collect::<Vec<_>>();
        requests.sort_by_key(|row| {
            (
                row["phase"].as_str().unwrap_or_default().to_owned(),
                row["function_id"].as_u64().unwrap_or_default(),
                row["continuation"].as_u64().unwrap_or_default(),
                row["operation"].as_str().unwrap_or_default().to_owned(),
            )
        });

        let total_requests = self
            .portal_requests
            .values()
            .map(|aggregate| aggregate.requests)
            .sum::<u64>();
        let mut requests_by_operation = HashMap::<&'static str, u64>::new();
        for (key, aggregate) in &self.portal_requests {
            *requests_by_operation
                .entry(key.operation.as_str())
                .or_default() += aggregate.requests;
        }
        let requests_by_operation = requests_by_operation
            .into_iter()
            .map(|(operation, count)| (operation.to_owned(), json!(count)))
            .collect::<serde_json::Map<_, _>>();

        let phase_portal = self
            .phase_names
            .iter()
            .enumerate()
            .map(|(phase, name)| {
                let aggregate = &self.portal_phases[phase];
                let same_region_rate = if aggregate.adjacent_pairs == 0 {
                    0.0
                } else {
                    aggregate.adjacent_same_region as f64 / aggregate.adjacent_pairs as f64
                };
                let offset_delta_histogram = aggregate
                    .offset_delta_histogram
                    .iter()
                    .map(|(&delta, &count)| (delta.to_string(), json!(count)))
                    .collect::<serde_json::Map<_, _>>();
                let chunks = self
                    .config
                    .chunk_cells
                    .iter()
                    .map(|&cells| {
                        let requests = aggregate.chunk_requests.get(&cells).copied().unwrap_or(0);
                        let revisits = aggregate.chunk_revisits.get(&cells).copied().unwrap_or(0);
                        let unique = aggregate
                            .chunk_unique
                            .get(&cells)
                            .map_or(0, HashMap::len);
                        (
                            cells.to_string(),
                            json!({
                                "requests": requests,
                                "unique_start_chunks": unique,
                                "revisits": revisits,
                                "revisit_rate": if requests == 0 { 0.0 } else { revisits as f64 / requests as f64 },
                                "revisit_definition": "Historical revisit: this (region, start offset / D) was seen earlier in this phase; it is not a predecessor-distance or adjacency metric.",
                            }),
                        )
                    })
                    .collect::<serde_json::Map<_, _>>();
                json!({
                    "phase": name,
                    "requests": aggregate.requests,
                    "adjacent_pairs": aggregate.adjacent_pairs,
                    "adjacent_same_region": aggregate.adjacent_same_region,
                    "adjacent_same_region_rate": same_region_rate,
                    "offset_delta_histogram": offset_delta_histogram,
                    "start_chunk_revisits": chunks,
                })
            })
            .collect::<Vec<_>>();

        let boundaries = self
            .config
            .boundaries
            .iter()
            .map(|boundary| {
                let function = program.function(boundary.function);
                json!({
                    "phase": boundary.phase,
                    "function_id": boundary.function.index(),
                    "function_name": function.and_then(FunctionDescriptor::name),
                    "entry": function.map(FunctionDescriptor::entry).map(|id| id.get()),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "format": "bfc-continuation-ir-phase-portal-v1",
            "phase_boundaries": boundaries,
            "phase_names": self.phase_names,
            "continuations": continuations,
            "transitions": transitions,
            "terminals": terminals,
            "portal": {
                "total_requests": total_requests,
                "by_operation": requests_by_operation,
                "requests": requests,
                "by_phase": phase_portal,
                "definitions": {
                    "adjacent_same_region": "An adjacent pair in the portal request sequence with the same phase and region identity. Phase changes break adjacency.",
                    "offset_delta": "Offset of the current request minus the immediately preceding request when phase and region match.",
                    "start_chunk": "The logical request start offset divided by D; multi-cell payload accesses are not expanded into per-cell events.",
                    "start_chunk_revisit": "A historical revisit of the same (region, start offset / D) within the phase; it is not evidence that requests are adjacent or safely batchable.",
                    "phase_change": "A change in effective phase label at continuation dispatch clears portal adjacency. A nested call and return with the same phase label do not clear it; different labels, including unknown, do.",
                    "frame_region_identity": "Function ID plus activation ID plus frame aggregate ID; recursive activations are distinct.",
                    "unknown_phase": "Requests and continuation events without an active configured function activation are attributed to unknown.",
                    "terminal_attribution": "Halt and abort are attributed to the active phase of the terminating continuation; no successor transition is emitted.",
                },
            },
        })
    }
}

fn portal_region_json(region: PortalRegion) -> serde_json::Value {
    match region {
        PortalRegion::Frame {
            function,
            activation,
            aggregate,
        } => json!({
            "kind": "frame",
            "function_id": function.index(),
            "activation_id": activation,
            "aggregate_id": aggregate.index(),
        }),
        PortalRegion::Global { global } => json!({
            "kind": "global",
            "global_id": global.index(),
        }),
        PortalRegion::Outbox {
            function,
            activation,
        } => json!({
            "kind": "outbox",
            "function_id": function.index(),
            "activation_id": activation,
        }),
    }
}

struct Machine<'a, R, W> {
    input: &'a mut R,
    output: &'a mut W,
    output_buffer: Vec<u8>,
    globals: Vec<Option<RuntimeValue>>,
    functions: Vec<Option<&'a FunctionDescriptor>>,
    continuations: Vec<Option<&'a Continuation>>,
    stack: Vec<Frame>,
    current: ContinuationId,
    stats: ContinuationRunStats,
    started: Instant,
    last_progress: Instant,
    progress_interval: Option<Duration>,
    collect_transitions: bool,
    next_activation: u64,
}

impl<'a, R: Read, W: Write> Machine<'a, R, W> {
    fn new(
        program: &'a ContinuationProgram,
        input: &'a mut R,
        output: &'a mut W,
        options: ContinuationRunOptions,
    ) -> Result<Self, ContinuationVmError> {
        let functions_len = program
            .functions()
            .iter()
            .map(|function| function.id().index())
            .max()
            .map_or(0, |index| index + 1);
        let mut functions = vec![None; functions_len];
        for function in program.functions() {
            functions[function.id().index()] = Some(function);
        }

        let continuations_len = program
            .continuations()
            .iter()
            .map(|continuation| usize::from(continuation.id().get()))
            .max()
            .map_or(1, |index| index + 1);
        let mut continuations = vec![None; continuations_len];
        for continuation in program.continuations() {
            continuations[usize::from(continuation.id().get())] = Some(continuation);
        }

        let globals_len = program
            .globals()
            .iter()
            .map(|global| global.id().index())
            .max()
            .map_or(0, |index| index + 1);
        let mut globals = (0..globals_len).map(|_| None).collect::<Vec<_>>();
        for global in program.globals() {
            globals[global.id().index()] = Some(initial_global(*global));
        }

        let main = function_at(&functions, program.main())?;
        let phase_metrics = options
            .phase_config
            .map(|config| PhaseMetrics::new(program, config))
            .transpose()
            .map_err(runtime)?;
        let root_phase = phase_metrics
            .as_ref()
            .and_then(|metrics| metrics.phase_for_function(main.id()));
        let current = main.entry();
        let stack = vec![new_frame(main, None, 0, root_phase)];
        let started = Instant::now();
        Ok(Self {
            input,
            output,
            output_buffer: Vec::with_capacity(OUTPUT_BUFFER_CELLS),
            globals,
            functions,
            continuations,
            stack,
            current,
            stats: ContinuationRunStats {
                max_call_depth: 1,
                continuation_counts: vec![0; continuations_len],
                transition_counts: HashMap::new(),
                terminal_counts: HashMap::new(),
                transitions_collected: options.collect_transitions,
                phase_metrics,
                ..ContinuationRunStats::default()
            },
            started,
            last_progress: started,
            progress_interval: options.progress_interval,
            collect_transitions: options.collect_transitions,
            next_activation: 1,
        })
    }

    fn run<F>(&mut self, progress: &mut F) -> Result<(), ContinuationVmError>
    where
        F: FnMut(ContinuationRunProgress),
    {
        loop {
            let continuation = self.continuation(self.current)?;
            let function = self.current_frame()?.function;
            if continuation.function() != function {
                return Err(runtime(format!(
                    "continuation {} belongs to function {}, but frame belongs to {}",
                    continuation.id().get(),
                    continuation.function().index(),
                    function.index()
                )));
            }
            self.stats.executed_continuations += 1;
            self.stats.continuation_counts[usize::from(self.current.get())] += 1;
            if let Some(phase) = self.current_phase()
                && let Some(metrics) = self.stats.phase_metrics.as_mut()
            {
                metrics.record_continuation(phase, self.current);
            }
            self.maybe_progress(progress, false);

            for instruction in continuation.body() {
                self.execute_instruction(instruction, progress)?;
            }
            if !self.execute_terminator(continuation.terminator())? {
                return Ok(());
            }
        }
    }

    fn execute_instruction<F>(
        &mut self,
        instruction: &FrameInstruction,
        progress: &mut F,
    ) -> Result<(), ContinuationVmError>
    where
        F: FnMut(ContinuationRunProgress),
    {
        self.stats.executed_frame_instructions += 1;
        self.maybe_progress(progress, false);
        match instruction {
            FrameInstruction::SubWithBorrow {
                left,
                right,
                difference,
                borrow,
                true_value,
                false_value,
            } => {
                let a = self.read_address(*left)?;
                let b = self.read_address(*right)?;
                self.write_address(*left, 0)?;
                self.write_address(*right, 0)?;
                self.write_address(*difference, a.wrapping_sub(b))?;
                self.write_address(*borrow, if a < b { *true_value } else { *false_value })?;
            }
            FrameInstruction::Compare {
                left,
                right,
                dst,
                true_value,
                false_value,
            } => {
                let less = self.read_address(*left)? < self.read_address(*right)?;
                self.write_address(*left, 0)?;
                self.write_address(*right, 0)?;
                self.write_address(*dst, if less { *true_value } else { *false_value })?;
            }
            FrameInstruction::Set { dst, value } => self.write_address(*dst, *value)?,
            FrameInstruction::AddConst { dst, value } => {
                let result = self.read_address(*dst)?.wrapping_add(*value);
                self.write_address(*dst, result)?;
            }
            FrameInstruction::Copy { src, dst } => {
                let value = self.read_address(*src)?;
                self.write_address(*dst, value)?;
            }
            FrameInstruction::Transfer { src, targets } => {
                let value = self.read_address(*src)?;
                for target in targets {
                    let result = self
                        .read_address(target.dst)?
                        .wrapping_add(value.wrapping_mul(target.factor));
                    self.write_address(target.dst, result)?;
                }
                self.write_address(*src, 0)?;
            }
            FrameInstruction::AggregateCopy { src, dst, cells } => {
                let value = self.read_region(*src, 0, *cells)?;
                self.write_region(*dst, 0, &value)?;
            }
            FrameInstruction::Input { dst } => {
                let mut byte = [0];
                let value = match self.input.read(&mut byte)? {
                    0 => 0,
                    _ => byte[0],
                };
                self.stats.input_operations += 1;
                self.write_address(*dst, value)?;
            }
            FrameInstruction::Output { src } => {
                let value = self.read_address(*src)?;
                self.output_buffer.push(value);
                self.stats.output_bytes += 1;
                if self.output_buffer.len() == OUTPUT_BUFFER_CELLS {
                    self.flush_output()?;
                }
            }
            FrameInstruction::Loop { condition, body } => {
                while self.read_address(*condition)? != 0 {
                    self.stats.loop_iterations += 1;
                    for instruction in body {
                        self.execute_instruction(instruction, progress)?;
                    }
                }
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                let condition_value = self.read_address(*condition)?;
                self.write_address(*condition, 0)?;
                let body = if condition_value != 0 {
                    then_body
                } else {
                    else_body
                };
                for instruction in body {
                    self.execute_instruction(instruction, progress)?;
                }
            }
        }
        Ok(())
    }

    /// Return true to continue dispatching.
    fn execute_terminator(&mut self, terminator: &Terminator) -> Result<bool, ContinuationVmError> {
        let from = self.current;
        let phase = self.current_phase();
        match terminator {
            Terminator::Goto { target } => self.current = *target,
            Terminator::Branch {
                condition,
                then_target,
                else_target,
            } => {
                let condition_value = self.read_address(*condition)?;
                self.write_address(*condition, 0)?;
                self.current = if condition_value != 0 {
                    *then_target
                } else {
                    *else_target
                };
            }
            Terminator::Call {
                callee,
                arguments,
                return_to,
            } => {
                let arguments = arguments
                    .iter()
                    .map(|operand| self.read_operand(*operand))
                    .collect::<Result<Vec<_>, _>>()?;
                let function = self.function(*callee)?;
                let mut frame = new_frame(function, Some(*return_to), 0, None);
                for (argument, parameter) in
                    arguments.into_iter().zip(function.parameter_locations())
                {
                    match (argument, parameter) {
                        (OwnedOperand::Cell(value), ParameterLocation::Cell(slot)) => {
                            frame.slots[slot.index()] = value;
                        }
                        (
                            OwnedOperand::Cell(value),
                            ParameterLocation::AggregateElement { aggregate, index },
                        ) => {
                            frame_aggregate_mut(&mut frame, *aggregate)?[*index] = value;
                        }
                        (
                            OwnedOperand::Aggregate(value),
                            ParameterLocation::Array(id) | ParameterLocation::Aggregate(id),
                        ) => {
                            *frame_aggregate_mut(&mut frame, *id)? = value;
                        }
                        _ => unreachable!("validated call operand types match parameters"),
                    }
                }
                self.current = function.entry();
                let phase = self
                    .stats
                    .phase_metrics
                    .as_ref()
                    .and_then(|metrics| metrics.phase_for_function(function.id()));
                frame.activation = self.next_activation;
                frame.phase = phase;
                self.next_activation += 1;
                self.stack.push(frame);
                self.stats.calls += 1;
                self.stats.max_call_depth = self.stats.max_call_depth.max(self.stack.len());
            }
            Terminator::Return { value } => {
                let value = value
                    .map(|operand| self.read_operand(operand))
                    .transpose()?;
                let frame = self
                    .stack
                    .pop()
                    .ok_or_else(|| runtime("return with an empty call stack"))?;
                let return_to = frame
                    .return_to
                    .ok_or_else(|| runtime("main function attempted to return"))?;
                let caller = self.current_frame_mut()?;
                match value {
                    Some(OwnedOperand::Cell(value)) => caller.abi_value = value,
                    Some(OwnedOperand::Aggregate(value)) => {
                        if caller.outbox.len() < value.len() {
                            return Err(runtime("aggregate return exceeds caller outbox"));
                        }
                        caller.outbox[..value.len()].copy_from_slice(&value);
                        caller.abi_value = 0;
                    }
                    None => caller.abi_value = 0,
                }
                self.current = return_to;
                self.stats.returns += 1;
            }
            Terminator::ArrayLoad {
                array,
                index,
                destination,
                return_to,
            } => {
                let index = usize::from(self.read_address(*index)?);
                self.record_portal(PortalOperation::ArrayLoad, *array, index, 1);
                let value = self.read_region_cell(*array, index)?;
                self.write_address(*destination, value)?;
                self.current = *return_to;
                self.stats.array_loads += 1;
            }
            Terminator::ArrayStore {
                array,
                index,
                value,
                return_to,
            } => {
                let index = usize::from(self.read_address(*index)?);
                let value = self.read_address(*value)?;
                self.record_portal(PortalOperation::ArrayStore, *array, index, 1);
                self.write_region_cell(*array, index, value)?;
                self.current = *return_to;
                self.stats.array_stores += 1;
            }
            Terminator::AggregateLoad {
                source,
                offset,
                destination,
                cells,
                return_to,
            } => {
                let start = self.read_logical_offset(*offset)?;
                self.record_portal(PortalOperation::AggregateLoad, *source, start, *cells);
                let value = self.read_region(*source, start, *cells)?;
                self.write_operand(*destination, &value)?;
                self.advance_logical_offset(*offset, cells.saturating_sub(1))?;
                self.current = *return_to;
                self.stats.aggregate_loads += 1;
            }
            Terminator::AggregateStore {
                destination,
                offset,
                source,
                cells,
                return_to,
            } => {
                let start = self.read_logical_offset(*offset)?;
                let value = self.read_operand_cells(*source, *cells)?;
                self.record_portal(PortalOperation::AggregateStore, *destination, start, *cells);
                self.write_region(*destination, start, &value)?;
                self.advance_logical_offset(*offset, cells.saturating_sub(1))?;
                self.current = *return_to;
                self.stats.aggregate_stores += 1;
            }
            Terminator::Abort => {
                self.stats.aborted = true;
                if self.collect_transitions {
                    self.record_terminal(from, ContinuationTerminatorKind::Abort);
                }
                self.record_phase_terminal(phase, from, ContinuationTerminatorKind::Abort);
                self.record_termination();
                return Ok(false);
            }
            Terminator::Halt => {
                if self.collect_transitions {
                    self.record_terminal(from, ContinuationTerminatorKind::Halt);
                }
                self.record_phase_terminal(phase, from, ContinuationTerminatorKind::Halt);
                self.record_termination();
                return Ok(false);
            }
        }
        if self.collect_transitions {
            *self
                .stats
                .transition_counts
                .entry((from, self.current))
                .or_default() += 1;
        }
        self.record_phase_transition(phase, from, self.current);
        Ok(true)
    }

    fn current_phase(&self) -> Option<usize> {
        self.stats.phase_metrics.as_ref()?;
        Some(
            self.stack
                .iter()
                .rev()
                .find_map(|frame| frame.phase)
                .unwrap_or(0),
        )
    }

    fn record_phase_transition(
        &mut self,
        phase: Option<usize>,
        from: ContinuationId,
        to: ContinuationId,
    ) {
        let Some(phase) = phase else {
            return;
        };
        if let Some(metrics) = self.stats.phase_metrics.as_mut() {
            metrics.record_transition(phase, from, to);
        }
    }

    fn record_phase_terminal(
        &mut self,
        phase: Option<usize>,
        from: ContinuationId,
        kind: ContinuationTerminatorKind,
    ) {
        let Some(phase) = phase else {
            return;
        };
        if let Some(metrics) = self.stats.phase_metrics.as_mut() {
            metrics.record_terminal(phase, from, kind);
        }
    }

    fn record_portal(
        &mut self,
        operation: PortalOperation,
        region: AggregateRegion,
        offset: usize,
        cells: usize,
    ) {
        let Some(phase) = self.current_phase() else {
            return;
        };
        let current_frame = self.stack.last().expect("current frame exists");
        let function = current_frame.function;
        let activation = current_frame.activation;
        let continuation = self.current;
        let region = match region {
            AggregateRegion::Frame(aggregate) => PortalRegion::Frame {
                function,
                activation,
                aggregate,
            },
            AggregateRegion::Global(global) => PortalRegion::Global { global },
            AggregateRegion::Outbox => PortalRegion::Outbox {
                function,
                activation,
            },
        };
        if let Some(metrics) = self.stats.phase_metrics.as_mut() {
            metrics.record_portal(
                PortalRequestKey {
                    phase,
                    continuation,
                    function,
                    region,
                    operation,
                    cells,
                },
                offset,
            );
        }
    }

    fn record_termination(&mut self) {
        self.stats.final_continuation = Some(self.current);
        self.stats.final_function_stack = self.stack.iter().map(|frame| frame.function).collect();
    }

    fn record_terminal(&mut self, from: ContinuationId, kind: ContinuationTerminatorKind) {
        *self.stats.terminal_counts.entry((from, kind)).or_default() += 1;
    }

    fn read_address(&self, address: Address) -> Result<u8, ContinuationVmError> {
        match address {
            Address::Frame(slot) => self
                .current_frame()?
                .slots
                .get(slot.index())
                .copied()
                .ok_or_else(|| runtime(format!("frame slot {} is out of bounds", slot.index()))),
            Address::Global(id) => match self.global(id)? {
                RuntimeValue::Cell(value) => Ok(*value),
                RuntimeValue::Aggregate(_) => Err(runtime("aggregate global used as a cell")),
            },
            Address::ArrayElement { array, index } => self.read_region_cell(array, index),
            Address::AbiValue => Ok(self.current_frame()?.abi_value),
        }
    }

    fn write_address(&mut self, address: Address, value: u8) -> Result<(), ContinuationVmError> {
        match address {
            Address::Frame(slot) => {
                let cell = self
                    .current_frame_mut()?
                    .slots
                    .get_mut(slot.index())
                    .ok_or_else(|| {
                        runtime(format!("frame slot {} is out of bounds", slot.index()))
                    })?;
                *cell = value;
            }
            Address::Global(id) => match self.global_mut(id)? {
                RuntimeValue::Cell(cell) => *cell = value,
                RuntimeValue::Aggregate(_) => {
                    return Err(runtime("aggregate global used as a cell"));
                }
            },
            Address::ArrayElement { array, index } => {
                self.write_region_cell(array, index, value)?;
            }
            Address::AbiValue => self.current_frame_mut()?.abi_value = value,
        }
        Ok(())
    }

    fn read_operand(&self, operand: ValueOperand) -> Result<OwnedOperand, ContinuationVmError> {
        Ok(match operand {
            ValueOperand::Cell(address) => OwnedOperand::Cell(self.read_address(address)?),
            ValueOperand::Array(region) => {
                OwnedOperand::Aggregate(self.read_region(region, 0, self.region_len(region)?)?)
            }
            ValueOperand::Aggregate {
                region,
                offset,
                cells,
            } => OwnedOperand::Aggregate(self.read_region(region, offset, cells)?),
        })
    }

    fn read_operand_cells(
        &self,
        operand: ValueOperand,
        cells: usize,
    ) -> Result<Vec<u8>, ContinuationVmError> {
        match self.read_operand(operand)? {
            OwnedOperand::Cell(value) if cells == 1 => Ok(vec![value]),
            OwnedOperand::Aggregate(value) if value.len() == cells => Ok(value),
            _ => Err(runtime("operand size does not match aggregate access")),
        }
    }

    fn write_operand(
        &mut self,
        operand: ValueOperand,
        value: &[u8],
    ) -> Result<(), ContinuationVmError> {
        match operand {
            ValueOperand::Cell(address) if value.len() == 1 => {
                self.write_address(address, value[0])
            }
            ValueOperand::Array(region) => self.write_region(region, 0, value),
            ValueOperand::Aggregate {
                region,
                offset,
                cells,
            } if cells == value.len() => self.write_region(region, offset, value),
            _ => Err(runtime("destination size does not match aggregate access")),
        }
    }

    fn read_region(
        &self,
        region: AggregateRegion,
        offset: usize,
        cells: usize,
    ) -> Result<Vec<u8>, ContinuationVmError> {
        let end = offset
            .checked_add(cells)
            .ok_or_else(|| runtime("aggregate range overflows"))?;
        let value = self.region(region)?;
        value
            .get(offset..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| runtime(format!("aggregate range {offset}..{end} is out of bounds")))
    }

    fn write_region(
        &mut self,
        region: AggregateRegion,
        offset: usize,
        value: &[u8],
    ) -> Result<(), ContinuationVmError> {
        let end = offset
            .checked_add(value.len())
            .ok_or_else(|| runtime("aggregate range overflows"))?;
        self.region_mut(region)?
            .get_mut(offset..end)
            .ok_or_else(|| runtime(format!("aggregate range {offset}..{end} is out of bounds")))?
            .copy_from_slice(value);
        Ok(())
    }

    fn read_region_cell(
        &self,
        region: AggregateRegion,
        index: usize,
    ) -> Result<u8, ContinuationVmError> {
        self.region(region)?
            .get(index)
            .copied()
            .ok_or_else(|| runtime(format!("aggregate index {index} is out of bounds")))
    }

    fn write_region_cell(
        &mut self,
        region: AggregateRegion,
        index: usize,
        value: u8,
    ) -> Result<(), ContinuationVmError> {
        *self
            .region_mut(region)?
            .get_mut(index)
            .ok_or_else(|| runtime(format!("aggregate index {index} is out of bounds")))? = value;
        Ok(())
    }

    fn region(&self, region: AggregateRegion) -> Result<&[u8], ContinuationVmError> {
        match region {
            AggregateRegion::Frame(id) => Ok(frame_aggregate(self.current_frame()?, id)?),
            AggregateRegion::Global(id) => match self.global(id)? {
                RuntimeValue::Aggregate(value) => Ok(value),
                RuntimeValue::Cell(_) => Err(runtime("scalar global used as an aggregate")),
            },
            AggregateRegion::Outbox => Ok(&self.current_frame()?.outbox),
        }
    }

    fn region_mut(&mut self, region: AggregateRegion) -> Result<&mut [u8], ContinuationVmError> {
        match region {
            AggregateRegion::Frame(id) => Ok(frame_aggregate_mut(self.current_frame_mut()?, id)?),
            AggregateRegion::Global(id) => match self.global_mut(id)? {
                RuntimeValue::Aggregate(value) => Ok(value),
                RuntimeValue::Cell(_) => Err(runtime("scalar global used as an aggregate")),
            },
            AggregateRegion::Outbox => Ok(&mut self.current_frame_mut()?.outbox),
        }
    }

    fn region_len(&self, region: AggregateRegion) -> Result<usize, ContinuationVmError> {
        Ok(self.region(region)?.len())
    }

    fn read_logical_offset(&self, offset: LogicalOffset) -> Result<usize, ContinuationVmError> {
        Ok(usize::from(self.read_address(offset.low)?)
            | (usize::from(self.read_address(offset.high)?) << 8))
    }

    fn advance_logical_offset(
        &mut self,
        offset: LogicalOffset,
        amount: usize,
    ) -> Result<(), ContinuationVmError> {
        let advanced = (self.read_logical_offset(offset)? + amount) & 0xffff;
        self.write_address(offset.low, advanced as u8)?;
        self.write_address(offset.high, (advanced >> 8) as u8)
    }

    fn function(&self, id: FunctionId) -> Result<&'a FunctionDescriptor, ContinuationVmError> {
        function_at(&self.functions, id)
    }

    fn continuation(&self, id: ContinuationId) -> Result<&'a Continuation, ContinuationVmError> {
        self.continuations
            .get(usize::from(id.get()))
            .and_then(Option::as_ref)
            .copied()
            .ok_or_else(|| runtime(format!("unknown continuation {}", id.get())))
    }

    fn global(&self, id: GlobalId) -> Result<&RuntimeValue, ContinuationVmError> {
        self.globals
            .get(id.index())
            .and_then(Option::as_ref)
            .ok_or_else(|| runtime(format!("unknown global {}", id.index())))
    }

    fn global_mut(&mut self, id: GlobalId) -> Result<&mut RuntimeValue, ContinuationVmError> {
        self.globals
            .get_mut(id.index())
            .and_then(Option::as_mut)
            .ok_or_else(|| runtime(format!("unknown global {}", id.index())))
    }

    fn current_frame(&self) -> Result<&Frame, ContinuationVmError> {
        self.stack
            .last()
            .ok_or_else(|| runtime("execution has no current frame"))
    }

    fn current_frame_mut(&mut self) -> Result<&mut Frame, ContinuationVmError> {
        self.stack
            .last_mut()
            .ok_or_else(|| runtime("execution has no current frame"))
    }

    fn flush_output(&mut self) -> Result<(), ContinuationVmError> {
        if !self.output_buffer.is_empty() {
            self.output.write_all(&self.output_buffer)?;
            self.output_buffer.clear();
        }
        Ok(())
    }

    fn maybe_progress<F>(&mut self, progress: &mut F, force: bool)
    where
        F: FnMut(ContinuationRunProgress),
    {
        let Some(interval) = self.progress_interval else {
            return;
        };
        let work = self.stats.executed_continuations + self.stats.executed_frame_instructions;
        if !force && work & PROGRESS_CHECK_MASK != 0 {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.last_progress) < interval {
            return;
        }
        self.last_progress = now;
        progress(ContinuationRunProgress {
            elapsed: now.duration_since(self.started),
            current_continuation: self.current,
            executed_continuations: self.stats.executed_continuations,
            executed_frame_instructions: self.stats.executed_frame_instructions,
            loop_iterations: self.stats.loop_iterations,
            calls: self.stats.calls,
            output_bytes: self.stats.output_bytes,
            call_depth: self.stack.len(),
            max_call_depth: self.stats.max_call_depth,
        });
    }
}

fn initial_global(global: GlobalDescriptor) -> RuntimeValue {
    match global.value_type() {
        ValueType::Cell => RuntimeValue::Cell(0),
        ValueType::Array(cells) | ValueType::Aggregate { cells } => {
            RuntimeValue::Aggregate(vec![0; cells])
        }
        ValueType::Void => unreachable!("validated globals are not void"),
    }
}

fn new_frame(
    function: &FunctionDescriptor,
    return_to: Option<ContinuationId>,
    activation: u64,
    phase: Option<usize>,
) -> Frame {
    let aggregates_len = function
        .frame_aggregates()
        .iter()
        .map(|aggregate| aggregate.id().index())
        .max()
        .map_or(0, |index| index + 1);
    let mut aggregates = (0..aggregates_len).map(|_| None).collect::<Vec<_>>();
    for aggregate in function.frame_aggregates() {
        aggregates[aggregate.id().index()] = Some(vec![0; aggregate.cells()]);
    }
    Frame {
        function: function.id(),
        activation,
        phase,
        slots: vec![0; function.frame_slots()],
        aggregates,
        outbox: vec![0; function.outbox_cells()],
        abi_value: 0,
        return_to,
    }
}

fn frame_aggregate(frame: &Frame, id: FrameAggregateId) -> Result<&[u8], ContinuationVmError> {
    frame
        .aggregates
        .get(id.index())
        .and_then(Option::as_deref)
        .ok_or_else(|| runtime(format!("unknown frame aggregate {}", id.index())))
}

fn frame_aggregate_mut(
    frame: &mut Frame,
    id: FrameAggregateId,
) -> Result<&mut Vec<u8>, ContinuationVmError> {
    frame
        .aggregates
        .get_mut(id.index())
        .and_then(Option::as_mut)
        .ok_or_else(|| runtime(format!("unknown frame aggregate {}", id.index())))
}

fn function_at<'a>(
    functions: &[Option<&'a FunctionDescriptor>],
    id: FunctionId,
) -> Result<&'a FunctionDescriptor, ContinuationVmError> {
    functions
        .get(id.index())
        .and_then(Option::as_ref)
        .copied()
        .ok_or_else(|| runtime(format!("unknown function {}", id.index())))
}

fn runtime(message: impl Into<String>) -> ContinuationVmError {
    ContinuationVmError::Runtime(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FrameSlot;

    fn execute(source: &str, input: &[u8]) -> (Vec<u8>, ContinuationRunStats) {
        let program = crate::lower_source(source).unwrap();
        let mut input = input;
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                collect_transitions: true,
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        (output, stats)
    }

    #[test]
    fn executes_cells_calls_and_control_flow() {
        let source = "cell twice(cell value) { return value + value; } void main() { cell value = input(); while (value != 0) { output(twice(value)); value = value - 1; } }";
        let (output, stats) = execute(source, &[3]);
        assert_eq!(output, [6, 4, 2]);
        assert_eq!(stats.calls, 3);
        assert_eq!(stats.returns, 3);
        assert!(stats.executed_frame_instructions > 0);
        assert!(stats.max_call_depth >= 2);
        let program = crate::lower_source(source).unwrap();
        stats.validate_transition_accounting(&program).unwrap();
        assert!(
            stats
                .terminal_counts()
                .iter()
                .any(|(_, kind, _)| *kind == ContinuationTerminatorKind::Halt)
        );
    }

    #[test]
    fn executes_dynamic_aggregate_access_and_return() {
        let source = "struct Pair { cell a; cell b; } Pair choose(Pair[2] values, cell index) { return values[index]; } void main() { Pair[2] values; values[1].a = 'O'; values[1].b = 'K'; Pair result = choose(values, 1); output(result.a); output(result.b); }";
        let (output, stats) = execute(source, &[]);
        assert_eq!(output, b"OK");
        assert!(stats.aggregate_loads > 0);
        assert_eq!(stats.calls, 1);
        let program = crate::lower_source(source).unwrap();
        stats.validate_transition_accounting(&program).unwrap();
    }

    #[test]
    fn eof_input_is_zero() {
        let (output, stats) = execute("void main() { output(input()); output(input()); }", &[]);
        assert_eq!(output, [0, 0]);
        assert_eq!(stats.input_operations, 2);
    }

    #[test]
    fn optionally_collects_hot_continuation_transitions() {
        // Keep dispatcher edges in this transition-collection fixture.
        let (program, _) = crate::lower_source_with_options(
            "void main() { cell value = input(); while (value != 0) { output(value); value = value - 1; } }",
            crate::ContinuationOptimizationOptions {
                structure_local_control_flow: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut input = &[2][..];
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                progress_interval: None,
                collect_transitions: true,
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(output, [2, 1]);
        assert!(!stats.hottest_transitions(10).is_empty());
    }

    #[test]
    fn transition_collection_does_not_change_execution_counters() {
        let source = "void main() { cell value = input(); while (value != 0) { output(value); value = value - 1; } }";
        let (program, _) = crate::lower_source_with_options(
            source,
            crate::ContinuationOptimizationOptions {
                structure_local_control_flow: false,
                ..Default::default()
            },
        )
        .unwrap();
        let run = |collect_transitions| {
            let mut input = &[2][..];
            let mut output = Vec::new();
            let stats = run_continuations_with_io(
                &program,
                &mut input,
                &mut output,
                ContinuationRunOptions {
                    progress_interval: None,
                    collect_transitions,
                    ..ContinuationRunOptions::default()
                },
                |_| {},
            )
            .unwrap();
            (output, stats)
        };
        let (output_on, stats_on) = run(true);
        let (output_off, stats_off) = run(false);
        assert_eq!(output_on, output_off);
        assert_eq!(
            stats_on.executed_continuations,
            stats_off.executed_continuations
        );
        assert_eq!(
            stats_on.executed_frame_instructions,
            stats_off.executed_frame_instructions
        );
        assert_eq!(stats_on.loop_iterations, stats_off.loop_iterations);
        assert_eq!(stats_on.calls, stats_off.calls);
        assert_eq!(stats_on.returns, stats_off.returns);
        assert!(stats_on.transitions_collected());
        assert!(!stats_off.transitions_collected());
        assert!(!stats_on.transition_counts().is_empty());
        stats_on.validate_transition_accounting(&program).unwrap();
        assert!(stats_off.validate_transition_accounting(&program).is_err());
    }

    #[test]
    fn transition_metrics_record_portals_and_abort_terminals() {
        let source = "struct Pair { cell a; cell b; } Pair choose(Pair[2] values, cell index) { return values[index]; } void main() { Pair[2] values; values[1].a = 'O'; values[1].b = 'K'; Pair result = choose(values, input()); output(result.a); output(result.b); }";
        let program = crate::lower_source(source).unwrap();
        let mut input = &[1][..];
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                progress_interval: None,
                collect_transitions: true,
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(output, b"OK");
        assert!(stats.aggregate_loads > 0);
        stats.validate_transition_accounting(&program).unwrap();

        let program = crate::lower_source("void main() { abort(); }").unwrap();
        let mut input = &[][..];
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                progress_interval: None,
                collect_transitions: true,
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        assert!(stats.aborted);
        assert!(
            stats
                .terminal_counts()
                .iter()
                .any(|(_, kind, _)| *kind == ContinuationTerminatorKind::Abort)
        );
        stats.validate_transition_accounting(&program).unwrap();
    }

    #[test]
    fn phase_metrics_are_opt_in_and_phase_boundaries_return_after_calls() {
        let source = "cell recurse(cell depth) { if (depth == 0) { return 7; } return recurse(depth - 1); } void main() { output(recurse(2)); }";
        let program = crate::lower_source(source).unwrap();
        let main = program
            .functions()
            .iter()
            .find(|function| function.name() == Some("main"))
            .unwrap();
        let recurse = program
            .functions()
            .iter()
            .find(|function| function.name() == Some("recurse"))
            .unwrap();
        let phase_config = ContinuationPhaseConfig {
            artifact_kind: "source".into(),
            artifact_id: "phase-test".into(),
            boundaries: vec![
                ContinuationPhaseBoundary {
                    phase: "main".into(),
                    function: main.id(),
                },
                ContinuationPhaseBoundary {
                    phase: "recursive".into(),
                    function: recurse.id(),
                },
            ],
            chunk_cells: vec![16],
        };
        let run = |phase_config| {
            let mut input = &[][..];
            let mut output = Vec::new();
            let stats = run_continuations_with_io(
                &program,
                &mut input,
                &mut output,
                ContinuationRunOptions {
                    collect_transitions: true,
                    phase_config,
                    ..ContinuationRunOptions::default()
                },
                |_| {},
            )
            .unwrap();
            (output, stats)
        };
        let (output_on, stats_on) = run(Some(phase_config));
        let (output_off, stats_off) = run(None);
        assert_eq!(output_on, output_off);
        assert_eq!(
            stats_on.executed_continuations,
            stats_off.executed_continuations
        );
        assert_eq!(
            stats_on.executed_frame_instructions,
            stats_off.executed_frame_instructions
        );
        assert_eq!(stats_on.calls, stats_off.calls);
        assert_eq!(stats_on.returns, stats_off.returns);
        assert!(stats_on.phase_metrics_json(&program).is_some());
        assert!(stats_off.phase_metrics_json(&program).is_none());
        let metrics = stats_on.phase_metrics_json(&program).unwrap();
        assert!(metrics["phase_names"].as_array().unwrap().len() >= 3);
        assert!(
            metrics["continuations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["phase"] == "recursive")
        );
        assert!(
            metrics["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["phase"] == "main" && row["kind"] == "halt")
        );
    }

    #[test]
    fn phase_metrics_record_array_load_store_region_and_offset() {
        let cid = |value| ContinuationId::new(value).unwrap();
        let main = FunctionDescriptor::new_typed(
            FunctionId::new(0),
            vec![],
            2,
            vec![],
            0,
            ValueType::Void,
            cid(1),
        );
        let program = ContinuationProgram::new_with_globals(
            FunctionId::new(0),
            vec![GlobalDescriptor::array(GlobalId::new(0), 4)],
            vec![main],
            vec![
                Continuation::new(
                    cid(1),
                    FunctionId::new(0),
                    vec![
                        FrameInstruction::Set {
                            dst: Address::Frame(FrameSlot::new(0)),
                            value: 2,
                        },
                        FrameInstruction::Set {
                            dst: Address::Frame(FrameSlot::new(1)),
                            value: 65,
                        },
                    ],
                    Terminator::ArrayStore {
                        array: AggregateRegion::Global(GlobalId::new(0)),
                        index: Address::Frame(FrameSlot::new(0)),
                        value: Address::Frame(FrameSlot::new(1)),
                        return_to: cid(2),
                    },
                ),
                Continuation::new(
                    cid(2),
                    FunctionId::new(0),
                    vec![],
                    Terminator::ArrayLoad {
                        array: AggregateRegion::Global(GlobalId::new(0)),
                        index: Address::Frame(FrameSlot::new(0)),
                        destination: Address::Frame(FrameSlot::new(1)),
                        return_to: cid(3),
                    },
                ),
                Continuation::new(cid(3), FunctionId::new(0), vec![], Terminator::Halt),
            ],
        )
        .unwrap();
        let mut input = &[][..];
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                collect_transitions: true,
                phase_config: Some(ContinuationPhaseConfig {
                    artifact_kind: "fixture".into(),
                    artifact_id: "array".into(),
                    boundaries: vec![ContinuationPhaseBoundary {
                        phase: "main".into(),
                        function: FunctionId::new(0),
                    }],
                    chunk_cells: vec![16],
                }),
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        let metrics = stats.phase_metrics_json(&program).unwrap();
        let requests = metrics["portal"]["requests"].as_array().unwrap();
        assert!(requests.iter().any(|row| row["operation"] == "array_store"));
        assert!(requests.iter().any(|row| row["operation"] == "array_load"));
        assert!(requests.iter().all(|row| row["region"]["kind"] == "global"));
        assert_eq!(stats.array_loads, 1);
        assert_eq!(stats.array_stores, 1);
    }

    #[test]
    fn phase_metrics_attribute_abort_to_active_phase() {
        let program = crate::lower_source("void main() { abort(); }").unwrap();
        let main = program
            .functions()
            .iter()
            .find(|function| function.name() == Some("main"))
            .unwrap();
        let mut input = &[][..];
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions {
                phase_config: Some(ContinuationPhaseConfig {
                    artifact_kind: "source".into(),
                    artifact_id: "abort".into(),
                    boundaries: vec![ContinuationPhaseBoundary {
                        phase: "main".into(),
                        function: main.id(),
                    }],
                    chunk_cells: vec![16],
                }),
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        let metrics = stats.phase_metrics_json(&program).unwrap();
        assert!(
            metrics["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["phase"] == "main" && row["kind"] == "abort")
        );
    }
}
