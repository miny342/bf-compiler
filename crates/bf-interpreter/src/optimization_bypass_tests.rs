use super::*;
use crate::tests::{run_reference, test_profile_map};
use bf_profiling::ProfileRange;

const OLD_COMPARE: &[u8] = b"[>>+<[-<->>-]>[-<<[-]>>>]<<<]";
const SLIDE: &[u8] = b"[>>>[-<]<<-]";
const REMOTE: &[u8] = b"[->[>>]>+<<<[<<]>]";

fn options(mask: u8) -> RunOptions {
    RunOptions {
        disable_rle: mask & 1 != 0,
        disable_clear: mask & 2 != 0,
        disable_scan: mask & 4 != 0,
        disable_transfer: mask & 8 != 0,
        disable_countdown: mask & 16 != 0,
        disable_compare: mask & 32 != 0,
        disable_remote_transfer: mask & 64 != 0,
        ..RunOptions::default()
    }
}

fn configured_program(source: &[u8], mask: u8) -> FastProgram {
    parse_optimized_with_options(source, None, Optimizations::from(&options(mask))).unwrap()
}

fn encodings(source: &[u8]) -> [Vec<u8>; 3] {
    let commands = source
        .iter()
        .copied()
        .filter(|byte| b"+-<>[],.".contains(byte))
        .collect::<Vec<_>>();
    let mut encoded = [
        source.to_vec(),
        b"@BFCRLE1;".to_vec(),
        b"@BFCRLE2;".to_vec(),
    ];
    let mut position = 0;
    while position < commands.len() {
        let byte = commands[position];
        let mut count = 1;
        if b"+-<>".contains(&byte) {
            while commands.get(position + count) == Some(&byte) {
                count += 1;
            }
        }
        encoded[1].push(byte);
        encoded[2].push(byte);
        if count > 1 {
            encoded[1].extend_from_slice(count.to_string().as_bytes());
            encoded[2].extend_from_slice(format!("{count:x}").as_bytes());
        }
        position += count;
    }
    encoded
}

fn assert_logical_result(actual: &RunResult, expected: &RunResult) {
    assert_eq!(actual.output, expected.output);
    assert_eq!(
        actual.stats.executed_instructions,
        expected.stats.executed_instructions
    );
    assert_eq!(
        actual.stats.executed_rle_instructions,
        expected.stats.executed_rle_instructions
    );
    assert_eq!(actual.stats.max_pointer, expected.stats.max_pointer);
    if let Some(profile) = &actual.profile
        && profile.sampling_interval.is_none()
    {
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|site| site.counters.raw_bf_instructions)
                .sum::<u64>(),
            actual.stats.executed_instructions
        );
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|site| site.counters.rle_instructions)
                .sum::<u64>(),
            actual.stats.executed_rle_instructions
        );
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|site| site.counters.fast_operations)
                .sum::<u64>(),
            actual.stats.optimization.executed_native_operations
        );
    }
}

#[test]
fn recognition_respects_disabled_prerequisites_and_preserves_independent_idioms() {
    for mask in 0..128 {
        let clear = configured_program(b"[-]", mask);
        assert_eq!(
            matches!(
                clear.instruction_optimization(&clear[0]),
                Some(LoopOptimization::Clear { .. })
            ),
            mask & 3 == 0,
            "clear mask={mask}"
        );
        let scan = configured_program(b"[>>]", mask);
        assert_eq!(
            matches!(
                scan.instruction_optimization(&scan[0]),
                Some(LoopOptimization::Scan { .. })
            ),
            mask & 5 == 0,
            "scan mask={mask}"
        );
        let transfer = configured_program(b"[->+<]", mask);
        assert_eq!(
            matches!(
                transfer.instruction_optimization(&transfer[0]),
                Some(LoopOptimization::Transfer { .. })
            ),
            mask & 9 == 0,
            "transfer mask={mask}"
        );
        let old = configured_program(OLD_COMPARE, mask);
        assert_eq!(
            matches!(
                old.instruction_optimization(&old[0]),
                Some(LoopOptimization::Compare)
            ),
            mask & 35 == 0,
            "old compare mask={mask}"
        );
        let slide = configured_program(SLIDE, mask);
        assert_eq!(
            matches!(
                slide.instruction_optimization(&slide[0]),
                Some(LoopOptimization::CompareSlide)
            ),
            mask & 33 == 0,
            "slide mask={mask}"
        );
        let remote = configured_program(REMOTE, mask);
        assert_eq!(
            matches!(
                remote.instruction_optimization(&remote[0]),
                Some(LoopOptimization::RemoteTransfer)
            ),
            mask & 77 == 0,
            "remote mask={mask}"
        );
        let countdown = configured_program(b"[-[-[-]]]", mask);
        assert_eq!(
            matches!(countdown[0], FastInstruction::Countdown { .. }),
            mask & 17 == 0,
            "countdown mask={mask}"
        );
        if mask & 1 != 0 {
            for program in [clear, scan, transfer, old, slide, remote, countdown] {
                assert!(program.optimizations.is_empty());
                assert!(program.instructions.iter().all(|instruction| matches!(
                    instruction,
                    FastInstruction::RawRun { .. }
                        | FastInstruction::Loop {
                            optimization: None,
                            ..
                        }
                )));
            }
        }
    }
}

#[test]
fn disabled_clears_are_not_reclassified_as_transfers() {
    for body in ["[-]", "[+]", "[+++]", "[---]", "[+--]", "[-+-]"] {
        for source in encodings(body.as_bytes()) {
            let program = configured_program(&source, 2);
            assert!(program.instruction_optimization(&program[0]).is_none());
        }
        for initial in [0, 1, 19, 127, 255] {
            let source = format!(",{body}.");
            let expected = run_reference(source.as_bytes(), &[initial]).unwrap();
            for source in encodings(source.as_bytes()) {
                let actual = run_with_options(&source, &[initial], options(2)).unwrap();
                assert_logical_result(&actual, &expected);
                assert_eq!(actual.stats.optimization.clear_loops, 0);
                assert_eq!(actual.stats.optimization.transfer_loops, 0);
            }
        }
    }
}

#[test]
fn every_switch_combination_matches_raw_execution_and_profile_counts() {
    let cases: &[(&[u8], &[u8])] = &[
        (b"++++[->>>++>+<<<<]>>>.>.", b""),
        (b"+>+>+<<[>].", b""),
        (b",>,<[>>+<[-<->>-]>[-<<[-]>>>]<<<].>.>.>.", &[19, 7]),
        (b",>+>>,<<<[>>>[-<]<<-].>.>.>.", &[19, 7]),
        (b">>+>>+>>+<<<<<,[->[>>]>+<<<[<<]>]>>>>>>>>.<<<<<<<<.", &[3]),
        (b"+++[-[-[-[-]>+<]>+<]>+<]>.", b""),
        (b"++[-[-],].", &[3, 1, 0]),
        (b"++>+++<[-[-]>].", b""),
    ];
    for &(plain, input) in cases {
        let expected = run_reference(plain, input).unwrap();
        for source in encodings(plain) {
            let map = test_profile_map(
                &source,
                vec![ProfileRange {
                    start: 0,
                    end: bf_profiling::bf_identity(&source).instruction_count,
                    site: ProfileSiteId(1),
                }],
            );
            let baseline = run_with_options(
                &source,
                input,
                RunOptions {
                    profile: Some(ProfileOptions {
                        map: map.clone(),
                        mode: ProfileMode::Counters,
                    }),
                    ..options(1)
                },
            )
            .unwrap();
            let baseline_counters = baseline.profile.unwrap().sites[1].counters;
            for mask in 0..128 {
                for mode in [None, Some(ProfileMode::Counters), Some(ProfileMode::Exact)] {
                    let actual = run_with_options(
                        &source,
                        input,
                        RunOptions {
                            profile: mode.map(|mode| ProfileOptions {
                                map: map.clone(),
                                mode,
                            }),
                            ..options(mask)
                        },
                    )
                    .unwrap();
                    assert_logical_result(&actual, &expected);
                    if let Some(profile) = &actual.profile {
                        let counters = profile.sites[1].counters;
                        assert_eq!(counters.loop_entries, baseline_counters.loop_entries);
                        assert_eq!(counters.loop_iterations, baseline_counters.loop_iterations);
                        assert_eq!(
                            counters.pointer_distance,
                            baseline_counters.pointer_distance
                        );
                    }
                    if mask & 1 != 0 {
                        let stats = actual.stats.optimization;
                        assert_eq!(stats.rle_operations, 0);
                        assert_eq!(stats.clear_loops, 0);
                        assert_eq!(stats.scan_loops, 0);
                        assert_eq!(stats.transfer_loops, 0);
                        assert_eq!(stats.remote_transfer_loops, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn scalar_runs_preserve_rle_counts_across_comments_profile_ranges_and_encodings() {
    let plain = b"+++ comment ++>><<++.";
    let expected = run_reference(plain, b"").unwrap();
    for source in encodings(plain) {
        let count = bf_profiling::bf_identity(&source).instruction_count;
        let map = test_profile_map(
            &source,
            (0..count)
                .map(|ordinal| ProfileRange {
                    start: ordinal,
                    end: ordinal + 1,
                    site: ProfileSiteId(1 + (ordinal % 2) as u32),
                })
                .collect(),
        );
        for mode in [
            None,
            Some(ProfileMode::Counters),
            Some(ProfileMode::Exact),
            Some(ProfileMode::Sample {
                interval: Duration::from_millis(1),
            }),
        ] {
            let actual = run_with_options(
                &source,
                b"",
                RunOptions {
                    profile: mode.map(|mode| ProfileOptions {
                        map: map.clone(),
                        mode,
                    }),
                    ..options(1)
                },
            )
            .unwrap();
            assert_logical_result(&actual, &expected);
            assert_eq!(actual.stats.optimization.executed_native_operations, count);
            assert_eq!(actual.stats.optimization.rle_operations, 0);
            if let Some(profile) = &actual.profile {
                assert_eq!(profile.mixed_provenance_native_operations, 0);
                if matches!(mode, Some(ProfileMode::Exact)) {
                    assert_eq!(profile.clock_reads, 2 * count);
                }
            }
        }
    }
}

#[test]
fn sampling_and_embedded_profiles_respect_bypasses() {
    for plain in [
        b"+++>+<[>>+<[-<->>-]>[-<<[-]>>>]<<<].".as_slice(),
        b"+++>+>>+<<<[>>>[-<]<<-].",
        b">>+>>+>>+<<<<<+++[->[>>]>+<<<[<<]>]>>>>>>>>.",
        b"+++[-[-[-[-]>+<]>+<]>+<]>.",
    ] {
        let expected = run_reference(plain, b"").unwrap();
        for source in encodings(plain) {
            let map = test_profile_map(
                &source,
                vec![ProfileRange {
                    start: 0,
                    end: bf_profiling::bf_identity(&source).instruction_count,
                    site: ProfileSiteId(1),
                }],
            );
            let embedded =
                bf_profiling::embed_profile_markers(std::str::from_utf8(&source).unwrap(), &map)
                    .unwrap();
            for mask in [0, 1, 2, 4, 8, 16, 32, 64, 127] {
                let baseline = run_with_options(&source, b"", options(mask)).unwrap();
                for source in [&source[..], embedded.as_bytes()] {
                    let actual = run_with_options(
                        source,
                        b"",
                        RunOptions {
                            profile: Some(ProfileOptions {
                                map: map.clone(),
                                mode: ProfileMode::Sample {
                                    interval: Duration::from_millis(1),
                                },
                            }),
                            ..options(mask)
                        },
                    )
                    .unwrap();
                    assert_eq!(actual.stats, baseline.stats);
                    assert_logical_result(&actual, &expected);
                }
            }
        }
    }
}

#[test]
fn huge_compressed_scalar_runs_remain_compact_and_interruptible() {
    for source in [
        b"@BFCRLE1;+1099511627776".as_slice(),
        b"@BFCRLE2;+10000000000",
    ] {
        let program = configured_program(source, 1);
        assert_eq!(program.len(), 1);
        assert!(matches!(
            program[0],
            FastInstruction::RawRun {
                count: 1_099_511_627_776,
                ..
            }
        ));
        let actual = run_with_options(
            source,
            b"",
            RunOptions {
                progress: Some(ProgressOptions {
                    interval: None,
                    interrupted: || true,
                    callback: Arc::new(|snapshot| {
                        assert_eq!(snapshot.stats.optimization.rle_operations, 0);
                    }),
                }),
                ..options(1)
            },
        );
        assert!(matches!(actual, Err(Error::Interrupted)));
    }
}

#[test]
fn disabled_optimizations_preserve_boundary_errors_and_unbounded_growth() {
    let mut right_transfer = vec![b'>'; TAPE_LEN - 1];
    right_transfer.extend_from_slice(b"++[->+<]>.");
    let mut right_scan = vec![b'>'; TAPE_LEN - 1];
    right_scan.extend_from_slice(b"+[>].");
    for plain in [
        b"+[-<+>]".as_slice(),
        b"+[<]",
        b"++[-[-]<]",
        &right_transfer,
        &right_scan,
    ] {
        for source in encodings(plain) {
            let expected_error = run_with_stats(&source, b"").unwrap_err();
            for mask in 0..128 {
                assert_eq!(
                    run_with_options(&source, b"", options(mask)).unwrap_err(),
                    expected_error,
                    "mask={mask}"
                );
            }
        }
    }
    let expected = run_unbounded_with_stats(&right_transfer, b"").unwrap();
    for source in encodings(&right_transfer) {
        for mask in 0..128 {
            let actual = run_with_options(
                &source,
                b"",
                RunOptions {
                    unbounded_tape: true,
                    ..options(mask)
                },
            )
            .unwrap();
            assert_logical_result(&actual, &expected);
        }
    }
}
