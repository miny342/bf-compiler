//! Regression coverage for portal request transport and profile counters.
use super::*;

#[test]
fn request_transport_preserves_every_byte_and_profile_structure() {
    for (padding, nibble_transfer) in [(16, false), (16, true), (256, false), (256, true)] {
        let (artifact, _) = transport_fixture(padding, nibble_transfer);
        let mut input = Vec::new();
        let mut expected = Vec::new();
        for value in 0..=255u8 {
            input.push(1);
            for field in 0..7u8 {
                let byte = value.wrapping_add(field.wrapping_mul(31));
                input.push(byte);
                expected.push(byte);
            }
        }
        input.push(0);
        assert_eq!(
            bf_interpreter::run(artifact.source.as_bytes(), &input).unwrap(),
            expected
        );
        for disabled in [false, true] {
            let run = bf_interpreter::run_with_options(
                artifact.source.as_bytes(),
                &input,
                bf_interpreter::RunOptions {
                    disable_remote_transfer: disabled,
                    collect_stats: true,
                    profile: Some(bf_interpreter::ProfileOptions {
                        map: artifact.map.clone(),
                        mode: bf_interpreter::ProfileMode::Counters,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(run.output, expected);
            let profile = run.profile.unwrap();
            let transport_loops: u64 = profile
                .sites
                .iter()
                .filter(|site| {
                    artifact
                        .map
                        .sites
                        .iter()
                        .find(|record| record.id == site.site)
                        .unwrap()
                        .stable_key
                        .starts_with("abi.portal.route.transport.")
                })
                .map(|site| site.counters.remote_transfer_loops)
                .sum();
            assert_eq!(
                transport_loops,
                if disabled {
                    0
                } else {
                    256 * if nibble_transfer { 10 } else { 7 }
                }
            );
            assert_eq!(run.stats.optimization.remote_transfer_fallbacks, 0);
        }
        let json = artifact.map.to_json_pretty().unwrap();
        for key in [
            "abi.portal.route.decompose",
            "abi.portal.route.pack",
            "abi.portal.route.transport.nibble.1",
            "abi.portal.route.transport.unary",
        ] {
            assert_eq!(
                json.contains(key),
                nibble_transfer || key.ends_with("unary")
            );
        }
    }
}
