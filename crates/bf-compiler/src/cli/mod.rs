//! CLI orchestration shared by BFC source and external CIR input.

mod execution;
mod identity;
mod input;
mod metrics;
mod options;
mod output;

use identity::IrArtifactMetadata;
use options::Options;
use std::process::ExitCode;

pub(crate) fn run() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bfc: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main_result() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse()?;
    let input = input::load(&options)?;
    if let Some(path) = options.cir_output.as_deref() {
        output::write_cir(path, &input, false)?;
    }
    if options.run_ir {
        if let Some(path) = options.ir_dump_output.as_deref() {
            output::write_cir(path, &input, true)?;
        }
        let phase_config = options
            .ir_phase_config
            .as_deref()
            .map(|path| {
                metrics::load_phase_config(
                    path,
                    input.source_kind,
                    &input.artifact_identity,
                    options.optimization_options,
                    &input.program,
                )
            })
            .transpose()?;
        execution::run_ir_program(
            &input.program,
            input.optimization_stats,
            IrArtifactMetadata {
                source_kind: input.source_kind,
                artifact_identity: &input.artifact_identity,
                optimization_options: options.optimization_options,
            },
            options.ir_metrics_output.as_deref(),
            options.ir_progress_interval,
            options.collect_ir_transitions,
            phase_config.as_ref(),
        )?;
    } else if options.cir_output.is_none() {
        output::write_bf(&input, &options)?;
    }
    Ok(())
}
