use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Read};

use bf_compiler::{lower_continuations, lower_source, optimize_bf_with_stats};
use bf_interpreter::run_with_stats;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os();
    let _executable = arguments.next();
    let path = arguments
        .next()
        .ok_or("usage: profile_bf_ir <program.bfc> (program input is read from stdin)")?;
    if arguments.next().is_some() {
        return Err("usage: profile_bf_ir <program.bfc> (program input is read from stdin)".into());
    }

    let source = fs::read_to_string(&path)?;
    let continuation_program = lower_source(&source)?;
    let unoptimized = lower_continuations(&continuation_program)?;
    let (optimized, static_stats) = optimize_bf_with_stats(&unoptimized);
    let unoptimized_source = unoptimized.to_source();
    let optimized_source = optimized.to_source();

    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let unoptimized_run = run_with_stats(unoptimized_source.as_bytes(), &input)?;
    let optimized_run = run_with_stats(optimized_source.as_bytes(), &input)?;
    if unoptimized_run.output != optimized_run.output {
        return Err("optimized program produced different output".into());
    }

    println!("program: {}", path.to_string_lossy());
    println!("input bytes: {}", input.len());
    println!("output bytes: {}", optimized_run.output.len());
    println!();
    println!("metric                         before          after     reduction");
    print_metric(
        "BF IR nodes",
        static_stats.instruction_nodes_before as u64,
        static_stats.instruction_nodes_after as u64,
    );
    print_metric(
        "BF source bytes",
        static_stats.source_bytes_before as u64,
        static_stats.source_bytes_after as u64,
    );
    print_metric(
        "executed BF instructions",
        unoptimized_run.stats.executed_instructions,
        optimized_run.stats.executed_instructions,
    );
    print_metric(
        "executed RLE instructions",
        unoptimized_run.stats.executed_rle_instructions,
        optimized_run.stats.executed_rle_instructions,
    );
    print_metric(
        "maximum tape pointer",
        unoptimized_run.stats.max_pointer as u64,
        optimized_run.stats.max_pointer as u64,
    );

    Ok(())
}

fn print_metric(label: &str, before: u64, after: u64) {
    let reduction = if before == 0 {
        0.0
    } else {
        100.0 * (before.saturating_sub(after) as f64) / (before as f64)
    };
    println!("{label:<28} {before:>12} {after:>14} {reduction:>8.2}%");
}
