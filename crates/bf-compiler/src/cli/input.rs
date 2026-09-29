//! Source and binary CIR loading into a common CLI compilation result.

use super::identity::{cir_artifact_identity, source_artifact_identity, validate_cli_artifact_id};
use super::metrics::memory_metrics;
use super::options::Options;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::time::Instant;

pub(super) struct LoadedProgram {
    pub(super) program: bf_compiler::ContinuationProgram,
    pub(super) optimization_stats: bf_compiler::ContinuationOptimizationStats,
    pub(super) source_kind: &'static str,
    pub(super) artifact_identity: String,
}

pub(super) fn load(options: &Options) -> Result<LoadedProgram, Box<dyn std::error::Error>> {
    if let Some(path) = options.cir_input.as_deref() {
        load_cir(path, options)
    } else {
        load_sources(options)
    }
}

fn load_cir(path: &OsStr, options: &Options) -> Result<LoadedProgram, Box<dyn std::error::Error>> {
    let optimization_options = options.optimization_options;
    let run_ir = options.run_ir;
    let ir_artifact_id = &options.ir_artifact_id;
    let bytes = if path == "-" {
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut io::stdin().lock(), &mut bytes)?;
        bytes
    } else {
        fs::read(path)
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

    Ok(LoadedProgram {
        program,
        optimization_stats,
        source_kind: "cir",
        artifact_identity,
    })
}

fn load_sources(options: &Options) -> Result<LoadedProgram, Box<dyn std::error::Error>> {
    let source_paths = &options.source_paths;
    let optimization_options = options.optimization_options;
    let ir_artifact_id = &options.ir_artifact_id;
    let mut sources = Vec::with_capacity(source_paths.len());
    for path in source_paths {
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

    let report_lowering = options.run_ir || options.cir_output.is_some();
    if report_lowering && let Some(cli_id) = ir_artifact_id.as_deref() {
        validate_cli_artifact_id(cli_id, &artifact_identity)?;
    }
    let lower_started = Instant::now();
    if report_lowering {
        eprintln!("bfc-ir phase=lower status=started {}", memory_metrics());
    }
    let (program, optimization_stats) =
        bf_compiler::lower_sources_with_options(&source_files, optimization_options)?;
    if report_lowering {
        eprintln!(
            "bfc-ir phase=lower status=finished elapsed_ms={} functions={} continuations={} globals={} {}",
            lower_started.elapsed().as_millis(),
            program.functions().len(),
            program.continuations().len(),
            program.globals().len(),
            memory_metrics(),
        );
    }
    Ok(LoadedProgram {
        program,
        optimization_stats,
        source_kind: "source",
        artifact_identity,
    })
}
