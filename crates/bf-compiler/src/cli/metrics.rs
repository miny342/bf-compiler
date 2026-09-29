//! Phase configuration and serialized IR execution reports.

use super::identity::{IR_ARTIFACT_ID_VERSION, IrArtifactMetadata, lowering_options_json};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub(super) struct LoadedPhaseConfig {
    pub(super) raw: Value,
    pub(super) runtime: bf_compiler::ContinuationPhaseConfig,
}

pub(super) fn load_phase_config(
    path: &std::ffi::OsStr,
    source_kind: &str,
    actual_artifact_id: &str,
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
    program: &bf_compiler::ContinuationProgram,
) -> Result<LoadedPhaseConfig, Box<dyn std::error::Error>> {
    let raw: Value = serde_json::from_str(
        &fs::read_to_string(Path::new(path))
            .map_err(|error| format!("failed to read phase config: {error}"))?,
    )?;
    let object = raw
        .as_object()
        .ok_or("phase config root must be a JSON object")?;
    if object.get("format").and_then(Value::as_str) != Some("bfc-ir-phase-config-v1") {
        return Err("phase config format must be bfc-ir-phase-config-v1".into());
    }
    let artifact = object
        .get("artifact")
        .and_then(Value::as_object)
        .ok_or("phase config requires artifact object")?;
    let configured_kind = artifact
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("phase config artifact.kind must be a string")?;
    if configured_kind != source_kind {
        return Err(format!(
            "phase config artifact kind {configured_kind:?} does not match {source_kind:?}"
        )
        .into());
    }
    let configured_id = artifact
        .get("id")
        .and_then(Value::as_str)
        .ok_or("phase config artifact.id must be a string")?;
    if configured_id != actual_artifact_id {
        return Err(format!(
            "phase config artifact id {configured_id:?} does not match the actual artifact identity"
        )
        .into());
    }
    if artifact.get("identity_version").and_then(Value::as_str) != Some(IR_ARTIFACT_ID_VERSION) {
        return Err(format!(
            "phase config artifact.identity_version must be {IR_ARTIFACT_ID_VERSION}"
        )
        .into());
    }
    let expected_options = lowering_options_json(optimization_options);
    if artifact.get("lowering_options") != Some(&expected_options) {
        return Err(
            "phase config artifact.lowering_options do not match the IR runner options".into(),
        );
    }
    let chunk_cells = object
        .get("chunk_cells")
        .and_then(Value::as_array)
        .ok_or("phase config requires chunk_cells array")?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or("phase config chunk_cells must contain integers")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected_chunks = chunk_cells.clone();
    expected_chunks.sort_unstable();
    expected_chunks.dedup();
    if expected_chunks != [16] {
        return Err("phase config chunk_cells must contain exactly 16".into());
    }
    let phase_entries = object
        .get("phases")
        .and_then(Value::as_array)
        .ok_or("phase config requires phases array")?;
    if phase_entries.is_empty() {
        return Err("phase config phases array must not be empty".into());
    }
    let mut boundaries = Vec::with_capacity(phase_entries.len());
    for entry in phase_entries {
        let entry = entry
            .as_object()
            .ok_or("phase config phase entry must be an object")?;
        let phase = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or("phase config phase.name must be a string")?;
        let function_id = if source_kind == "source" {
            if entry.get("function_id").is_some() {
                return Err("source phase entries must use function_name".into());
            }
            let function_name = entry
                .get("function_name")
                .and_then(Value::as_str)
                .ok_or("source phase entry requires function_name")?;
            let mut matches = program
                .functions()
                .iter()
                .filter(|function| function.name() == Some(function_name));
            let function = matches
                .next()
                .ok_or_else(|| format!("unknown source phase function {function_name:?}"))?;
            if matches.next().is_some() {
                return Err(
                    format!("source phase function name {function_name:?} is ambiguous").into(),
                );
            }
            function.id()
        } else {
            if entry.get("function_name").is_some() {
                return Err("CIR phase entries must use function_id".into());
            }
            let function_id = entry
                .get("function_id")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or("CIR phase entry requires integer function_id")?;
            let function = program
                .function(bf_compiler::FunctionId::new(function_id))
                .ok_or_else(|| format!("unknown CIR phase function {function_id}"))?;
            function.id()
        };
        boundaries.push(bf_compiler::ContinuationPhaseBoundary {
            phase: phase.to_owned(),
            function: function_id,
        });
    }
    let runtime = bf_compiler::ContinuationPhaseConfig {
        artifact_kind: source_kind.to_owned(),
        artifact_id: actual_artifact_id.to_owned(),
        boundaries,
        chunk_cells,
    };
    Ok(LoadedPhaseConfig { raw, runtime })
}

pub(super) fn write_ir_metrics(
    path: &std::ffi::OsStr,
    artifact: IrArtifactMetadata<'_>,
    phase_config: Option<&Value>,
    program: &bf_compiler::ContinuationProgram,
    optimization: bf_compiler::ContinuationOptimizationStats,
    run: &bf_compiler::ContinuationRunStats,
) -> Result<(), Box<dyn std::error::Error>> {
    let IrArtifactMetadata {
        source_kind,
        artifact_identity,
        optimization_options,
    } = artifact;
    let mut static_in_degree = HashMap::<bf_compiler::ContinuationId, usize>::new();
    let mut static_out_degree = HashMap::<bf_compiler::ContinuationId, usize>::new();
    for continuation in program.continuations() {
        let successors = terminator_successors(continuation.terminator());
        static_out_degree.insert(continuation.id(), successors.len());
        for successor in successors {
            *static_in_degree.entry(successor).or_default() += 1;
        }
    }

    let mut dynamic_in_degree = HashMap::<bf_compiler::ContinuationId, u64>::new();
    let mut dynamic_out_degree = HashMap::<bf_compiler::ContinuationId, u64>::new();
    let transitions = run
        .transition_counts()
        .into_iter()
        .map(|(from, to, count)| {
            *dynamic_out_degree.entry(from).or_default() += count;
            *dynamic_in_degree.entry(to).or_default() += count;
            let kind = program
                .continuation(from)
                .map(|continuation| {
                    bf_compiler::ContinuationTerminatorKind::from_terminator(
                        continuation.terminator(),
                    )
                })
                .unwrap_or(bf_compiler::ContinuationTerminatorKind::Goto);
            json!({
                "from": from.get(),
                "to": to.get(),
                "kind": kind.as_str(),
                "count": count,
            })
        })
        .collect::<Vec<_>>();
    let terminals = run
        .terminal_counts()
        .into_iter()
        .map(|(from, kind, count)| {
            json!({
                "from": from.get(),
                "kind": kind.as_str(),
                "count": count,
            })
        })
        .collect::<Vec<_>>();
    let continuation_rows = program
        .continuations()
        .iter()
        .map(|continuation| {
            let function = program
                .function(continuation.function())
                .expect("validated continuation owner");
            let kind = bf_compiler::ContinuationTerminatorKind::from_terminator(
                continuation.terminator(),
            );
            json!({
                "id": continuation.id().get(),
                "function_id": continuation.function().index(),
                "function_name": function.name(),
                "terminator": kind.as_str(),
                "body_instructions": continuation.body().len(),
                "static_in_degree": static_in_degree.get(&continuation.id()).copied().unwrap_or(0),
                "static_out_degree": static_out_degree.get(&continuation.id()).copied().unwrap_or(0),
                "executions": run.continuation_count(continuation.id()),
                "dynamic_in_degree": dynamic_in_degree.get(&continuation.id()).copied().unwrap_or(0),
                "dynamic_out_degree": dynamic_out_degree.get(&continuation.id()).copied().unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();
    let functions = program
        .functions()
        .iter()
        .map(|function| {
            json!({
                "id": function.id().index(),
                "name": function.name(),
                "entry": function.entry().get(),
            })
        })
        .collect::<Vec<_>>();
    let accounting = match run.validate_transition_accounting(program) {
        Ok(()) => json!({"ok": true}),
        Err(error) => json!({"ok": false, "error": error}),
    };
    let report = json!({
        "format": if phase_config.is_some() {
            "bfc-continuation-ir-metrics-v2"
        } else {
            "bfc-continuation-ir-metrics-v1"
        },
        "source_kind": source_kind,
        "artifact_identity": artifact_identity,
        "artifact_identity_definition": {
            "version": IR_ARTIFACT_ID_VERSION,
            "source_file_order_and_boundaries": "ordered raw-byte frames with an explicit file count; CIR uses one raw-byte frame",
            "lowering_options": lowering_options_json(optimization_options),
        },
        "phase_config": phase_config,
        "measurement_options": {
            "transition_collection": run.transitions_collected(),
            "phase_portal_collection": phase_config.is_some(),
            "phase_chunk_cells": phase_config.and_then(|config| config.get("chunk_cells")),
            "lowering_options": lowering_options_json(optimization_options),
        },
        "functions": functions,
        "continuations": continuation_rows,
        "optimization": {
            "continuations_before": optimization.continuations_before,
            "continuations_after": optimization.continuations_after,
            "empty_gotos_before": optimization.empty_gotos_before,
            "empty_gotos_after": optimization.empty_gotos_after,
            "empty_gotos_threaded": optimization.empty_gotos_threaded,
            "branch_successors_inlined": optimization.branch_successors_inlined,
            "frame_instructions_duplicated": optimization.frame_instructions_duplicated,
            "unreachable_continuations_removed": optimization.unreachable_continuations_removed,
            "successor_references_rewritten": optimization.successor_references_rewritten,
            "function_entries_rewritten": optimization.function_entries_rewritten,
            "continuation_ids_compacted": optimization.continuation_ids_compacted,
            "local_structure": {
                "continuations_removed": optimization.local_structure.continuations_removed,
                "straight_blocks": optimization.local_structure.straight_blocks,
                "branches": optimization.local_structure.branches,
                "loops": optimization.local_structure.loops,
                "scratch_slots": optimization.local_structure.scratch_slots,
            },
        },
        "run": {
            "transitions_collected": run.transitions_collected(),
            "executed_continuations": run.executed_continuations,
            "executed_frame_instructions": run.executed_frame_instructions,
            "loop_iterations": run.loop_iterations,
            "calls": run.calls,
            "returns": run.returns,
            "array_loads": run.array_loads,
            "array_stores": run.array_stores,
            "aggregate_loads": run.aggregate_loads,
            "aggregate_stores": run.aggregate_stores,
            "input_operations": run.input_operations,
            "output_bytes": run.output_bytes,
            "max_call_depth": run.max_call_depth,
            "aborted": run.aborted,
            "final_continuation": run.final_continuation.map(|id| id.get()),
            "final_function_stack": run.final_function_stack.iter().map(|id| id.index()).collect::<Vec<_>>(),
        },
        "transitions": transitions,
        "terminals": terminals,
        "phase_metrics": run.phase_metrics_json(program),
        "accounting": accounting,
    });
    fs::write(Path::new(path), serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

pub(super) fn terminator_successors(
    terminator: &bf_compiler::Terminator,
) -> Vec<bf_compiler::ContinuationId> {
    match terminator {
        bf_compiler::Terminator::Goto { target } => vec![*target],
        bf_compiler::Terminator::Branch {
            then_target,
            else_target,
            ..
        } => vec![*then_target, *else_target],
        bf_compiler::Terminator::Call { return_to, .. }
        | bf_compiler::Terminator::ArrayLoad { return_to, .. }
        | bf_compiler::Terminator::ArrayStore { return_to, .. }
        | bf_compiler::Terminator::AggregateLoad { return_to, .. }
        | bf_compiler::Terminator::AggregateStore { return_to, .. } => vec![*return_to],
        bf_compiler::Terminator::Return { .. }
        | bf_compiler::Terminator::Abort
        | bf_compiler::Terminator::Halt => Vec::new(),
    }
}

pub(super) fn memory_metrics() -> String {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return "rss_kib=unknown hwm_kib=unknown vm_kib=unknown".into();
    };
    let value = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|value| value.split_ascii_whitespace().next())
            .unwrap_or("unknown")
            .to_owned()
    };
    format!(
        "rss_kib={} hwm_kib={} vm_kib={}",
        value("VmRSS:"),
        value("VmHWM:"),
        value("VmSize:"),
    )
}
