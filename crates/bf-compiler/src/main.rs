use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::json;

fn main() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bfc: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main_result() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os();
    let executable = arguments.next().unwrap_or_default();
    let mut unlimited_tape = false;
    let mut profile_map_output = None;
    let mut profile_granularity = None;
    let mut embed_profile = false;
    let mut run_ir = false;
    let mut enable_2c = true;
    let mut ir_metrics_output = None;
    let mut collect_ir_transitions = true;
    let mut cir_input = None;
    let mut ir_progress_interval = Duration::from_secs(10);
    let mut source_paths = Vec::new();
    while let Some(argument) = arguments.next() {
        if argument == "--unlimited-tape" {
            unlimited_tape = true;
        } else if argument == "--profile-map-output" {
            profile_map_output = Some(
                arguments
                    .next()
                    .ok_or("--profile-map-output requires PATH")?,
            );
        } else if argument == "--profile-granularity" {
            profile_granularity = Some(parse_granularity(
                &arguments
                    .next()
                    .ok_or("--profile-granularity requires a value")?,
            )?);
        } else if argument == "--embed-profile" {
            embed_profile = true;
        } else if argument == "--run-ir" {
            run_ir = true;
        } else if argument == "--enable-2c" {
            enable_2c = true;
        } else if argument == "--disable-2c" {
            enable_2c = false;
        } else if argument == "--ir-metrics" {
            ir_metrics_output = Some(arguments.next().ok_or("--ir-metrics requires PATH")?);
            collect_ir_transitions = true;
        } else if argument == "--no-ir-transitions" {
            collect_ir_transitions = false;
        } else if argument == "--cir-input" {
            cir_input = Some(arguments.next().ok_or("--cir-input requires PATH or -")?);
        } else if argument == "--ir-progress-interval" {
            ir_progress_interval = parse_duration(
                &arguments
                    .next()
                    .ok_or("--ir-progress-interval requires a value")?,
            )?;
        } else {
            source_paths.push(argument);
        }
    }
    if source_paths.is_empty() && cir_input.is_none() {
        return Err(format!(
            "usage: {} [--run-ir] [--ir-metrics PATH] [--no-ir-transitions] [--enable-2c|--disable-2c] [--ir-progress-interval 10s] [--unlimited-tape] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source] <source.bfc>...\n       {} --cir-input <program.cir|-> [--run-ir] [--ir-metrics PATH] [--no-ir-transitions] [--enable-2c|--disable-2c] [--unlimited-tape] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source]",
            executable.to_string_lossy(),
            executable.to_string_lossy()
        )
        .into());
    }
    if cir_input.is_some() && !source_paths.is_empty() {
        return Err("--cir-input cannot be combined with source paths".into());
    }
    if ir_metrics_output.is_some() && !run_ir {
        return Err("--ir-metrics requires --run-ir".into());
    }
    if profile_granularity.is_some() && profile_map_output.is_none() && !embed_profile {
        return Err(
            "--profile-granularity requires --profile-map-output or --embed-profile".into(),
        );
    }
    if run_ir
        && (unlimited_tape
            || profile_map_output.is_some()
            || profile_granularity.is_some()
            || embed_profile)
    {
        return Err("--run-ir cannot be combined with Brainfuck code-generation options".into());
    }
    if let Some(output) = &profile_map_output {
        if output == "-" {
            return Err("--profile-map-output cannot be stdout".into());
        }
        if source_paths.iter().any(|source| source == output) {
            return Err("--profile-map-output cannot overwrite an input source".into());
        }
        if cir_input
            .as_ref()
            .is_some_and(|input| input != "-" && input == output)
        {
            return Err("--profile-map-output cannot overwrite the CIR input".into());
        }
        let output_path = Path::new(output);
        if output_path.exists() {
            let output_path = fs::canonicalize(output_path)?;
            if source_paths
                .iter()
                .filter_map(|source| fs::canonicalize(source).ok())
                .any(|source| source == output_path)
            {
                return Err("--profile-map-output cannot overwrite an input source".into());
            }
            if cir_input
                .as_ref()
                .filter(|input| *input != "-")
                .and_then(|input| fs::canonicalize(input).ok())
                .is_some_and(|input| input == output_path)
            {
                return Err("--profile-map-output cannot overwrite the CIR input".into());
            }
        }
    }

    let profile_granularity = profile_granularity.or_else(|| {
        (profile_map_output.is_some() || embed_profile)
            .then_some(bf_compiler::ProfileGranularity::Continuation)
    });

    if let Some(path) = cir_input {
        let bytes = if path == "-" {
            let mut bytes = Vec::new();
            io::Read::read_to_end(&mut io::stdin().lock(), &mut bytes)?;
            bytes
        } else {
            fs::read(&path)
                .map_err(|error| format!("failed to read '{}': {error}", path.to_string_lossy()))?
        };
        let decode_started = Instant::now();
        let flat = bf_compiler::SelfhostCirProgram::decode(&bytes)?;
        let mut portal_regions = BTreeSet::new();
        let mut portal_function_regions = BTreeSet::new();
        let mut portal_sites = 0usize;
        for continuation in &flat.continuations {
            for instruction in &continuation.instructions {
                if let bf_compiler::SelfhostCirInstruction::Array {
                    base,
                    cells,
                    storage,
                    ..
                } = instruction
                {
                    portal_sites += 1;
                    portal_regions.insert((*storage as u8, *base, *cells));
                    portal_function_regions.insert((
                        continuation.function,
                        *storage as u8,
                        *base,
                        *cells,
                    ));
                }
            }
        }
        eprintln!(
            "bfc-cir phase=decode status=finished elapsed_ms={} bytes={} functions={} continuations={} static_cells={} portal_sites={} portal_regions={} portal_function_regions={} {}",
            decode_started.elapsed().as_millis(),
            bytes.len(),
            flat.functions.len(),
            flat.continuations.len(),
            flat.static_cells,
            portal_sites,
            portal_regions.len(),
            portal_function_regions.len(),
            memory_metrics(),
        );
        let lower_started = Instant::now();
        let optimization_options = bf_compiler::ContinuationOptimizationOptions {
            inline_branch_successors: enable_2c,
        };
        let (program, optimization_stats) =
            bf_compiler::lower_selfhost_cir_with_options(&flat, optimization_options)?;
        eprintln!(
            "bfc-cir phase=adapt status=finished elapsed_ms={} functions={} continuations={} globals={} {}",
            lower_started.elapsed().as_millis(),
            program.functions().len(),
            program.continuations().len(),
            program.globals().len(),
            memory_metrics(),
        );
        if run_ir {
            run_ir_program(
                &program,
                optimization_stats,
                "cir",
                ir_metrics_output.as_deref(),
                ir_progress_interval,
                collect_ir_transitions,
            )?;
            return Ok(());
        }
        let compile_started = Instant::now();
        let stdout = io::stdout();
        let mut output = BufWriter::with_capacity(1024 * 1024, stdout.lock());
        if let Some(granularity) = profile_granularity {
            let source = compile_profiled(
                &program,
                unlimited_tape,
                granularity,
                profile_map_output.as_deref().map(Path::new),
                embed_profile,
            )?;
            eprintln!(
                "bfc-cir phase=compile status=finished elapsed_ms={} output_bytes={} profiled=true {}",
                compile_started.elapsed().as_millis(),
                source.len(),
                memory_metrics(),
            );
            output.write_all(source.as_bytes())?;
        } else {
            let lowered = if unlimited_tape {
                bf_compiler::lower_continuations_unbounded(&program)?
            } else {
                bf_compiler::lower_continuations(&program)?
            };
            let brainfuck = bf_compiler::optimize_bf(&lowered);
            eprintln!(
                "bfc-cir phase=compile status=finished elapsed_ms={} output_bytes={} profiled=false {}",
                compile_started.elapsed().as_millis(),
                brainfuck.source_len(),
                memory_metrics(),
            );
            brainfuck.write_source(&mut output)?;
        }
        output.flush()?;
        return Ok(());
    }

    let mut sources = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let source = fs::read_to_string(path)
            .map_err(|error| format!("failed to read '{}': {error}", path.to_string_lossy()))?;
        sources.push((path.to_string_lossy().into_owned(), source));
    }
    let source_files: Vec<_> = sources
        .iter()
        .map(|(name, source)| bf_compiler::SourceFile::new(name, source))
        .collect();
    if run_ir {
        let lower_started = Instant::now();
        eprintln!("bfc-ir phase=lower status=started {}", memory_metrics());
        let optimization_options = bf_compiler::ContinuationOptimizationOptions {
            inline_branch_successors: enable_2c,
        };
        let (program, optimization_stats) =
            bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
        eprintln!(
            "bfc-ir phase=lower status=finished elapsed_ms={} functions={} continuations={} globals={} {}",
            lower_started.elapsed().as_millis(),
            program.functions().len(),
            program.continuations().len(),
            program.globals().len(),
            memory_metrics(),
        );

        run_ir_program(
            &program,
            optimization_stats,
            "source",
            ir_metrics_output.as_deref(),
            ir_progress_interval,
            collect_ir_transitions,
        )?;
        return Ok(());
    }
    let optimization_options = bf_compiler::ContinuationOptimizationOptions {
        inline_branch_successors: enable_2c,
    };
    let brainfuck = if let Some(granularity) = profile_granularity {
        let (program, _) =
            bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
        compile_profiled(
            &program,
            unlimited_tape,
            granularity,
            profile_map_output.as_deref().map(Path::new),
            embed_profile,
        )?
    } else if unlimited_tape {
        let (program, _) =
            bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
        bf_compiler::compile_continuations_unbounded(&program)?
    } else {
        let (program, _) =
            bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
        bf_compiler::compile_continuations(&program)?
    };
    io::stdout().write_all(brainfuck.as_bytes())?;
    Ok(())
}

/// Keep profile artifacts and embedded output identical for source and CIR input.
fn compile_profiled(
    program: &bf_compiler::ContinuationProgram,
    unlimited_tape: bool,
    granularity: bf_compiler::ProfileGranularity,
    map_output: Option<&Path>,
    embed_profile: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let artifact = if unlimited_tape {
        bf_compiler::compile_continuations_unbounded_with_profile(program, granularity)?
    } else {
        bf_compiler::compile_continuations_with_profile(program, granularity)?
    };
    if let Some(path) = map_output {
        fs::write(path, artifact.map.to_json_pretty()?)?;
    }
    if embed_profile {
        Ok(artifact.embedded_source()?)
    } else {
        Ok(artifact.source)
    }
}

fn run_ir_program(
    program: &bf_compiler::ContinuationProgram,
    optimization_stats: bf_compiler::ContinuationOptimizationStats,
    source_kind: &str,
    metrics_path: Option<&std::ffi::OsStr>,
    progress_interval: Duration,
    collect_transitions: bool,
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
    eprintln!(
        "bfc-ir phase=execute status=finished elapsed_ms={} continuations={} frame_instructions={} loop_iterations={} calls={} returns={} array_loads={} array_stores={} aggregate_loads={} aggregate_stores={} input_operations={} output_bytes={} max_call_depth={} aborted={} final_continuation={} function_stack={} {}",
        execute_started.elapsed().as_millis(),
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
        write_ir_metrics(path, source_kind, program, optimization_stats, &stats)?;
    }
    Ok(())
}

fn write_ir_metrics(
    path: &std::ffi::OsStr,
    source_kind: &str,
    program: &bf_compiler::ContinuationProgram,
    optimization: bf_compiler::ContinuationOptimizationStats,
    run: &bf_compiler::ContinuationRunStats,
) -> Result<(), Box<dyn std::error::Error>> {
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
        "format": "bfc-continuation-ir-metrics-v1",
        "source_kind": source_kind,
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
        "accounting": accounting,
    });
    fs::write(Path::new(path), serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

fn terminator_successors(terminator: &bf_compiler::Terminator) -> Vec<bf_compiler::ContinuationId> {
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

fn parse_granularity(
    value: &std::ffi::OsStr,
) -> Result<bf_compiler::ProfileGranularity, Box<dyn std::error::Error>> {
    match value.to_string_lossy().as_ref() {
        "abi" => Ok(bf_compiler::ProfileGranularity::Abi),
        "continuation" => Ok(bf_compiler::ProfileGranularity::Continuation),
        "instruction" => Ok(bf_compiler::ProfileGranularity::Instruction),
        "source" => Ok(bf_compiler::ProfileGranularity::Source),
        _ => Err("--profile-granularity must be abi, continuation, instruction, or source".into()),
    }
}

fn parse_duration(value: &std::ffi::OsStr) -> Result<Duration, Box<dyn std::error::Error>> {
    let value = value
        .to_str()
        .ok_or("--ir-progress-interval must be UTF-8")?;
    let (number, unit) = ["ms", "s"]
        .into_iter()
        .find_map(|unit| value.strip_suffix(unit).map(|number| (number, unit)))
        .ok_or("--ir-progress-interval must use ms or s")?;
    let number: u64 = number.parse()?;
    let duration = match unit {
        "ms" => Duration::from_millis(number),
        "s" => Duration::from_secs(number),
        _ => unreachable!(),
    };
    if duration.is_zero() {
        return Err("--ir-progress-interval must be greater than zero".into());
    }
    Ok(duration)
}

fn memory_metrics() -> String {
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
