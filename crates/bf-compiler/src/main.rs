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
    let source_paths: Vec<_> = arguments.collect();
    if source_paths.is_empty() {
        return Err(format!("usage: {} <source.bfc>...", executable.to_string_lossy()).into());
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
    let brainfuck = bf_compiler::compile_sources(&source_files)?;
    io::stdout().write_all(brainfuck.as_bytes())?;
    Ok(())
}
