//! BF output, profile sidecars, and internal CIR snapshots.

use super::input::LoadedProgram;
use super::metrics::memory_metrics;
use super::options::Options;
use serde_json::json;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Instant;

pub(super) fn write_bf(
    input: &LoadedProgram,
    options: &Options,
) -> Result<(), Box<dyn std::error::Error>> {
    let compile_started = Instant::now();
    let mut output = BufWriter::with_capacity(1024 * 1024, io::stdout().lock());
    if let Some(granularity) = options.profile_granularity {
        let source = compile_profiled(
            &input.program,
            options.codegen_options,
            granularity,
            options.profile_map_output.as_deref().map(Path::new),
            options.embed_profile,
            options.compressed_bf,
        )?;
        if input.source_kind == "cir" {
            eprintln!(
                "bfc-cir phase=compile status=finished elapsed_ms={} output_bytes={} profiled=true {}",
                compile_started.elapsed().as_millis(),
                source.len(),
                memory_metrics(),
            );
        }
        output.write_all(source.as_bytes())?;
    } else {
        let lowered = bf_compiler::lower_continuations_with_codegen_options(
            &input.program,
            options.codegen_options,
        )?;
        let brainfuck = bf_compiler::optimize_bf(&lowered);
        if input.source_kind == "cir" {
            eprintln!(
                "bfc-cir phase=compile status=finished elapsed_ms={} output_bytes={} profiled=false {}",
                compile_started.elapsed().as_millis(),
                if options.compressed_bf {
                    brainfuck.compressed_source_len()
                } else {
                    brainfuck.source_len()
                },
                memory_metrics(),
            );
        }
        if options.compressed_bf {
            brainfuck.write_compressed_source(&mut output)?;
        } else {
            brainfuck.write_source(&mut output)?;
        }
    }
    output.flush()?;
    Ok(())
}

pub(super) fn write_cir(
    path: &std::ffi::OsStr,
    input: &LoadedProgram,
    pretty: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let dump = json!({
        "format": "bfc-continuation-ir-v1",
        "source_kind": input.source_kind,
        "artifact_identity": input.artifact_identity,
        "program": input.program,
    });
    let bytes = if pretty {
        serde_json::to_vec_pretty(&dump)?
    } else {
        serde_json::to_vec(&dump)?
    };
    fs::write(Path::new(path), bytes)?;
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
