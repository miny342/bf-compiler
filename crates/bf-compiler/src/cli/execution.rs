//! Streaming IR execution and progress reporting for the CLI.

use super::identity::IrArtifactMetadata;
use super::metrics::{LoadedPhaseConfig, memory_metrics, write_ir_metrics};
use std::io::{self, BufReader, BufWriter, Write};
use std::time::{Duration, Instant};

pub(super) fn run_ir_program(
    program: &bf_compiler::ContinuationProgram,
    optimization_stats: bf_compiler::ContinuationOptimizationStats,
    artifact: IrArtifactMetadata<'_>,
    metrics_path: Option<&std::ffi::OsStr>,
    progress_interval: Duration,
    collect_transitions: bool,
    phase_config: Option<&LoadedPhaseConfig>,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::with_capacity(1024 * 1024, stdin.lock());
    let mut output = BufWriter::with_capacity(1024 * 1024, stdout.lock());
    let execute_started = Instant::now();
    eprintln!("bfc-ir phase=execute status=started {}", memory_metrics());
    let stats = bf_compiler::run_continuations_with_io(
        program,
        &mut input,
        &mut output,
        bf_compiler::ContinuationRunOptions {
            progress_interval: Some(progress_interval),
            collect_transitions,
            phase_config: phase_config.map(|config| config.runtime.clone()),
        },
        |progress| {
            eprintln!(
                "bfc-ir phase=execute status=running elapsed_ms={} continuation={} continuations={} frame_instructions={} loop_iterations={} calls={} call_depth={} max_call_depth={} output_bytes={} {}",
                progress.elapsed.as_millis(),
                progress.current_continuation.get(),
                progress.executed_continuations,
                progress.executed_frame_instructions,
                progress.loop_iterations,
                progress.calls,
                progress.call_depth,
                progress.max_call_depth,
                progress.output_bytes,
                memory_metrics(),
            );
        },
    )?;
    output.flush()?;
    let execute_elapsed = execute_started.elapsed();
    eprintln!(
        "bfc-ir phase=execute status=finished elapsed_ns={} elapsed_ms={} continuations={} frame_instructions={} loop_iterations={} calls={} returns={} array_loads={} array_stores={} aggregate_loads={} aggregate_stores={} input_operations={} output_bytes={} max_call_depth={} aborted={} final_continuation={} function_stack={} {}",
        execute_elapsed.as_nanos(),
        execute_elapsed.as_millis(),
        stats.executed_continuations,
        stats.executed_frame_instructions,
        stats.loop_iterations,
        stats.calls,
        stats.returns,
        stats.array_loads,
        stats.array_stores,
        stats.aggregate_loads,
        stats.aggregate_stores,
        stats.input_operations,
        stats.output_bytes,
        stats.max_call_depth,
        stats.aborted,
        stats
            .final_continuation
            .map_or_else(|| "none".into(), |id| id.get().to_string()),
        stats
            .final_function_stack
            .iter()
            .map(|id| id.index().to_string())
            .collect::<Vec<_>>()
            .join(","),
        memory_metrics(),
    );
    for (rank, (continuation, count)) in stats.hottest_continuations(10).into_iter().enumerate() {
        eprintln!(
            "bfc-ir hot_continuation rank={} continuation={} dispatches={count}",
            rank + 1,
            continuation.get(),
        );
    }
    for (rank, (from, to, count)) in stats.hottest_transitions(10).into_iter().enumerate() {
        eprintln!(
            "bfc-ir hot_transition rank={} from={} to={} transitions={count}",
            rank + 1,
            from.get(),
            to.get(),
        );
    }
    if let Some(path) = metrics_path {
        write_ir_metrics(
            path,
            artifact,
            phase_config.map(|config| &config.raw),
            program,
            optimization_stats,
            &stats,
        )?;
    }
    Ok(())
}
