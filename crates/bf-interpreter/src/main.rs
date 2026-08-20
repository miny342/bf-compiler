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
    let Some(program_path) = arguments.next() else {
        return Err(format!("usage: {} <program.bf>", executable.to_string_lossy()).into());
    };
    if arguments.next().is_some() {
        return Err(format!("usage: {} <program.bf>", executable.to_string_lossy()).into());
    }

    let source = fs::read(program_path)?;
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;

    let output = bf_interpreter::run(&source, &input)?;
    io::stdout().write_all(&output)?;
    Ok(())
}
