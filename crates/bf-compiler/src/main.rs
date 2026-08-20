use std::env;
use std::fs;
use std::io::{self, Write};
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
    let Some(source_path) = arguments.next() else {
        return Err(format!("usage: {} <program.bfc>", executable.to_string_lossy()).into());
    };
    if arguments.next().is_some() {
        return Err(format!("usage: {} <program.bfc>", executable.to_string_lossy()).into());
    }

    let source = fs::read_to_string(source_path)?;
    let brainfuck = bf_compiler::compile_source(&source)?;
    io::stdout().write_all(brainfuck.as_bytes())?;
    Ok(())
}
