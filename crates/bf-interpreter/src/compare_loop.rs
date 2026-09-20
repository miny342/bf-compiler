//! Constant-time evaluation of two destructive comparison idioms.
//!
//! This is a BF idiom, not an ABI operation: no absolute address, compiler
//! metadata, or assumed scratch state is used. With cells L,R,0,0 at entry,
//! each iteration subtracts one from both operands until either is zero;
//! any remaining L is cleared. The pointer returns to L. Other tape states
//! and boundary crossings retain the original interpreter path.
//! The shorter `[>>>[-<]<<-]` uses L,1,0,R and preserves the remainder,
//! exiting at L for L <= R or at the adjacent flag otherwise.
use super::*;

fn moved(instruction: &FastInstruction, expected: isize) -> bool {
    matches!(instruction, FastInstruction::Move { amount, source_offsets, .. }
        if *amount == expected && source_offsets.len() == expected.unsigned_abs())
}

fn added(instruction: &FastInstruction, expected: u8) -> bool {
    matches!(instruction, FastInstruction::Add { amount, raw_count: 1, .. }
        if *amount == expected)
}

pub(super) fn recognize(body: &[FastInstruction]) -> bool {
    let [
        a,
        b,
        c,
        FastInstruction::Loop { body: nonzero, .. },
        d,
        FastInstruction::Loop { body: zero, .. },
        e,
    ] = body
    else {
        return false;
    };
    let [n0, n1, n2, n3, n4] = nonzero.as_slice() else {
        return false;
    };
    let [z0, z1, FastInstruction::Loop { body: clear, .. }, z2] = zero.as_slice() else {
        return false;
    };
    let [clear] = clear.as_slice() else {
        return false;
    };
    moved(a, 2)
        && added(b, 1)
        && moved(c, -1)
        && moved(d, 1)
        && moved(e, -3)
        && added(n0, 255)
        && moved(n1, -1)
        && added(n2, 255)
        && moved(n3, 2)
        && added(n4, 255)
        && added(z0, 255)
        && moved(z1, -2)
        && added(clear, 255)
        && moved(z2, 3)
}

pub(super) fn recognize_slide(body: &[FastInstruction]) -> bool {
    let [a, FastInstruction::Loop { body: inner, .. }, b, c] = body else {
        return false;
    };
    let [d, e] = inner.as_slice() else {
        return false;
    };
    moved(a, 3) && moved(b, -2) && added(c, 255) && added(d, 255) && moved(e, -1)
}

impl Machine<'_> {
    pub(super) fn execute_compare_slide<const PROFILE: bool>(
        &mut self,
        site: ResolvedProfileSite,
    ) -> bool {
        let left = self.tape[self.pointer];
        if left == 0 {
            self.countdown_counts::<PROFILE>(site, 1, 1);
            return true;
        }
        let Some(end) = self
            .pointer
            .checked_add(3)
            .filter(|end| *end < self.tape.len())
        else {
            return false;
        };
        if self.tape[self.pointer + 1] != 1 || self.tape[self.pointer + 2] != 0 {
            return false;
        }
        let right = self.tape[end];
        let paired = u64::from(left.min(right));
        let greater = u64::from(left > right);
        self.countdown_counts::<PROFILE>(
            site,
            1 + 11 * paired + 8 * greater,
            1 + 8 * paired + 5 * greater,
        );
        let rle_operations = 5 * paired + 3 * greater;
        self.optimization.rle_operations += rle_operations;
        self.tape[self.pointer] = left.saturating_sub(right);
        self.tape[end] = right.saturating_sub(left);
        self.tape[self.pointer + 1] = 1 - greater as u8;
        self.pointer += greater as usize;
        self.max_pointer = self.max_pointer.max(end);
        if PROFILE {
            let counters = &mut self.profile.as_mut().unwrap().site_mut(site).counters;
            counters.loop_entries += paired + greater;
            counters.loop_iterations += 2 * paired + greater;
            counters.rle_operations += rle_operations;
            counters.pointer_distance += 6 * paired + 5 * greater;
            self.record_maximum_pointer_at(site, end);
        }
        true
    }

    pub(super) fn execute_compare_loop<const PROFILE: bool>(
        &mut self,
        site: ResolvedProfileSite,
    ) -> bool {
        let left = self.tape[self.pointer];
        if left == 0 {
            self.countdown_counts::<PROFILE>(site, 1, 1);
            return true;
        }
        let Some(end) = self
            .pointer
            .checked_add(3)
            .filter(|end| *end < self.tape.len())
        else {
            return false;
        };
        if self.tape[self.pointer + 2] != 0 || self.tape[end] != 0 {
            return false;
        }
        let right = self.tape[self.pointer + 1];
        let paired = u64::from(left.min(right));
        let remainder = u64::from(left.saturating_sub(right));
        let tail = u64::from(remainder != 0);
        // Each paired pass executes 18 raw / 14 RLE commands. The final
        // left-only pass executes 19 / 13 plus the remaining clear iterations.
        self.countdown_counts::<PROFILE>(
            site,
            1 + 18 * paired + 19 * tail + 2 * remainder,
            1 + 14 * paired + 13 * tail + 2 * remainder,
        );
        let rle_operations = 10 * paired + 8 * tail;
        self.optimization.rle_operations += rle_operations;
        self.optimization.clear_loops += tail;
        self.tape[self.pointer] = 0;
        self.tape[self.pointer + 1] = right.saturating_sub(left);
        self.max_pointer = self.max_pointer.max(end);
        if PROFILE {
            let counters = &mut self.profile.as_mut().unwrap().site_mut(site).counters;
            // The outer loop entry is counted by execute_block_profiled.
            counters.loop_entries += 2 * paired + 3 * tail;
            counters.loop_iterations += 2 * paired + 2 * tail + remainder;
            counters.rle_operations += rle_operations;
            counters.clear_loops += tail;
            counters.pointer_distance += 10 * paired + 12 * tail;
            self.record_maximum_pointer_at(site, end);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{assert_matches_reference, test_profile_map};
    use bf_profiling::ProfileRange;

    // Recognizer fixtures, not a contract on every compiler lowering.
    // Keep the old form for existing BF artifacts alongside the new form.
    const LOOP: &[u8] = b"[>>+<[-<->>-]>[-<<[-]>>>]<<<]";
    const SLIDE: &[u8] = b"[>>>[-<]<<-]";

    #[test]
    fn slide_covers_every_pair_exit_pointer_remainder_and_raw_counts() {
        let parsed = parse_optimized(SLIDE, None).unwrap();
        assert!(matches!(
            parsed[0],
            FastInstruction::Loop {
                optimization: Some(LoopOptimization::CompareSlide),
                ..
            }
        ));
        let mut source = b",>+>>,<<<".to_vec();
        source.extend_from_slice(SLIDE);
        source.extend_from_slice(b".>.>.>.");
        let raw = parse(&source).unwrap();
        let fast = parse_optimized(&source, None).unwrap();
        for left in 0_u8..=255 {
            for right in 0_u8..=255 {
                let mut machine = Machine::new(b"", false, None, None, None, None);
                machine.tape[..4].copy_from_slice(&[left, 1, 0, right]);
                machine.execute_block(&parsed).unwrap();
                assert_eq!(
                    &machine.tape[..4],
                    &[
                        left.saturating_sub(right),
                        u8::from(left <= right),
                        0,
                        right.saturating_sub(left)
                    ]
                );
                assert_eq!(machine.pointer, usize::from(left > right));
                let input = [left, right];
                let actual = execute(&fast, &input, false, None, None).unwrap();
                let expected = execute_reference(&raw, &input).unwrap();
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
            }
        }
    }

    #[test]
    fn slide_runtime_guards_preserve_fallback_state() {
        for value in 0..=255 {
            for cell in [1, 2] {
                if (cell == 1 && value == 1) || (cell == 2 && value == 0) {
                    continue;
                }
                let mut m = Machine::new(b"", false, None, None, None, None);
                m.tape[..4].copy_from_slice(&[5, 1, 0, 8]);
                m.tape[cell] = value;
                let before = m.tape.clone();
                assert!(!m.execute_compare_slide::<false>(ResolvedProfileSite::ROOT));
                assert_eq!(m.tape, before);
                assert_eq!(m.pointer, 0);
                assert_eq!(m.executed_instructions, 0);
            }
        }
        for distance in 0..=3 {
            for left in [0, 1] {
                let mut source = vec![b'>'; TAPE_LEN - 1 - distance];
                source.extend(std::iter::repeat_n(b'+', left));
                source.extend_from_slice(SLIDE);
                // Invalid flag=0 is deliberately left for the fallback path.
                let fast = run_with_stats(&source, b"");
                let raw = crate::tests::run_reference(&source, b"");
                match (fast, raw) {
                    (Ok(fast), Ok(raw)) => {
                        assert_eq!(fast.output, raw.output);
                        assert_eq!(
                            fast.stats.executed_instructions,
                            raw.stats.executed_instructions
                        );
                        assert_eq!(
                            fast.stats.executed_rle_instructions,
                            raw.stats.executed_rle_instructions
                        );
                        assert_eq!(fast.stats.max_pointer, raw.stats.max_pointer);
                    }
                    (Err(fast), Err(raw)) => assert_eq!(fast, raw),
                    other => panic!("boundary mismatch: {other:?}"),
                }
            }
        }
        let mut source = vec![b'>'; TAPE_LEN - 2];
        source.extend_from_slice(b"+>+<");
        source.extend_from_slice(SLIDE);
        source.push(b'.');
        assert!(matches!(
            run_with_stats(&source, b""),
            Err(Error::TapeOverflow { .. })
        ));
        let grown = run_unbounded_with_stats(&source, b"").unwrap();
        assert_eq!(grown.output, [0]);
        assert_eq!(grown.stats.max_pointer, TAPE_LEN + 1);
    }

    #[test]
    fn every_operand_pair_matches_raw_execution() {
        let mut source = b",>,<".to_vec();
        source.extend_from_slice(LOOP);
        source.extend_from_slice(b".>.>.>.");
        let parsed = parse_optimized(&source, None).unwrap();
        assert!(matches!(
            parsed[4],
            FastInstruction::Loop {
                optimization: Some(LoopOptimization::Compare),
                ..
            }
        ));
        let reference = parse(&source).unwrap();
        for left in 0..=255 {
            for right in 0..=255 {
                let input = [left, right];
                let fast = execute(&parsed, &input, false, None, None).unwrap();
                let raw = execute_reference(&reference, &input).unwrap();
                assert_eq!(fast.output, raw.output, "{left}, {right}");
                assert_eq!(fast.output, [0, right.saturating_sub(left), 0, 0]);
                assert_eq!(
                    fast.stats.executed_instructions,
                    raw.stats.executed_instructions
                );
                assert_eq!(
                    fast.stats.executed_rle_instructions,
                    raw.stats.executed_rle_instructions
                );
                assert_eq!(fast.stats.max_pointer, raw.stats.max_pointer);
            }
        }
    }

    #[test]
    fn runtime_guards_leave_fallback_state_untouched() {
        let site = ResolvedProfileSite {
            id: ProfileSiteId(0),
            slot: 0,
        };
        for scratch in 1..=255 {
            for cell in [2, 3] {
                let mut machine = Machine::new(b"", false, None, None, None, None);
                machine.tape[..4].copy_from_slice(&[5, 8, 0, 0]);
                machine.tape[cell] = scratch;
                let before = machine.tape.clone();
                assert!(!machine.execute_compare_loop::<false>(site));
                assert_eq!(machine.tape, before);
                assert_eq!(machine.executed_instructions, 0);
                assert_eq!(machine.pointer, 0);
            }
        }
        // Nonzero scratch can change the pointer path, including underflow.
        let mut source = b"+>>-<<".to_vec();
        source.extend_from_slice(LOOP);
        assert_eq!(
            run_with_stats(&source, b""),
            crate::tests::run_reference(&source, b"")
        );
    }

    #[test]
    fn boundaries_growth_and_skipped_loops_match_reference() {
        for distance in 0..=3 {
            for initial in [0, 1] {
                let mut source = vec![b'>'; TAPE_LEN - 1 - distance];
                source.extend(std::iter::repeat_n(b'+', initial));
                source.extend_from_slice(LOOP);
                let raw = crate::tests::run_reference(&source, b"");
                let fast = run_with_stats(&source, b"");
                match (fast, raw) {
                    (Ok(fast), Ok(raw)) => {
                        assert_eq!(
                            fast.stats.executed_instructions,
                            raw.stats.executed_instructions
                        );
                        assert_eq!(fast.stats.max_pointer, raw.stats.max_pointer);
                    }
                    (Err(fast), Err(raw)) => assert_eq!(fast, raw),
                    other => panic!("boundary mismatch: {other:?}"),
                }
                let grown = run_unbounded_with_stats(&source, b"").unwrap();
                assert_eq!(
                    grown.stats.max_pointer,
                    TAPE_LEN - 1 - distance + 3 * initial
                );
            }
        }
    }

    #[test]
    fn profiles_and_encodings_preserve_logical_counters() {
        for idiom in [LOOP, SLIDE] {
            for (left, right) in [(0, 255), (255, 0), (255, 255), (7, 19), (19, 7)] {
                let mut plain = if idiom == SLIDE {
                    b",>+>>,<<<".to_vec()
                } else {
                    b",>,<".to_vec()
                };
                plain.extend_from_slice(idiom);
                plain.extend_from_slice(b".>.>.>.");
                for header in [b"".as_slice(), b"@BFCRLE1;", b"@BFCRLE2;"] {
                    let mut source = header.to_vec();
                    source.extend_from_slice(&plain);
                    let end = plain.len() as u64;
                    let baseline = assert_matches_reference(&plain, &[left, right]);
                    for mode in [
                        None,
                        Some(ProfileMode::Counters),
                        Some(ProfileMode::Exact),
                        Some(ProfileMode::Sample {
                            interval: Duration::from_millis(1),
                        }),
                    ] {
                        let map = test_profile_map(
                            &source,
                            vec![ProfileRange {
                                start: 0,
                                end,
                                site: ProfileSiteId(1),
                            }],
                        );
                        for disable_remote_transfer in [false, true] {
                            let options = RunOptions {
                                disable_remote_transfer,
                                collect_stats: true,
                                profile: mode.map(|mode| ProfileOptions {
                                    map: map.clone(),
                                    mode,
                                }),
                                ..RunOptions::default()
                            };
                            let result =
                                run_with_options(&source, &[left, right], options.clone()).unwrap();
                            let bypassed = run_with_options(
                                &source,
                                &[left, right],
                                RunOptions {
                                    disable_compare: true,
                                    ..options
                                },
                            )
                            .unwrap();
                            assert_eq!(result.output, baseline.output);
                            assert_eq!(result.stats, baseline.stats);
                            assert_eq!(bypassed.output, result.output);
                            assert_eq!(
                                bypassed.stats.executed_instructions,
                                result.stats.executed_instructions
                            );
                            assert_eq!(
                                bypassed.stats.executed_rle_instructions,
                                result.stats.executed_rle_instructions
                            );
                            assert_eq!(bypassed.stats.max_pointer, result.stats.max_pointer);
                            // Ensure the public option really bypasses the native path.
                            if left != 0 {
                                assert!(
                                    bypassed.stats.optimization.executed_native_operations
                                        > result.stats.optimization.executed_native_operations
                                );
                            }
                            if matches!(mode, Some(ProfileMode::Counters | ProfileMode::Exact)) {
                                let mut counters = result.profile.unwrap().sites[1].counters;
                                let mut bypassed_counters =
                                    bypassed.profile.unwrap().sites[1].counters;
                                assert_eq!(
                                    counters.raw_bf_instructions,
                                    baseline.stats.executed_instructions
                                );
                                assert_eq!(
                                    counters.rle_instructions,
                                    baseline.stats.executed_rle_instructions
                                );
                                assert_eq!(
                                    counters.maximum_pointer_observed,
                                    baseline.stats.max_pointer
                                );
                                counters.fast_operations = 0;
                                bypassed_counters.fast_operations = 0;
                                assert_eq!(counters, bypassed_counters);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nested_profile_counters_match_unfolded_loop() {
        for source in [LOOP, SLIDE] {
            let map = test_profile_map(
                source,
                vec![ProfileRange {
                    start: 0,
                    end: source.len() as u64,
                    site: ProfileSiteId(1),
                }],
            );
            let folded = parse_optimized(source, Some(&map)).unwrap();
            let unfolded = parse_optimized_with_flags(source, Some(&map), true, false).unwrap();
            assert!(matches!(
                folded[0],
                FastInstruction::Loop {
                    optimization: Some(LoopOptimization::Compare | LoopOptimization::CompareSlide),
                    ..
                }
            ));
            assert!(matches!(
                unfolded[0],
                FastInstruction::Loop {
                    optimization: None,
                    ..
                }
            ));
            let options = ProfileOptions {
                map,
                mode: ProfileMode::Counters,
            };
            for (left, right) in [(0, 255), (255, 0), (255, 255), (7, 19), (19, 7)] {
                let mut fast = Machine::new(b"", false, Some(&options), None, None, None);
                let mut slow = Machine::new(b"", false, Some(&options), None, None, None);
                let cells = if source == SLIDE {
                    [left, 1, 0, right]
                } else {
                    [left, right, 0, 0]
                };
                fast.tape[..4].copy_from_slice(&cells);
                slow.tape[..4].copy_from_slice(&cells);
                fast.execute_block(&folded).unwrap();
                slow.execute_block(&unfolded).unwrap();
                assert_eq!(fast.tape, slow.tape);
                let mut fast_counts = fast.profile.unwrap().sites[1].counters;
                let mut slow_counts = slow.profile.unwrap().sites[1].counters;
                fast_counts.fast_operations = 0;
                slow_counts.fast_operations = 0;
                assert_eq!(fast_counts, slow_counts);
            }
        }
    }

    #[test]
    fn profile_boundaries_and_near_misses_are_not_folded() {
        for source in [LOOP, SLIDE] {
            for split in 1..source.len() {
                let map = test_profile_map(
                    source,
                    vec![
                        ProfileRange {
                            start: 0,
                            end: split as u64,
                            site: ProfileSiteId(1),
                        },
                        ProfileRange {
                            start: split as u64,
                            end: source.len() as u64,
                            site: ProfileSiteId(2),
                        },
                    ],
                );
                let parsed = parse_optimized(source, Some(&map)).unwrap();
                assert!(matches!(
                    parsed[0],
                    FastInstruction::Loop {
                        optimization: None,
                        ..
                    }
                ));
            }
        }
        for source in [
            // Same wrapping update, different logical instruction counts.
            b"@BFCRLE1;[>>+257<[-<->>-]>[-<<[-]>>>]<<<]".as_slice(),
            b"[>>>+<[-<->>-]>[-<<[-]>>>]<<<]",
            b"[>>+<[-<->>-]>[-<<[+]>>>]<<<]",
            b"[>>+<[-<->>-]>[-<<[-]>>>]<<<.]",
            b"[>>>>[-<]<<-]",
            b"[>>>[+<]<<-]",
            b"@BFCRLE1;[>>>[-257<]<<-]",
        ] {
            let parsed = parse_optimized(source, None).unwrap();
            assert!(matches!(
                parsed[0],
                FastInstruction::Loop {
                    optimization: None,
                    ..
                }
            ));
        }
        for source in [b"@BFCRLE1;[>3[-<]<2-]".as_slice(), b"@BFCRLE2;[>3[-<]<2-]"] {
            let parsed = parse_optimized(source, None).unwrap();
            assert!(matches!(
                parsed[0],
                FastInstruction::Loop {
                    optimization: Some(LoopOptimization::CompareSlide),
                    ..
                }
            ));
        }
        for source in [
            b"@BFCRLE1;[>2+<[-<->2-]>[-<2[-]>3]<3]".as_slice(),
            b"@BFCRLE2;[>2+<[-<->2-]>[-<2[-]>3]<3]",
        ] {
            let parsed = parse_optimized(source, None).unwrap();
            assert!(matches!(
                parsed[0],
                FastInstruction::Loop {
                    optimization: Some(LoopOptimization::Compare),
                    ..
                }
            ));
        }
    }
}
