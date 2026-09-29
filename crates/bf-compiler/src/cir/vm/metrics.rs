//! Execution counters, phase tracking, and portal request aggregation.

use crate::{
    ContinuationId, ContinuationProgram, FrameAggregateId, FunctionDescriptor, FunctionId,
    GlobalId, Terminator,
};
use serde_json::json;
use std::collections::HashMap;

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
    pub(super) continuation_counts: Vec<u64>,
    pub(super) transition_counts: HashMap<(ContinuationId, ContinuationId), u64>,
    pub(super) terminal_counts: HashMap<(ContinuationId, ContinuationTerminatorKind), u64>,
    pub(super) transitions_collected: bool,
    pub(super) phase_metrics: Option<PhaseMetrics>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum PortalOperation {
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
pub(super) enum PortalRegion {
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
pub(super) struct PortalRequestKey {
    pub(super) phase: usize,
    pub(super) continuation: ContinuationId,
    pub(super) function: FunctionId,
    pub(super) region: PortalRegion,
    pub(super) operation: PortalOperation,
    pub(super) cells: usize,
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
pub(super) struct PhaseMetrics {
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
    pub(super) fn new(
        program: &ContinuationProgram,
        config: ContinuationPhaseConfig,
    ) -> Result<Self, String> {
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

    pub(super) fn phase_for_function(&self, function: FunctionId) -> Option<usize> {
        self.function_phases.get(&function).copied()
    }

    pub(super) fn record_continuation(&mut self, phase: usize, continuation: ContinuationId) {
        if self.last_phase != Some(phase) {
            self.previous_portal = None;
            self.last_phase = Some(phase);
        }
        *self
            .continuation_counts
            .entry((phase, continuation))
            .or_default() += 1;
    }

    pub(super) fn record_transition(
        &mut self,
        phase: usize,
        from: ContinuationId,
        to: ContinuationId,
    ) {
        *self.transition_counts.entry((phase, from, to)).or_default() += 1;
    }

    pub(super) fn record_terminal(
        &mut self,
        phase: usize,
        from: ContinuationId,
        kind: ContinuationTerminatorKind,
    ) {
        *self.terminal_counts.entry((phase, from, kind)).or_default() += 1;
    }

    pub(super) fn record_portal(&mut self, key: PortalRequestKey, offset: usize) {
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
