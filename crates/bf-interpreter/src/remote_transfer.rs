//! Conservative dynamic linearization of scan-bearing transfer loops.
//!
//! No compiler ABI or profile key is trusted. A read-only probe must return to
//! the source, and every scanned guard (including zero endpoints) must be
//! disjoint from every written cell. Thus the route is invariant over all
//! iterations. Failure leaves all BF state untouched and uses the original loop.
use super::*;

const MAX_BODY: usize = 32;

pub(super) fn recognize(body: &[FastInstruction]) -> bool {
    if body.len() > MAX_BODY
        || !matches!(
            body.first(),
            Some(FastInstruction::Add {
                amount: 1 | 255,
                ..
            })
        )
    {
        return false;
    }
    let mut scans = 0;
    for instruction in body {
        match instruction {
            FastInstruction::Add { .. } | FastInstruction::Move { .. } => {}
            FastInstruction::Loop {
                optimization: Some(LoopOptimization::Scan { amount, .. }),
                ..
            } if *amount != 0 => scans += 1,
            _ => return false,
        }
    }
    scans != 0
}

#[derive(Clone, Copy, Default)]
struct ScanSpan {
    start: usize,
    end: usize,
    stride: usize,
}
impl ScanSpan {
    fn contains(self, cell: usize) -> bool {
        cell >= self.start.min(self.end)
            && cell <= self.start.max(self.end)
            && cell.abs_diff(self.start).is_multiple_of(self.stride)
    }
}

struct Probe {
    updates: [(usize, u8); MAX_BODY],
    updates_len: usize,
    scans: [ScanSpan; MAX_BODY],
    scans_len: usize,
    raw: u64,
    rle: u64,
    distance: u64,
    scan_steps: u64,
    maximum: usize,
}

impl Machine<'_> {
    fn probe_remote_transfer(
        &mut self,
        site: ResolvedProfileSite,
        body: &[FastInstruction],
    ) -> Result<Option<Probe>, Error> {
        let mut probe = Probe {
            updates: [(0, 0); MAX_BODY],
            updates_len: 0,
            scans: [ScanSpan::default(); MAX_BODY],
            scans_len: 0,
            raw: 0,
            rle: 0,
            distance: 0,
            scan_steps: 0,
            maximum: self.pointer,
        };
        let mut pointer = self.pointer;
        for instruction in body {
            match instruction {
                FastInstruction::Add {
                    amount, raw_count, ..
                } => {
                    // Keep even cancelling writes: an intermediate change to a
                    // scanned guard would invalidate route independence.
                    probe.updates[probe.updates_len] = (pointer, *amount);
                    probe.updates_len += 1;
                    let Some(raw) = probe.raw.checked_add(*raw_count) else {
                        return Ok(None);
                    };
                    let Some(rle) = probe.rle.checked_add(1) else {
                        return Ok(None);
                    };
                    probe.raw = raw;
                    probe.rle = rle;
                }
                FastInstruction::Move {
                    amount,
                    source_offsets,
                    ..
                } => {
                    let Some(next) = pointer
                        .checked_add_signed(*amount)
                        .filter(|p| *p < self.tape.len())
                    else {
                        return Ok(None);
                    };
                    pointer = next;
                    probe.maximum = probe.maximum.max(pointer);
                    let Some(raw) = probe.raw.checked_add(source_offsets.len() as u64) else {
                        return Ok(None);
                    };
                    let Some(rle) = probe.rle.checked_add(1) else {
                        return Ok(None);
                    };
                    let Some(distance) = probe.distance.checked_add(source_offsets.len() as u64)
                    else {
                        return Ok(None);
                    };
                    probe.raw = raw;
                    probe.rle = rle;
                    probe.distance = distance;
                }
                FastInstruction::Loop {
                    optimization:
                        Some(LoopOptimization::Scan {
                            amount,
                            source_offsets,
                        }),
                    ..
                } => {
                    let start = pointer;
                    let mut steps = 0u64;
                    // Probe allocated cells only. Growth/boundary errors use
                    // normal execution, preserving exact physical error offsets.
                    while self.tape[pointer] != 0 {
                        if steps.is_multiple_of(1024) {
                            self.observe_progress(site)?;
                        }
                        let Some(next) = pointer
                            .checked_add_signed(*amount)
                            .filter(|p| *p < self.tape.len())
                        else {
                            return Ok(None);
                        };
                        pointer = next;
                        steps += 1;
                    }
                    probe.maximum = probe.maximum.max(start.max(pointer));
                    probe.scans[probe.scans_len] = ScanSpan {
                        start,
                        end: pointer,
                        stride: amount.unsigned_abs(),
                    };
                    probe.scans_len += 1;
                    probe.scan_steps += steps;
                    let Some(distance) = steps.checked_mul(source_offsets.len() as u64) else {
                        return Ok(None);
                    };
                    let Some(raw) = probe
                        .raw
                        .checked_add(1)
                        .and_then(|n| n.checked_add(distance))
                        .and_then(|n| n.checked_add(steps))
                    else {
                        return Ok(None);
                    };
                    let Some(rle) = probe
                        .rle
                        .checked_add(1)
                        .and_then(|n| n.checked_add(steps.checked_mul(2)?))
                    else {
                        return Ok(None);
                    };
                    probe.raw = raw;
                    probe.rle = rle;
                    let Some(distance) = probe.distance.checked_add(distance) else {
                        return Ok(None);
                    };
                    probe.distance = distance;
                }
                _ => unreachable!("recognized remote transfer body"),
            }
        }
        if pointer != self.pointer {
            return Ok(None);
        }
        for &(cell, amount) in &probe.updates[..probe.updates_len] {
            if amount != 0
                && probe.scans[..probe.scans_len]
                    .iter()
                    .any(|span| span.contains(cell))
            {
                return Ok(None);
            }
        }
        Ok(Some(probe))
    }

    pub(super) fn execute_remote_transfer<const PROFILE: bool>(
        &mut self,
        site: ResolvedProfileSite,
        body: &[FastInstruction],
    ) -> Result<bool, Error> {
        let initial = self.tape[self.pointer];
        if initial == 0 {
            self.optimization.remote_transfer_loops += 1;
            if PROFILE {
                self.add_counts(site, 1, 1);
                self.profile
                    .as_mut()
                    .unwrap()
                    .site_mut(site)
                    .counters
                    .remote_transfer_loops += 1;
            } else {
                self.add_counts_unprofiled(1, 1);
            }
            return Ok(true);
        }
        let Some(probe) = self.probe_remote_transfer(site, body)? else {
            return Ok(self.remote_transfer_fallback::<PROFILE>(site));
        };
        let delta = probe.updates[..probe.updates_len]
            .iter()
            .filter(|(cell, _)| *cell == self.pointer)
            .fold(0u8, |sum, (_, n)| sum.wrapping_add(*n));
        if !matches!(delta, 1 | 255) {
            return Ok(self.remote_transfer_fallback::<PROFILE>(site));
        }
        let iterations = if delta == 255 {
            initial as u64
        } else {
            0u8.wrapping_sub(initial) as u64
        };
        let Some(raw) = probe
            .raw
            .checked_add(1)
            .and_then(|n| n.checked_mul(iterations))
            .and_then(|n| n.checked_add(1))
        else {
            return Ok(self.remote_transfer_fallback::<PROFILE>(site));
        };
        let Some(rle) = probe
            .rle
            .checked_add(1)
            .and_then(|n| n.checked_mul(iterations))
            .and_then(|n| n.checked_add(1))
        else {
            return Ok(self.remote_transfer_fallback::<PROFILE>(site));
        };
        let Some(distance) = probe.distance.checked_mul(iterations) else {
            return Ok(self.remote_transfer_fallback::<PROFILE>(site));
        };
        for &(cell, amount) in &probe.updates[..probe.updates_len] {
            self.tape[cell] = self.tape[cell].wrapping_add(amount.wrapping_mul(iterations as u8));
        }
        debug_assert_eq!(self.tape[self.pointer], 0);
        self.max_pointer = self.max_pointer.max(probe.maximum);
        self.optimization.remote_transfer_loops += 1;
        self.optimization.remote_transfer_iterations += iterations;
        if PROFILE {
            self.add_counts(site, raw, rle);
            let counters = &mut self.profile.as_mut().unwrap().site_mut(site).counters;
            counters.remote_transfer_loops += 1;
            counters.remote_transfer_iterations += iterations;
            counters.loop_entries += iterations * probe.scans_len as u64;
            counters.loop_iterations += iterations * (1 + probe.scan_steps);
            counters.pointer_distance += distance;
            self.record_maximum_pointer_at(site, probe.maximum);
        } else {
            self.add_counts_unprofiled(raw, rle);
        }
        Ok(true)
    }

    fn remote_transfer_fallback<const PROFILE: bool>(&mut self, site: ResolvedProfileSite) -> bool {
        self.optimization.remote_transfer_fallbacks += 1;
        if PROFILE {
            self.profile
                .as_mut()
                .unwrap()
                .site_mut(site)
                .counters
                .remote_transfer_fallbacks += 1;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Machine<'static> {
        Machine::new(b"", false, None, None, None, None)
    }

    fn compare(body: &[u8], tape: &[u8], origin: usize, expect_fast: bool) {
        let optimized = parse_optimized_with_flags(body, None, true, true).unwrap();
        let baseline = parse_optimized_with_flags(body, None, false, true).unwrap();
        let mut a = machine();
        let mut b = machine();
        a.tape[..tape.len()].copy_from_slice(tape);
        b.tape[..tape.len()].copy_from_slice(tape);
        a.pointer = origin;
        b.pointer = origin;
        a.max_pointer = origin;
        b.max_pointer = origin;
        let actual = a.execute_block(&optimized);
        let expected = b.execute_block(&baseline);
        assert_eq!(actual, expected);
        assert_eq!(a.tape, b.tape);
        assert_eq!(a.pointer, b.pointer);
        assert_eq!(a.max_pointer, b.max_pointer);
        if actual.is_ok() {
            assert_eq!(a.executed_instructions, b.executed_instructions);
            assert_eq!(a.executed_rle_instructions, b.executed_rle_instructions);
        }
        if expect_fast {
            assert_eq!(a.optimization.remote_transfer_loops, 1);
        } else {
            assert_eq!(a.optimization.remote_transfer_fallbacks, 1);
        }
    }

    #[test]
    fn both_directions_all_byte_values_multiple_destinations_and_zero_length_scans() {
        for initial in 0..=255 {
            for length in [0, 1, 3, 17] {
                let mut tape = vec![0; 2 * length + 6];
                tape[1] = initial;
                for i in 1..=length {
                    tape[2 * i] = 19;
                }
                tape[2 * length + 3] = 251;
                for source in [
                    b"[->[>>]>+<<<[<<]>]".as_slice(),
                    b"[+>[>>]>-----<<<[<<]>]",
                    b"[->[>>]>+>++<<<<[<<]>]",
                    b"@BFCRLE1;[->+256[>2]>+16<3[<2]>]",
                ] {
                    compare(source, &tape, 1, true);
                }
                let mut reverse = vec![0; 2 * length + 6];
                reverse[2 * length + 3] = initial;
                reverse[1] = 251;
                for i in 1..=length {
                    reverse[2 + 2 * i] = 19;
                }
                compare(b"[-<[<<]<+>>>[>>]<]", &reverse, 2 * length + 3, true);
            }
        }
    }

    #[test]
    fn unsafe_routes_and_boundary_errors_fall_back_without_mutating_probe_state() {
        // Source is a scan guard; its decrement changes where the scan stops.
        compare(b"[-[>]<<<]", &[0, 0, 0, 1, 1, 0], 3, false);
        // The destination is the forward scan's zero endpoint.
        compare(b"[->[>]+<<[<]>]", &[0, 2, 1, 0, 0], 1, false);
        // The body ends at a different zero cell instead of the original guard.
        compare(b"[->[>]>+]", &[0, 1, 1, 0, 255], 1, false);
        // Exact physical source offsets must survive both RLE and raw fallback.
        compare(b"[->>[<]<]", &[1, 0, 0], 0, false);
        compare(b"@BFCRLE1;[->2[<]<]", &[1, 0, 0], 0, false);
        // Probe never grows memory. Normal execution may grow successfully.
        let source = b"@BFCRLE1;[->30000[>]+<30000]";
        let optimized = parse_optimized_with_flags(source, None, true, true).unwrap();
        let mut m = machine();
        m.grow_tape = true;
        m.tape[0] = 1;
        m.execute_block(&optimized).unwrap();
        assert_eq!(m.tape[30000], 1);
        assert_eq!(m.pointer, 0);
        assert_eq!(m.optimization.remote_transfer_fallbacks, 1);
        assert_eq!(m.max_pointer, 30000);
    }

    #[test]
    fn a_failed_probe_preserves_tape_pointer_and_maximum() {
        let parsed = parse_optimized(b"[->[>]+<<[<]>]", None).unwrap();
        let [FastInstruction::Loop { body, .. }] = parsed.as_slice() else {
            panic!()
        };
        let mut m = machine();
        m.pointer = 1;
        m.tape[1] = 2;
        m.tape[2] = 1;
        let before = m.tape.clone();
        assert!(
            m.probe_remote_transfer(ResolvedProfileSite::ROOT, body)
                .unwrap()
                .is_none()
        );
        assert_eq!(m.tape, before);
        assert_eq!(m.pointer, 1);
        assert_eq!(m.max_pointer, 0);
        assert_eq!(m.executed_instructions, 0);
    }

    #[test]
    fn probe_observes_interrupts_without_committing_partial_state() {
        let parsed = parse_optimized(b"[->[>>]>+<<<[<<]>]", None).unwrap();
        let [FastInstruction::Loop { body, .. }] = parsed.as_slice() else {
            panic!()
        };
        let mut m = machine();
        m.pointer = 1;
        m.tape[1] = 255;
        m.tape[2] = 1;
        m.progress_countdown = 1;
        m.progress = Some(ProgressOptions {
            interval: None,
            interrupted: || true,
            callback: Arc::new(|_| {}),
        });
        assert!(matches!(
            m.probe_remote_transfer(ResolvedProfileSite::ROOT, body),
            Err(Error::Interrupted)
        ));
        assert_eq!(m.pointer, 1);
        assert_eq!(m.tape[1], 255);
        assert_eq!(m.executed_instructions, 0);
    }
}
