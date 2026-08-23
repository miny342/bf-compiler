use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use bf_frame_experiment::{
    build_aggregate_probe, build_call_probe, build_portal_probe, build_probe, measure_portal_probe,
    measure_portal_probe_for_length,
};

fn main() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bf-frame-experiment: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main_result() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args();
    let executable = arguments.next().unwrap_or_default();
    let first = arguments.next().unwrap_or_else(|| "frame".to_owned());
    let second = arguments.next();
    if arguments.next().is_some() {
        return Err(format!("usage: {executable} [frame|portal|measure|matrix|abi] [8|16]").into());
    }
    let (mode, chunk_cells) = match (first.as_str(), second) {
        ("8" | "16", None) => ("frame".to_owned(), first),
        (_, chunk_cells) => (first, chunk_cells.unwrap_or_else(|| "16".to_owned())),
    };

    match (mode.as_str(), chunk_cells.as_str()) {
        ("frame", "8") => io::stdout().write_all(build_probe::<8>().as_bytes())?,
        ("frame", "16") => io::stdout().write_all(build_probe::<16>().as_bytes())?,
        ("portal", "8") => io::stdout().write_all(build_portal_probe::<8>().source.as_bytes())?,
        ("portal", "16") => io::stdout().write_all(build_portal_probe::<16>().source.as_bytes())?,
        ("measure", "8") => println!("{:#?}", measure_portal_probe::<8>()),
        ("measure", "16") => println!("{:#?}", measure_portal_probe::<16>()),
        ("matrix", _) => {
            for length in [16, 32, 100, 256] {
                println!(
                    "cells=8 length={length}: {:#?}",
                    measure_portal_probe_for_length::<8>(length)
                );
                println!(
                    "cells=16 length={length}: {:#?}",
                    measure_portal_probe_for_length::<16>(length)
                );
            }
        }
        ("abi", _) => {
            print_abi_measurements::<8>();
            print_abi_measurements::<16>();
        }
        ("frame" | "portal" | "measure", _) => {
            return Err("chunk cell count must be 8 or 16".into());
        }
        _ => return Err("mode must be frame, portal, measure, matrix, or abi".into()),
    }
    Ok(())
}

fn print_abi_measurements<const CELLS: usize>() {
    let call = build_call_probe::<CELLS>();
    let call_result = bf_interpreter::run_with_stats(call.source.as_bytes(), &[20]).unwrap();
    let aggregate = build_aggregate_probe::<CELLS>();
    let aggregate_result =
        bf_interpreter::run_with_stats(aggregate.source.as_bytes(), &[10]).unwrap();
    println!(
        "cells={CELLS} scalar-recursion(depth=20): bytes={} stats={:?}",
        call.source.len(),
        call_result.stats,
    );
    println!(
        "cells={CELLS} aggregate-recursion(depth=10,cells={}): bytes={} stats={:?}",
        aggregate.result_cells,
        aggregate.source.len(),
        aggregate_result.stats,
    );
}
