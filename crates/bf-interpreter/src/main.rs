use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bf-interpreter: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main_result() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os();
    let executable = arguments.next().unwrap_or_default();
    let mut print_stats = false;
    let mut unlimited_tape = false;
    let mut program_path = None;
    for argument in arguments {
        if argument == "--stats" {
            print_stats = true;
        } else if argument == "--unlimited-tape" {
            unlimited_tape = true;
        } else if program_path.replace(argument).is_some() {
            return Err(usage(&executable).into());
        }
    }
    let Some(program_path) = program_path else {
        return Err(usage(&executable).into());
    };

    let source = fs::read(program_path)?;
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;

    let result = if unlimited_tape {
        bf_interpreter::run_unbounded_with_stats(&source, &input)?
    } else {
        bf_interpreter::run_with_stats(&source, &input)?
    };
    io::stdout().write_all(&result.output)?;
    if print_stats {
        let stats = result.stats;
        eprintln!("executed_instructions={}", stats.executed_instructions);
        eprintln!(
            "executed_rle_instructions={}",
            stats.executed_rle_instructions
        );
        eprintln!("max_pointer={}", stats.max_pointer);
        eprintln!(
            "native_operations={}",
            stats.optimization.executed_native_operations
        );
        eprintln!("rle_operations={}", stats.optimization.rle_operations);
        eprintln!("clear_loops={}", stats.optimization.clear_loops);
        eprintln!("scan_loops={}", stats.optimization.scan_loops);
        eprintln!("scan_steps={}", stats.optimization.scan_steps);
        eprintln!("transfer_loops={}", stats.optimization.transfer_loops);
        eprintln!(
            "transfer_iterations={}",
            stats.optimization.transfer_iterations
        );
    }
    Ok(())
}

fn usage(executable: &std::ffi::OsStr) -> String {
    format!(
        "usage: {} [--stats] [--unlimited-tape] <program.bf>",
        executable.to_string_lossy()
    )
}
