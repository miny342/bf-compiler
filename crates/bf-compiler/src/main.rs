use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
    let mut compressed_bf = false;
    let mut nibble_transfer = false;
    let mut profile_map_output = None;
    let mut profile_granularity = None;
    let mut embed_profile = false;
    let mut run_ir = false;
    let mut ir_dump_output = None;
    let mut cir_output = None;
    let mut optimization_options = bf_compiler::ContinuationOptimizationOptions::default();
    let mut ir_metrics_output = None;
    let mut collect_ir_transitions = true;
    let mut ir_phase_config = None;
    let mut ir_artifact_id = None;
    let mut cir_input = None;
    let mut ir_progress_interval = Duration::from_secs(10);
    let mut source_paths = Vec::new();
    while let Some(argument) = arguments.next() {
        if argument == "--unlimited-tape" {
            unlimited_tape = true;
        } else if argument == "--enable-nibble-transfer" {
            nibble_transfer = true;
        } else if argument == "--compressed-bf" {
            compressed_bf = true;
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
            optimization_options.inline_branch_successors = true;
        } else if argument == "--disable-2c" {
            optimization_options.inline_branch_successors = false;
        } else if argument == "--enable-local-control-flow" {
            optimization_options.structure_local_control_flow = true;
        } else if argument == "--disable-local-control-flow" {
            optimization_options.structure_local_control_flow = false;
        } else if argument == "--ir-metrics" {
            ir_metrics_output = Some(arguments.next().ok_or("--ir-metrics requires PATH")?);
            collect_ir_transitions = true;
        } else if argument == "--ir-dump" {
            ir_dump_output = Some(arguments.next().ok_or("--ir-dump requires PATH")?);
        } else if argument == "--cir-output" {
            cir_output = Some(arguments.next().ok_or("--cir-output requires PATH")?);
        } else if argument == "--no-ir-transitions" {
            collect_ir_transitions = false;
        } else if argument == "--ir-phase-config" {
            ir_phase_config = Some(arguments.next().ok_or("--ir-phase-config requires PATH")?);
        } else if argument == "--ir-artifact-id" {
            ir_artifact_id = Some(arguments.next().ok_or("--ir-artifact-id requires ID")?);
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
            "usage: {} [--run-ir] [--cir-output PATH] [--ir-dump PATH] [--ir-metrics PATH] [--ir-phase-config PATH --ir-artifact-id ID] [--no-ir-transitions] [--enable-2c|--disable-2c] [--enable-local-control-flow|--disable-local-control-flow] [--ir-progress-interval 10s] [--unlimited-tape] [--compressed-bf] [--enable-nibble-transfer] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source] <source.bfc>...\n       {} --cir-input <program.cir|-> [--run-ir] [--cir-output PATH] [--ir-dump PATH] [--ir-metrics PATH] [--ir-phase-config PATH --ir-artifact-id ID] [--no-ir-transitions] [--enable-2c|--disable-2c] [--unlimited-tape] [--compressed-bf] [--enable-nibble-transfer] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source]",
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
    if ir_dump_output.is_some() && !run_ir {
        return Err("--ir-dump requires --run-ir".into());
    }
    if ir_phase_config.is_some() != ir_artifact_id.is_some() {
        return Err("--ir-phase-config and --ir-artifact-id must be provided together".into());
    }
    if ir_phase_config.is_some() && !run_ir {
        return Err("--ir-phase-config requires --run-ir".into());
    }
    if ir_phase_config.is_some() && ir_metrics_output.is_none() {
        return Err("--ir-phase-config requires --ir-metrics".into());
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
            || embed_profile
            || compressed_bf
            || nibble_transfer)
    {
        return Err("--run-ir cannot be combined with Brainfuck code-generation options".into());
    }
    if let Some(output) = &profile_map_output {
        validate_output_path(
            "--profile-map-output",
            output,
            &source_paths,
            cir_input.as_ref(),
            ir_phase_config.as_ref(),
        )?;
    }
    if let Some(output) = &ir_metrics_output {
        validate_output_path(
            "--ir-metrics",
            output,
            &source_paths,
            cir_input.as_ref(),
            ir_phase_config.as_ref(),
        )?;
    }
    if let Some(output) = &ir_dump_output {
        validate_output_path(
            "--ir-dump",
            output,
            &source_paths,
            cir_input.as_ref(),
            ir_phase_config.as_ref(),
        )?;
    }
    if let Some(output) = &cir_output {
        validate_output_path(
            "--cir-output",
            output,
            &source_paths,
            cir_input.as_ref(),
            ir_phase_config.as_ref(),
        )?;
    }

    let profile_granularity = profile_granularity.or_else(|| {
        (profile_map_output.is_some() || embed_profile)
            .then_some(bf_compiler::ProfileGranularity::Continuation)
    });

    let codegen_options = bf_compiler::AbiCodegenOptions {
        unlimited_tape,
        nibble_transfer,
    };

    if let Some(path) = cir_input {
        let bytes = if path == "-" {
            let mut bytes = Vec::new();
            io::Read::read_to_end(&mut io::stdin().lock(), &mut bytes)?;
            bytes
        } else {
            fs::read(&path)
                .map_err(|error| format!("failed to read '{}': {error}", path.to_string_lossy()))?
        };
        let artifact_identity = cir_artifact_identity(&bytes, optimization_options);
        if run_ir && let Some(cli_id) = ir_artifact_id.as_deref() {
            validate_cli_artifact_id(cli_id, &artifact_identity)?;
        }
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
        if run_ir || cir_output.is_some() {
            if let Some(path) = cir_output.as_deref() {
                write_internal_cir(path, "cir", &artifact_identity, &program)?;
            }
            if !run_ir {
                return Ok(());
            }
            if let Some(path) = ir_dump_output.as_deref() {
                write_ir_dump(path, "cir", &artifact_identity, &program)?;
            }
            let phase_config = ir_phase_config
                .as_deref()
                .map(|path| {
                    load_phase_config(
                        path,
                        "cir",
                        &artifact_identity,
                        optimization_options,
                        &program,
                    )
                })
                .transpose()?;
            run_ir_program(
                &program,
                optimization_stats,
                IrArtifactMetadata {
                    source_kind: "cir",
                    artifact_identity: &artifact_identity,
                    optimization_options,
                },
                ir_metrics_output.as_deref(),
                ir_progress_interval,
                collect_ir_transitions,
                phase_config.as_ref(),
            )?;
            return Ok(());
        }
        let compile_started = Instant::now();
        let stdout = io::stdout();
        let mut output = BufWriter::with_capacity(1024 * 1024, stdout.lock());
        if let Some(granularity) = profile_granularity {
            let source = compile_profiled(
                &program,
                codegen_options,
                granularity,
                profile_map_output.as_deref().map(Path::new),
                embed_profile,
                compressed_bf,
            )?;
            eprintln!(
                "bfc-cir phase=compile status=finished elapsed_ms={} output_bytes={} profiled=true {}",
                compile_started.elapsed().as_millis(),
                source.len(),
                memory_metrics(),
            );
            output.write_all(source.as_bytes())?;
        } else {
            let lowered =
                bf_compiler::lower_continuations_with_codegen_options(&program, codegen_options)?;
            let brainfuck = bf_compiler::optimize_bf(&lowered);
            eprintln!(
                "bfc-cir phase=compile status=finished elapsed_ms={} output_bytes={} profiled=false {}",
                compile_started.elapsed().as_millis(),
                if compressed_bf {
                    brainfuck.compressed_source_len()
                } else {
                    brainfuck.source_len()
                },
                memory_metrics(),
            );
            if compressed_bf {
                brainfuck.write_compressed_source(&mut output)?;
            } else {
                brainfuck.write_source(&mut output)?;
            }
        }
        output.flush()?;
        return Ok(());
    }

    let mut sources = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let bytes = fs::read(path)
            .map_err(|error| format!("failed to read '{}': {error}", path.to_string_lossy()))?;
        let source = String::from_utf8(bytes.clone()).map_err(|error| {
            format!(
                "failed to read '{}' as UTF-8: {error}",
                path.to_string_lossy()
            )
        })?;
        sources.push((path.to_string_lossy().into_owned(), source, bytes));
    }
    let artifact_identity = source_artifact_identity(&sources, optimization_options);
    let source_files: Vec<_> = sources
        .iter()
        .map(|(name, source, _)| bf_compiler::SourceFile::new(name, source))
        .collect();
    if run_ir || cir_output.is_some() {
        if let Some(cli_id) = ir_artifact_id.as_deref() {
            validate_cli_artifact_id(cli_id, &artifact_identity)?;
        }
        let lower_started = Instant::now();
        eprintln!("bfc-ir phase=lower status=started {}", memory_metrics());
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

        if let Some(path) = cir_output.as_deref() {
            write_internal_cir(path, "source", &artifact_identity, &program)?;
        }
        if !run_ir {
            return Ok(());
        }
        if let Some(path) = ir_dump_output.as_deref() {
            write_ir_dump(path, "source", &artifact_identity, &program)?;
        }

        let phase_config = ir_phase_config
            .as_deref()
            .map(|path| {
                load_phase_config(
                    path,
                    "source",
                    &artifact_identity,
                    optimization_options,
                    &program,
                )
            })
            .transpose()?;
        run_ir_program(
            &program,
            optimization_stats,
            IrArtifactMetadata {
                source_kind: "source",
                artifact_identity: &artifact_identity,
                optimization_options,
            },
            ir_metrics_output.as_deref(),
            ir_progress_interval,
            collect_ir_transitions,
            phase_config.as_ref(),
        )?;
        return Ok(());
    }
    let brainfuck = if let Some(granularity) = profile_granularity {
        let (program, _) =
            bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
        compile_profiled(
            &program,
            codegen_options,
            granularity,
            profile_map_output.as_deref().map(Path::new),
            embed_profile,
            compressed_bf,
        )?
    } else if compressed_bf {
        let (program, _) =
            bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
        let lowered =
            bf_compiler::lower_continuations_with_codegen_options(&program, codegen_options)?;
        let mut output = BufWriter::new(io::stdout().lock());
        bf_compiler::optimize_bf(&lowered).write_compressed_source(&mut output)?;
        output.flush()?;
        return Ok(());
    } else {
        let (program, _) =
            bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
        let lowered =
            bf_compiler::lower_continuations_with_codegen_options(&program, codegen_options)?;
        bf_compiler::optimize_bf(&lowered).to_source()
    };
    io::stdout().write_all(brainfuck.as_bytes())?;
    Ok(())
}

/// Keep profile artifacts and embedded output identical for source and CIR input.
fn compile_profiled(
    program: &bf_compiler::ContinuationProgram,
    options: bf_compiler::AbiCodegenOptions,
    granularity: bf_compiler::ProfileGranularity,
    map_output: Option<&Path>,
    embed_profile: bool,
    compressed_bf: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let annotated = bf_compiler::lower_continuations_with_profile_and_codegen_options(
        program,
        granularity,
        options,
    )?;
    let artifact = bf_compiler::optimize_annotated_bf(&annotated).profile_artifact(compressed_bf);
    if let Some(path) = map_output {
        fs::write(path, artifact.map.to_json_pretty()?)?;
    }
    if embed_profile {
        Ok(artifact.embedded_source()?)
    } else {
        Ok(artifact.source)
    }
}

struct LoadedPhaseConfig {
    raw: Value,
    runtime: bf_compiler::ContinuationPhaseConfig,
}

struct IrArtifactMetadata<'a> {
    source_kind: &'a str,
    artifact_identity: &'a str,
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
}

fn write_ir_dump(
    path: &std::ffi::OsStr,
    source_kind: &str,
    artifact_identity: &str,
    program: &bf_compiler::ContinuationProgram,
) -> Result<(), Box<dyn std::error::Error>> {
    let dump = json!({
        "format": "bfc-continuation-ir-v1",
        "source_kind": source_kind,
        "artifact_identity": artifact_identity,
        "program": program,
    });
    fs::write(Path::new(path), serde_json::to_vec_pretty(&dump)?)?;
    Ok(())
}

fn write_internal_cir(
    path: &std::ffi::OsStr,
    source_kind: &str,
    artifact_identity: &str,
    program: &bf_compiler::ContinuationProgram,
) -> Result<(), Box<dyn std::error::Error>> {
    let dump = json!({
        "format": "bfc-continuation-ir-v1",
        "source_kind": source_kind,
        "artifact_identity": artifact_identity,
        "program": program,
    });
    fs::write(Path::new(path), serde_json::to_vec(&dump)?)?;
    Ok(())
}

fn run_ir_program(
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

fn load_phase_config(
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

fn write_ir_metrics(
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

const IR_ARTIFACT_ID_VERSION: &str = "bfc-ir-artifact-v5";

fn lowering_options_json(
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
) -> Value {
    json!({
        "inline_branch_successors": optimization_options.inline_branch_successors,
        "structure_local_control_flow": optimization_options.structure_local_control_flow,
    })
}

fn validate_cli_artifact_id(
    configured: &std::ffi::OsStr,
    actual: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let configured = configured
        .to_str()
        .ok_or("--ir-artifact-id must be UTF-8")?;
    if configured != actual {
        return Err(format!(
            "--ir-artifact-id {configured:?} does not match the actual artifact identity {actual:?}"
        )
        .into());
    }
    Ok(())
}

fn source_artifact_identity(
    sources: &[(String, String, Vec<u8>)],
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
) -> String {
    let mut data = Vec::new();
    data.extend_from_slice(IR_ARTIFACT_ID_VERSION.as_bytes());
    append_identity_frame(&mut data, b"source");
    append_lowering_options(&mut data, optimization_options);
    data.extend_from_slice(&(sources.len() as u64).to_be_bytes());
    for (_, _, bytes) in sources {
        append_identity_frame(&mut data, bytes);
    }
    sha256_hex(&data)
}

fn cir_artifact_identity(
    bytes: &[u8],
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
) -> String {
    let mut data = Vec::new();
    data.extend_from_slice(IR_ARTIFACT_ID_VERSION.as_bytes());
    append_identity_frame(&mut data, b"cir");
    append_lowering_options(&mut data, optimization_options);
    append_identity_frame(&mut data, bytes);
    sha256_hex(&data)
}

fn append_lowering_options(
    data: &mut Vec<u8>,
    options: bf_compiler::ContinuationOptimizationOptions,
) {
    append_identity_frame(
        data,
        if options.inline_branch_successors {
            b"inline_branch_successors=true"
        } else {
            b"inline_branch_successors=false"
        },
    );
    append_identity_frame(
        data,
        if options.structure_local_control_flow {
            b"structure_local_control_flow=true"
        } else {
            b"structure_local_control_flow=false"
        },
    );
}

fn append_identity_frame(data: &mut Vec<u8>, value: &[u8]) {
    data.extend_from_slice(&(value.len() as u64).to_be_bytes());
    data.extend_from_slice(value);
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut padded = input.to_vec();
    let bit_length = (padded.len() as u64) * 8;
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.as_chunks::<64>().0 {
        let mut words = [0u32; 64];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let mut working = state;
        for index in 0..64 {
            let s1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choice = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let temp2 = s0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for (state_word, working_word) in state.iter_mut().zip(working) {
            *state_word = state_word.wrapping_add(working_word);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

fn validate_output_path(
    option: &str,
    output: &std::ffi::OsStr,
    source_paths: &[std::ffi::OsString],
    cir_input: Option<&std::ffi::OsString>,
    phase_config: Option<&std::ffi::OsString>,
) -> Result<(), Box<dyn std::error::Error>> {
    if output == "-" {
        return Err(format!("{option} cannot be stdout").into());
    }
    let output_path = Path::new(output);
    for source in source_paths {
        if equivalent_path(output_path, Path::new(source))? {
            return Err(format!("{option} cannot overwrite an input source").into());
        }
    }
    if let Some(input) = cir_input.filter(|input| input.as_os_str() != "-")
        && equivalent_path(output_path, Path::new(input))?
    {
        return Err(format!("{option} cannot overwrite the CIR input").into());
    }
    if let Some(input) = phase_config
        && equivalent_path(output_path, Path::new(input))?
    {
        return Err(format!("{option} cannot overwrite the phase config input").into());
    }
    Ok(())
}

fn equivalent_path(left: &Path, right: &Path) -> io::Result<bool> {
    if left == right {
        return Ok(true);
    }
    Ok(canonicalize_for_comparison(left)? == canonicalize_for_comparison(right)?)
}

fn canonicalize_for_comparison(path: &Path) -> io::Result<std::path::PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let file_name = path.file_name().ok_or(error)?;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Ok(fs::canonicalize(parent)?.join(file_name))
        }
        Err(error) => Err(error),
    }
}

fn terminator_successors(terminator: &bf_compiler::Terminator) -> Vec<bf_compiler::ContinuationId> {
    match terminator {
        bf_compiler::Terminator::Goto { target } => vec![*target],
        bf_compiler::Terminator::Branch {
            then_target,
            else_target,
            ..
        } => vec![*then_target, *else_target],
        bf_compiler::Terminator::BranchWithBodies {
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

#[cfg(test)]
mod tests {
    use super::{sha256_hex, source_artifact_identity};

    #[test]
    fn artifact_identity_hash_uses_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn source_identity_preserves_order_boundaries_and_options() {
        let first = vec![
            ("a.bfc".to_owned(), "ab".to_owned(), b"ab".to_vec()),
            ("b.bfc".to_owned(), "c".to_owned(), b"c".to_vec()),
        ];
        let second = vec![
            ("a.bfc".to_owned(), "a".to_owned(), b"a".to_vec()),
            ("b.bfc".to_owned(), "bc".to_owned(), b"bc".to_vec()),
        ];
        assert_ne!(
            source_artifact_identity(
                &first,
                bf_compiler::ContinuationOptimizationOptions::default()
            ),
            source_artifact_identity(
                &second,
                bf_compiler::ContinuationOptimizationOptions::default()
            )
        );
        assert_ne!(
            source_artifact_identity(
                &first,
                bf_compiler::ContinuationOptimizationOptions::default()
            ),
            source_artifact_identity(
                &first,
                bf_compiler::ContinuationOptimizationOptions {
                    inline_branch_successors: false,
                    ..Default::default()
                }
            )
        );
    }
}
