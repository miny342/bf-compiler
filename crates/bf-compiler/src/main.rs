use std::env;
use std::fs;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
    if source_paths.is_empty() {
        return Err(format!(
            "usage: {} [--run-ir] [--ir-progress-interval 10s] [--unlimited-tape] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source] <source.bfc>...",
            executable.to_string_lossy()
        )
        .into());
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
        }
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
        let program = bf_compiler::lower_sources(&source_files)?;
        eprintln!(
            "bfc-ir phase=lower status=finished elapsed_ms={} functions={} continuations={} globals={} {}",
            lower_started.elapsed().as_millis(),
            program.functions().len(),
            program.continuations().len(),
            program.globals().len(),
            memory_metrics(),
        );

        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut input = BufReader::with_capacity(1024 * 1024, stdin.lock());
        let mut output = BufWriter::with_capacity(1024 * 1024, stdout.lock());
        let execute_started = Instant::now();
        eprintln!("bfc-ir phase=execute status=started {}", memory_metrics());
        let stats = bf_compiler::run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            bf_compiler::ContinuationRunOptions {
                progress_interval: Some(ir_progress_interval),
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
        for (rank, (continuation, count)) in stats.hottest_continuations(10).into_iter().enumerate()
        {
            eprintln!(
                "bfc-ir hot_continuation rank={} continuation={} dispatches={count}",
                rank + 1,
                continuation.get(),
            );
        }
        return Ok(());
    }
    let profile_granularity = profile_granularity
        .or_else(|| {
            profile_map_output
                .as_ref()
                .map(|_| bf_compiler::ProfileGranularity::Continuation)
        })
        .or_else(|| embed_profile.then_some(bf_compiler::ProfileGranularity::Continuation));
    let brainfuck = if let Some(granularity) = profile_granularity {
        let program = bf_compiler::lower_sources(&source_files)?;
        let artifact = if unlimited_tape {
            bf_compiler::compile_continuations_unbounded_with_profile(&program, granularity)?
        } else {
            bf_compiler::compile_continuations_with_profile(&program, granularity)?
        };
        if let Some(path) = profile_map_output {
            fs::write(path, artifact.map.to_json_pretty()?)?;
        }
        if embed_profile {
            artifact.embedded_source()?
        } else {
            artifact.source
        }
    } else if unlimited_tape {
        let program = bf_compiler::lower_sources(&source_files)?;
        bf_compiler::compile_continuations_unbounded(&program)?
    } else {
        bf_compiler::compile_sources(&source_files)?
    };
    io::stdout().write_all(brainfuck.as_bytes())?;
    Ok(())
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
