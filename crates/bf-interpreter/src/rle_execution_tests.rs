use super::*;
use crate::tests::{run_reference, test_profile_map};
use bf_profiling::ProfileRange;
use std::sync::Mutex;

fn rle_only() -> Optimizations {
    Optimizations::from(&RunOptions {
        disable_clear: true,
        disable_scan: true,
        disable_transfer: true,
        disable_countdown: true,
        disable_compare: true,
        disable_remote_transfer: true,
        ..RunOptions::default()
    })
}

#[test]
fn compact_matches_tree_for_dynamic_guards_io_and_boundary_errors() {
    for source in [
        &b",[>+<-]>."[..],
        b"+[>]+.",
        b">++[<+>->].<.<.",
        b",[[->+<]>[-<+>]<-]>.",
        b"[,].",
        b"[]+.",
        b"++[>.>-<.<-]",
        b">>>>xyz<<<<<<<",
        b"@BFCRLE1;>30001+.<30001.",
        b"@BFCRLE2;>7531+.<7531.",
        b"@BFCRLE1;>1000000000",
        b"@BFCRLE2;<100000000",
    ] {
        let compact = parse_optimized_with_options(source, None, rle_only()).unwrap();
        assert!(compact.compact_rle.is_some());
        let mut tree = compact.clone();
        tree.compact_rle = None;
        for input in [&b""[..], &[0][..], &[1][..], &[7, 4, 0][..], &[255][..]] {
            for grow in [false, true] {
                // Do not allocate a billion cells in unbounded mode.
                if grow && source.ends_with(b"1000000000") {
                    continue;
                }
                assert_eq!(
                    execute(&compact, input, grow, None, None),
                    execute(&tree, input, grow, None, None),
                    "source={source:?} input={input:?} grow={grow}"
                );
            }
        }
    }
}

#[test]
fn compact_executes_deep_nesting_without_recursive_vm_calls() {
    let depth = 8192;
    let mut source = vec![b'+'];
    source.extend(std::iter::repeat_n(b'[', depth));
    source.push(b'-');
    source.extend(std::iter::repeat_n(b']', depth));
    let program = parse_optimized_with_options(&source, None, rle_only()).unwrap();
    let actual = execute(&program, b"", false, None, None).unwrap();
    let expected = run_reference(&source, b"").unwrap();
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
    assert_eq!(
        actual.stats.optimization.executed_native_operations,
        depth as u64 + 2
    );
}

fn interrupted() -> bool {
    true
}

#[test]
fn compact_flushes_exact_counters_before_interrupt_snapshot() {
    let source = b"+[>+.<-]".repeat(PROGRESS_POLL_OPERATIONS as usize);
    let compact = parse_optimized_with_options(&source, None, rle_only()).unwrap();
    let mut tree = compact.clone();
    tree.compact_rle = None;
    let mut snapshots = Vec::new();
    for program in [&compact, &tree] {
        let saved = Arc::new(Mutex::new(None));
        let callback_saved = Arc::clone(&saved);
        let progress = ProgressOptions {
            interval: None,
            interrupted,
            callback: Arc::new(move |snapshot| {
                *callback_saved.lock().unwrap() = Some(snapshot.clone())
            }),
        };
        assert_eq!(
            execute(program, b"", false, None, Some(&progress)),
            Err(Error::Interrupted)
        );
        let mut snapshot = saved.lock().unwrap().take().unwrap();
        snapshot.elapsed = Duration::ZERO;
        snapshots.push(snapshot);
    }
    assert_eq!(snapshots[0], snapshots[1]);
    assert_eq!(
        snapshots[0].stats.optimization.executed_native_operations,
        u64::from(PROGRESS_POLL_OPERATIONS - 1)
    );
}

#[test]
fn batched_plain_moves_preserve_offsets_across_profile_splits() {
    let source = b">>>>xyz>>>>++++++++.<<<<....";
    let map = test_profile_map(
        source,
        vec![
            ProfileRange {
                start: 0,
                end: 2,
                site: ProfileSiteId(1),
            },
            ProfileRange {
                start: 2,
                end: 5,
                site: ProfileSiteId(2),
            },
            ProfileRange {
                start: 5,
                end: 9,
                site: ProfileSiteId(1),
            },
            ProfileRange {
                start: 9,
                end: 13,
                site: ProfileSiteId(2),
            },
            ProfileRange {
                start: 13,
                end: bf_profiling::bf_identity(source).instruction_count,
                site: ProfileSiteId(1),
            },
        ],
    );
    let program = parse_optimized_with_options(source, Some(&map), rle_only()).unwrap();
    assert!(program.compact_rle.is_none());
    let FastInstruction::Move {
        source_offsets,
        amount,
        ..
    } = &program[0]
    else {
        panic!("expected pointer run")
    };
    assert_eq!(*amount, 8);
    assert_eq!(source_offsets.range_count(), 2);
    assert_eq!(
        (0..8).map(|i| source_offsets.get(i)).collect::<Vec<_>>(),
        [0, 1, 2, 3, 7, 8, 9, 10]
    );
    let plain = parse_optimized_with_options(source, None, rle_only()).unwrap();
    let expected = execute(&plain, b"", false, None, None).unwrap();
    for mode in [ProfileMode::Counters, ProfileMode::Exact] {
        let actual = execute(
            &program,
            b"",
            false,
            Some(&ProfileOptions {
                map: map.clone(),
                mode,
            }),
            None,
        )
        .unwrap();
        assert_eq!(actual.output, expected.output);
        assert_eq!(actual.stats, expected.stats);
        let profile = actual.profile.unwrap();
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|s| s.counters.raw_bf_instructions)
                .sum::<u64>(),
            actual.stats.executed_instructions
        );
        assert_eq!(
            profile
                .sites
                .iter()
                .map(|s| s.counters.rle_instructions)
                .sum::<u64>(),
            actual.stats.executed_rle_instructions
        );
    }
}
