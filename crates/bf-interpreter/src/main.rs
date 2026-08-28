use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use bf_interpreter::{
    ProfileMap, ProfileMode, ProfileOptions, ProfileResult, RunOptions, RunResult, RunStats,
    SiteCounters, Timings, run_with_options,
};
use bf_profiling::embedded_profile_map;
use serde_json::{Value, json};

fn main() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bf-interpreter: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileFormat {
    Text,
    Json,
}

struct CliOptions {
    print_stats: bool,
    print_timings: bool,
    unlimited_tape: bool,
    program_path: PathBuf,
    profile_map_path: Option<PathBuf>,
    accept_embedded_profile: bool,
    profile_mode: Option<ProfileMode>,
    profile_output: Option<PathBuf>,
    profile_format: ProfileFormat,
}

#[derive(Debug, Clone, Copy)]
struct CliTimings {
    source_read: Duration,
    profile_map_read: Duration,
    output_write: Duration,
    process_total: Duration,
}

fn main_result() -> Result<(), Box<dyn std::error::Error>> {
    let process_started = Instant::now();
    let mut arguments = env::args_os();
    let executable = arguments.next().unwrap_or_default();
    let options = parse_arguments(&executable, arguments)?;
    validate_profile_output_paths(
        options.profile_output.as_deref(),
        &options.program_path,
        options.profile_map_path.as_deref(),
    )?;

    let source_read_started = Instant::now();
    let source = fs::read(&options.program_path)?;
    let source_read = source_read_started.elapsed();

    let mut profile_map_read = Duration::ZERO;
    let sidecar_map = if let Some(path) = &options.profile_map_path {
        let started = Instant::now();
        let json = fs::read_to_string(path)?;
        let map = ProfileMap::from_json(&json)?;
        profile_map_read = started.elapsed();
        Some(map)
    } else {
        None
    };
    let embedded_map = if options.accept_embedded_profile {
        let started = Instant::now();
        let map = embedded_profile_map(&source)?
            .ok_or("--accept-embedded-profile requires an embedded profile header")?;
        profile_map_read += started.elapsed();
        Some(map)
    } else {
        None
    };
    let profile_map = match (sidecar_map, embedded_map) {
        (Some(sidecar), Some(embedded)) => {
            if sidecar.bf != embedded.bf
                || sidecar.ranges != embedded.ranges
                || profile_site_identity(&sidecar) != profile_site_identity(&embedded)
            {
                return Err("sidecar and embedded profile maps disagree".into());
            }
            Some(sidecar)
        }
        (Some(map), None) | (None, Some(map)) => Some(map),
        (None, None) => None,
    };

    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let profile = profile_map.map(|map| ProfileOptions {
        map,
        mode: options.profile_mode.unwrap_or(ProfileMode::Counters),
    });
    let report_map = profile.as_ref().map(|profile| profile.map.clone());
    let result = run_with_options(
        &source,
        &input,
        RunOptions {
            unbounded_tape: options.unlimited_tape,
            collect_stats: options.print_stats || profile.is_some(),
            collect_timings: options.print_timings || profile.is_some(),
            profile,
        },
    )?;

    let output_write_started = Instant::now();
    io::stdout().write_all(&result.output)?;
    io::stdout().flush()?;
    let output_write = output_write_started.elapsed();
    let cli_timings = CliTimings {
        source_read,
        profile_map_read,
        output_write,
        process_total: process_started.elapsed(),
    };

    if options.print_stats && result.profile.is_none() {
        print_stats(&result.stats);
    }
    if options.print_timings && result.profile.is_none() {
        print_timings(result.timings.unwrap_or_default(), cli_timings);
    }
    if let Some(profile) = &result.profile {
        let map = report_map
            .as_ref()
            .expect("profile results always retain their input map");
        let report = match options.profile_format {
            ProfileFormat::Text => render_text_report(&result, profile, map, cli_timings),
            ProfileFormat::Json => render_json_report(&result, profile, map, cli_timings)?,
        };
        if let Some(path) = options.profile_output {
            fs::write(path, report)?;
        } else {
            eprint!("{report}");
        }
    }
    Ok(())
}

fn profile_site_identity(
    map: &ProfileMap,
) -> Vec<(
    bf_interpreter::ProfileSiteId,
    Option<bf_interpreter::ProfileSiteId>,
    &str,
    &str,
)> {
    let mut sites = map
        .sites
        .iter()
        .map(|site| {
            (
                site.id,
                site.parent,
                site.kind.as_str(),
                site.stable_key.as_str(),
            )
        })
        .collect::<Vec<_>>();
    sites.sort_by_key(|site| site.0);
    sites
}

fn parse_arguments(
    executable: &OsStr,
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<CliOptions, String> {
    let mut arguments = arguments.into_iter();
    let mut print_stats = false;
    let mut print_timings = false;
    let mut unlimited_tape = false;
    let mut program_path = None;
    let mut profile_map_path = None;
    let mut accept_embedded_profile = false;
    let mut profile_mode = None;
    let mut sample_interval = Duration::from_millis(1);
    let mut sample_interval_set = false;
    let mut profile_output = None;
    let mut profile_format = ProfileFormat::Text;

    while let Some(argument) = arguments.next() {
        if argument == "--stats" {
            print_stats = true;
        } else if argument == "--timings" {
            print_timings = true;
        } else if argument == "--unlimited-tape" {
            unlimited_tape = true;
        } else if argument == "--profile-map" {
            profile_map_path = Some(PathBuf::from(required_value(
                executable,
                "--profile-map",
                arguments.next(),
            )?));
        } else if argument == "--accept-embedded-profile" {
            accept_embedded_profile = true;
        } else if argument == "--profile-mode" {
            let value = required_value(executable, "--profile-mode", arguments.next())?;
            profile_mode = Some(match value.to_str() {
                Some("counters") => ProfileMode::Counters,
                Some("sample") => ProfileMode::Sample {
                    interval: sample_interval,
                },
                Some("exact") => ProfileMode::Exact,
                _ => {
                    return Err(format!(
                        "invalid --profile-mode: {}",
                        value.to_string_lossy()
                    ));
                }
            });
        } else if argument == "--profile-sample-interval" {
            let value = required_value(executable, "--profile-sample-interval", arguments.next())?;
            sample_interval = parse_duration(&value)?;
            sample_interval_set = true;
        } else if argument == "--profile-output" {
            let value = required_value(executable, "--profile-output", arguments.next())?;
            if value == "-" {
                return Err("--profile-output cannot be stdout".into());
            }
            profile_output = Some(PathBuf::from(value));
        } else if argument == "--profile-format" {
            let value = required_value(executable, "--profile-format", arguments.next())?;
            profile_format = match value.to_str() {
                Some("text") => ProfileFormat::Text,
                Some("json") => ProfileFormat::Json,
                _ => {
                    return Err(format!(
                        "invalid --profile-format: {}",
                        value.to_string_lossy()
                    ));
                }
            };
        } else if argument.to_string_lossy().starts_with('-')
            || program_path.replace(PathBuf::from(argument)).is_some()
        {
            return Err(usage(executable));
        }
    }

    let Some(program_path) = program_path else {
        return Err(usage(executable));
    };
    if profile_map_path.is_none()
        && !accept_embedded_profile
        && (profile_mode.is_some()
            || sample_interval_set
            || profile_output.is_some()
            || profile_format != ProfileFormat::Text)
    {
        return Err("profile options require --profile-map or --accept-embedded-profile".into());
    }
    if matches!(profile_mode, Some(ProfileMode::Sample { .. })) {
        profile_mode = Some(ProfileMode::Sample {
            interval: sample_interval,
        });
    }
    if sample_interval_set && !matches!(profile_mode, Some(ProfileMode::Sample { .. })) {
        return Err("--profile-sample-interval requires --profile-mode sample".into());
    }
    Ok(CliOptions {
        print_stats,
        print_timings,
        unlimited_tape,
        program_path,
        profile_map_path,
        accept_embedded_profile,
        profile_mode,
        profile_output,
        profile_format,
    })
}

fn required_value(
    executable: &OsStr,
    option: &str,
    value: Option<OsString>,
) -> Result<OsString, String> {
    value.ok_or_else(|| format!("{option} requires a value\n{}", usage(executable)))
}

fn validate_profile_output_paths(
    profile_output: Option<&Path>,
    program_path: &Path,
    profile_map_path: Option<&Path>,
) -> Result<(), String> {
    let Some(profile_output) = profile_output else {
        return Ok(());
    };

    for (input_path, input_name) in std::iter::once((program_path, "program"))
        .chain(profile_map_path.map(|path| (path, "profile map")))
    {
        if profile_output == input_path {
            return Err(format!(
                "--profile-output must not overwrite the {input_name}: {}",
                input_path.display()
            ));
        }

        if let (Ok(output_canonical), Ok(input_canonical)) = (
            fs::canonicalize(profile_output),
            fs::canonicalize(input_path),
        ) && output_canonical == input_canonical
        {
            return Err(format!(
                "--profile-output must not overwrite the {input_name}: {}",
                input_path.display()
            ));
        }
    }

    Ok(())
}

fn parse_duration(value: &OsStr) -> Result<Duration, String> {
    let value = value
        .to_str()
        .ok_or_else(|| "sampling interval must be UTF-8".to_string())?;
    let (number, unit) = ["ms", "us", "ns", "s"]
        .into_iter()
        .find_map(|unit| value.strip_suffix(unit).map(|number| (number, unit)))
        .ok_or_else(|| format!("invalid sampling interval {value:?}"))?;
    let number: u64 = number
        .parse()
        .map_err(|_| format!("invalid sampling interval {value:?}"))?;
    let duration = match unit {
        "s" => Duration::from_secs(number),
        "ms" => Duration::from_millis(number),
        "us" => Duration::from_micros(number),
        "ns" => Duration::from_nanos(number),
        _ => unreachable!(),
    };
    if duration.is_zero() {
        return Err("sampling interval must be greater than zero".into());
    }
    Ok(duration)
}

fn print_stats(stats: &RunStats) {
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

fn print_timings(timings: Timings, cli: CliTimings) {
    eprintln!("source_read_ns={}", cli.source_read.as_nanos());
    eprintln!("profile_map_read_ns={}", cli.profile_map_read.as_nanos());
    eprintln!("parse_ns={}", timings.parse.as_nanos());
    eprintln!("fast_ir_build_ns={}", timings.fast_ir_build.as_nanos());
    eprintln!("execute_ns={}", timings.execute.as_nanos());
    eprintln!("output_write_ns={}", cli.output_write.as_nanos());
    eprintln!("process_total_ns={}", cli.process_total.as_nanos());
}

fn render_text_report(
    result: &RunResult,
    profile: &ProfileResult,
    map: &ProfileMap,
    cli: CliTimings,
) -> String {
    let timings = result.timings.unwrap_or_default();
    let mode = if profile.sampling_interval.is_some() {
        "sample"
    } else if profile.clock_reads != 0 {
        "exact"
    } else {
        "counters"
    };
    let mut output = String::new();
    output.push_str("profile artifact\n");
    output.push_str(&format!("mode={mode}\n"));
    output.push_str(&format!("source_read_ns={}\n", cli.source_read.as_nanos()));
    output.push_str(&format!(
        "profile_map_read_ns={}\n",
        cli.profile_map_read.as_nanos()
    ));
    output.push_str(&format!("parse_ns={}\n", timings.parse.as_nanos()));
    output.push_str(&format!(
        "fast_ir_build_ns={}\n",
        timings.fast_ir_build.as_nanos()
    ));
    output.push_str(&format!("execute_ns={}\n", timings.execute.as_nanos()));
    output.push_str(&format!(
        "output_write_ns={}\n",
        cli.output_write.as_nanos()
    ));
    output.push_str(&format!(
        "process_total_ns={}\n",
        cli.process_total.as_nanos()
    ));
    output.push_str(&format!("total_samples={}\n", profile.total_samples));
    output.push_str(&format!("clock_reads={}\n", profile.clock_reads));
    output.push_str(&format!(
        "executed_instructions={}\nexecuted_rle_instructions={}\nmax_pointer={}\n",
        result.stats.executed_instructions,
        result.stats.executed_rle_instructions,
        result.stats.max_pointer,
    ));
    output.push_str(&format!(
        "native_operations={} rle_operations={} clear_loops={} scan_loops={} scan_steps={} transfer_loops={} transfer_iterations={}\n",
        result.stats.optimization.executed_native_operations,
        result.stats.optimization.rle_operations,
        result.stats.optimization.clear_loops,
        result.stats.optimization.scan_loops,
        result.stats.optimization.scan_steps,
        result.stats.optimization.transfer_loops,
        result.stats.optimization.transfer_iterations,
    ));
    let duration_sum = profile
        .sites
        .iter()
        .map(|site| site.exclusive_time.as_nanos())
        .sum::<u128>();
    output.push_str(&format!(
        "exclusive_duration_sum_ns={duration_sum} execute_duration_difference_ns={}\n",
        timings.execute.as_nanos() as i128 - duration_sum as i128
    ));
    output.push_str("site tree\n");
    let inclusive = inclusive_durations(profile, map);
    render_site_children(None, 0, profile, map, &inclusive, &mut output);
    output
}

fn render_json_report(
    result: &RunResult,
    profile: &ProfileResult,
    map: &ProfileMap,
    cli: CliTimings,
) -> Result<String, serde_json::Error> {
    let timings = result.timings.unwrap_or_default();
    let inclusive = inclusive_durations(profile, map);
    let sites = profile
        .sites
        .iter()
        .map(|site| {
            let metadata = map
                .sites
                .iter()
                .find(|metadata| metadata.id == site.site)
                .expect("validated map contains every profiled site");
            json!({
                "id": site.site.0,
                "parent": metadata.parent.map(|parent| parent.0),
                "kind": metadata.kind,
                "stable_key": metadata.stable_key,
                "label": metadata.label,
                "source": metadata.source,
                "attributes": metadata.attributes,
                "exclusive_duration_ns": duration_json(site.exclusive_time),
                "inclusive_duration_ns": duration_json(inclusive[&site.site.0]),
                "samples": site.samples,
                "low_confidence": profile.sampling_interval.is_some() && site.samples < 20,
                "profile_block_executions": site.profile_block_executions,
                "counters": counters_json(site.counters),
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "format": "bfc-bf-profile-report",
        "version": 1,
        "interpreter": {
            "package_version": env!("CARGO_PKG_VERSION"),
        },
        "artifact": {
            "instruction_count": map.bf.instruction_count,
            "fnv1a64": format!("{:016x}", map.bf.fnv1a64.0),
            "files": map.files,
        },
        "phase_timings_ns": {
            "source_read": duration_json(cli.source_read),
            "profile_map_read": duration_json(cli.profile_map_read),
            "parse": duration_json(timings.parse),
            "fast_ir_build": duration_json(timings.fast_ir_build),
            "execute": duration_json(timings.execute),
            "output_write": duration_json(cli.output_write),
            "process_total": duration_json(cli.process_total),
        },
        "run_stats": {
            "executed_instructions": result.stats.executed_instructions,
            "executed_rle_instructions": result.stats.executed_rle_instructions,
            "max_pointer": result.stats.max_pointer,
            "optimization": {
                "executed_native_operations": result.stats.optimization.executed_native_operations,
                "rle_operations": result.stats.optimization.rle_operations,
                "clear_loops": result.stats.optimization.clear_loops,
                "scan_loops": result.stats.optimization.scan_loops,
                "scan_steps": result.stats.optimization.scan_steps,
                "transfer_loops": result.stats.optimization.transfer_loops,
                "transfer_iterations": result.stats.optimization.transfer_iterations,
            },
        },
        "profile": {
            "total_samples": profile.total_samples,
            "sampling_interval_ns": profile.sampling_interval.map(duration_json),
            "clock_reads": profile.clock_reads,
            "profile_block_executions": profile.profile_block_executions,
            "measured_execute_time_ns": duration_json(profile.measured_execute_time),
            "exclusive_duration_sum_ns": duration_u128_json(
                profile.sites.iter().map(|site| site.exclusive_time.as_nanos()).sum()
            ),
            "execute_duration_difference_ns": timings.execute.as_nanos() as i128
                - profile.sites.iter().map(|site| site.exclusive_time.as_nanos()).sum::<u128>() as i128,
            "mixed_provenance_native_operations": profile.mixed_provenance_native_operations,
            "sites": sites,
        }
    });
    serde_json::to_string_pretty(&report).map(|mut json| {
        json.push('\n');
        json
    })
}

fn inclusive_durations(profile: &ProfileResult, map: &ProfileMap) -> BTreeMap<u32, Duration> {
    let mut result = map
        .sites
        .iter()
        .map(|site| (site.id.0, Duration::ZERO))
        .collect::<BTreeMap<_, _>>();
    for site_profile in &profile.sites {
        let mut current = Some(site_profile.site);
        while let Some(site) = current {
            *result.entry(site.0).or_default() += site_profile.exclusive_time;
            current = map
                .sites
                .iter()
                .find(|metadata| metadata.id == site)
                .and_then(|metadata| metadata.parent);
        }
    }
    result
}

fn render_site_children(
    parent: Option<bf_interpreter::ProfileSiteId>,
    depth: usize,
    profile: &ProfileResult,
    map: &ProfileMap,
    inclusive: &BTreeMap<u32, Duration>,
    output: &mut String,
) {
    let mut children = map
        .sites
        .iter()
        .filter(|site| site.parent == parent)
        .collect::<Vec<_>>();
    children.sort_by_key(|site| std::cmp::Reverse(inclusive[&site.id.0]));
    for metadata in children {
        let site = profile
            .sites
            .iter()
            .find(|profile| profile.site == metadata.id)
            .expect("validated map contains every profiled site");
        let percent = if profile.measured_execute_time.is_zero() {
            0.0
        } else {
            inclusive[&site.site.0].as_secs_f64() / profile.measured_execute_time.as_secs_f64()
                * 100.0
        };
        let confidence = if profile.sampling_interval.is_some() && site.samples < 20 {
            " low_confidence"
        } else {
            ""
        };
        output.push_str(&format!(
            "{}{} [{}] inclusive_ns={} exclusive_ns={} percent={percent:.2} samples={} fast={} raw={} rle={} entries={}{}\n",
            "  ".repeat(depth),
            metadata.stable_key,
            metadata.kind,
            inclusive[&site.site.0].as_nanos(),
            site.exclusive_time.as_nanos(),
            site.samples,
            site.counters.fast_operations,
            site.counters.raw_bf_instructions,
            site.counters.rle_instructions,
            site.profile_block_executions,
            confidence,
        ));
        render_site_children(
            Some(metadata.id),
            depth + 1,
            profile,
            map,
            inclusive,
            output,
        );
    }
}

fn counters_json(counters: SiteCounters) -> Value {
    json!({
        "fast_operations": counters.fast_operations,
        "raw_bf_instructions": counters.raw_bf_instructions,
        "rle_instructions": counters.rle_instructions,
        "loop_entries": counters.loop_entries,
        "loop_iterations": counters.loop_iterations,
        "rle_operations": counters.rle_operations,
        "clear_loops": counters.clear_loops,
        "scan_loops": counters.scan_loops,
        "scan_steps": counters.scan_steps,
        "transfer_loops": counters.transfer_loops,
        "transfer_iterations": counters.transfer_iterations,
        "input_operations": counters.input_operations,
        "output_operations": counters.output_operations,
        "pointer_distance": counters.pointer_distance,
        "maximum_pointer_observed": counters.maximum_pointer_observed,
    })
}

fn duration_json(duration: Duration) -> Value {
    json!(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
}

fn duration_u128_json(duration: u128) -> Value {
    json!(u64::try_from(duration).unwrap_or(u64::MAX))
}

fn usage(executable: &OsStr) -> String {
    format!(
        "usage: {} [--stats] [--timings] [--unlimited-tape] [--profile-map PATH] [--accept-embedded-profile] \
         [--profile-mode counters|sample|exact] [--profile-sample-interval 1ms] \
         [--profile-output PATH] [--profile-format text|json] <program.bf>",
        executable.to_string_lossy()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sampling_intervals() {
        assert_eq!(
            parse_duration(OsStr::new("1ms")),
            Ok(Duration::from_millis(1))
        );
        assert_eq!(
            parse_duration(OsStr::new("250us")),
            Ok(Duration::from_micros(250))
        );
        assert!(parse_duration(OsStr::new("0ms")).is_err());
        assert!(parse_duration(OsStr::new("1")).is_err());
    }
}
