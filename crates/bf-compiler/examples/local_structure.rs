//! Reproducible driver for the opt-in local CFG structure experiment.
use bf_compiler::*;
use std::{
    env, fs,
    io::{self, Write},
    time::Instant,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() != 5 {
        return Err(
            "usage: local_structure source|cir baseline|candidate ir|bf INPUT NEW_OUTPUT".into(),
        );
    }
    let [route, variant, mode, path, output]: [_; 5] = args.try_into().unwrap();
    if !["source", "cir"].contains(&route.as_str())
        || !["baseline", "candidate"].contains(&variant.as_str())
        || !["ir", "bf"].contains(&mode.as_str())
    {
        return Err("invalid mode".into());
    }
    let mut result_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    let bytes = fs::read(&path)?;
    let options = ContinuationOptimizationOptions {
        inline_branch_successors: true,
        structure_local_control_flow: false,
    };
    let program = if route == "source" {
        lower_source_with_options(std::str::from_utf8(&bytes)?, options)?.0
    } else {
        lower_selfhost_cir_with_options(&SelfhostCirProgram::decode(&bytes)?, options)?.0
    };
    let before = program.continuations().len();
    let start = Instant::now();
    let (program, stats) = if variant == "candidate" {
        structure_local_control_flow(&program)?
    } else {
        (program, LocalStructureStats::default())
    };
    eprintln!(
        "structure before={before} after={} elapsed_ns={} stats={stats:?}",
        program.continuations().len(),
        start.elapsed().as_nanos()
    );
    if mode == "ir" {
        let start = Instant::now();
        let stats = run_continuations_with_io(
            &program,
            &mut io::stdin().lock(),
            &mut result_file,
            ContinuationRunOptions::default(),
            |_| {},
        )?;
        eprintln!(
            "execute_ns={} continuations={} frame_instructions={} calls={} returns={} array_loads={} array_stores={} aggregate_loads={} aggregate_stores={} input={} output={} aborted={}",
            start.elapsed().as_nanos(),
            stats.executed_continuations,
            stats.executed_frame_instructions,
            stats.calls,
            stats.returns,
            stats.array_loads,
            stats.array_stores,
            stats.aggregate_loads,
            stats.aggregate_stores,
            stats.input_operations,
            stats.output_bytes,
            stats.aborted
        );
    } else {
        let mut map_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(format!("{output}map.json"))?;
        let artifact = compile_continuations_unbounded_with_profile(
            &program,
            ProfileGranularity::Continuation,
        )?;
        result_file.write_all(artifact.source.as_bytes())?;
        map_file.write_all(artifact.map.to_json_pretty()?.as_bytes())?;
    }
    Ok(())
}
