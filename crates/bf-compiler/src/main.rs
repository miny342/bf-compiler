use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

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
        } else {
            source_paths.push(argument);
        }
    }
    if source_paths.is_empty() {
        return Err(format!(
            "usage: {} [--unlimited-tape] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source] <source.bfc>...",
            executable.to_string_lossy()
        )
        .into());
    }
    if profile_granularity.is_some() && profile_map_output.is_none() && !embed_profile {
        return Err(
            "--profile-granularity requires --profile-map-output or --embed-profile".into(),
        );
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
