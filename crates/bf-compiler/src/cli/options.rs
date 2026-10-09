//! CLI argument parsing, option compatibility, and output path validation.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

pub(super) struct Options {
    pub(super) compressed_bf: bool,
    pub(super) profile_map_output: Option<OsString>,
    pub(super) profile_granularity: Option<bf_compiler::ProfileGranularity>,
    pub(super) embed_profile: bool,
    pub(super) run_ir: bool,
    pub(super) ir_dump_output: Option<OsString>,
    pub(super) cir_output: Option<OsString>,
    pub(super) optimization_options: bf_compiler::ContinuationOptimizationOptions,
    pub(super) ir_metrics_output: Option<OsString>,
    pub(super) collect_ir_transitions: bool,
    pub(super) ir_phase_config: Option<OsString>,
    pub(super) ir_artifact_id: Option<OsString>,
    pub(super) cir_input: Option<OsString>,
    pub(super) ir_progress_interval: Duration,
    pub(super) source_paths: Vec<OsString>,
    pub(super) codegen_options: bf_compiler::AbiCodegenOptions,
}

impl Options {
    pub(super) fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let mut arguments = env::args_os();
        let executable = arguments.next().unwrap_or_default();
        let mut unlimited_tape = false;
        let mut compressed_bf = false;
        let mut nibble_transfer = false;
        let mut inplace_compare = false;
        let mut anchor_bank = false;
        let mut region_emission = true;
        let mut static_frames = None;
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
            } else if argument == "--enable-static-frames"
                || argument == "--experimental-static-frames"
            {
                // Retain the old spelling as an alias for saved commands.
                static_frames = Some(true);
            } else if argument == "--disable-static-frames" {
                static_frames = Some(false);
            } else if argument == "--enable-nibble-transfer" {
                nibble_transfer = true;
            } else if argument == "--enable-inplace-compare" {
                inplace_compare = true;
            } else if argument == "--enable-anchor-bank" {
                anchor_bank = true;
            } else if argument == "--disable-region-emission" {
                region_emission = false;
            } else if argument == "--enable-region-emission" {
                region_emission = true;
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
            } else if argument == "--enable-function-inline" {
                optimization_options.inline_functions = true;
                optimization_options.generic_function_inline = false;
            } else if argument == "--enable-generic-function-inline" {
                optimization_options.inline_functions = true;
                optimization_options.generic_function_inline = true;
            } else if argument == "--disable-function-inline" {
                optimization_options.inline_functions = false;
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
                "usage: {} [--run-ir] [--cir-output PATH] [--ir-dump PATH] [--ir-metrics PATH] [--ir-phase-config PATH --ir-artifact-id ID] [--no-ir-transitions] [--enable-2c|--disable-2c] [--enable-local-control-flow|--disable-local-control-flow] [--enable-function-inline|--enable-generic-function-inline|--disable-function-inline] [--ir-progress-interval 10s] [--unlimited-tape] [--compressed-bf] [--enable-nibble-transfer] [--enable-inplace-compare] [--enable-anchor-bank] [--disable-region-emission] [--enable-static-frames|--disable-static-frames] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source] <source.bfc>...\n       {} --cir-input <program.cir|-> [--run-ir] [--cir-output PATH] [--ir-dump PATH] [--ir-metrics PATH] [--ir-phase-config PATH --ir-artifact-id ID] [--no-ir-transitions] [--enable-2c|--disable-2c] [--unlimited-tape] [--compressed-bf] [--enable-nibble-transfer] [--enable-inplace-compare] [--enable-anchor-bank] [--disable-region-emission] [--enable-static-frames|--disable-static-frames] [--profile-map-output PATH] [--embed-profile] [--profile-granularity abi|continuation|instruction|source]",
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
                || nibble_transfer
                || inplace_compare
                || anchor_bank
                || !region_emission
                || static_frames.is_some())
        {
            return Err(
                "--run-ir cannot be combined with Brainfuck code-generation options".into(),
            );
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
            static_frames: static_frames
                .unwrap_or_else(|| bf_compiler::AbiCodegenOptions::default().static_frames),
            unlimited_tape,
            nibble_transfer,
            inplace_compare,
            anchor_bank,
            region_emission,
        };

        Ok(Self {
            compressed_bf,
            profile_map_output,
            profile_granularity,
            embed_profile,
            run_ir,
            ir_dump_output,
            cir_output,
            optimization_options,
            ir_metrics_output,
            collect_ir_transitions,
            ir_phase_config,
            ir_artifact_id,
            cir_input,
            ir_progress_interval,
            source_paths,
            codegen_options,
        })
    }
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
