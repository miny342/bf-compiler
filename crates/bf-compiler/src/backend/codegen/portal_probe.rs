//! Export a compact fixture without exposing benchmark knobs in the public ABI.
use super::*;
use std::{fs, io::Write, path::Path};

fn transport_fixture(padding: usize, nibble_transfer: bool) -> (CompiledProfileArtifact, usize) {
    // Keep each actual frame within the ABI limit. Additional live flag
    // chunks model suspended callers, not one oversized activation.
    let frame_padding = padding.min(256);
    let extra_chunks = (padding - frame_padding) / crate::DEFAULT_CHUNK_CELLS;
    let declaration = format!("cell[{frame_padding}] keep;");
    let first = "keep[0]";
    let last = format!("keep[{}]", frame_padding - 1);
    let source = format!(
        "cell[16] data; void main() {{ {declaration} \
         {first} = input(); {last} = input(); \
         cell i = input(); data[i] = input(); output(data[i]); \
         output({first}); output({last}); }}"
    );
    let program = crate::lower_source(&source).unwrap();
    let config = AbiConfig::default();
    let layouts = build_layouts(&program, config).unwrap();
    let static_layout = StaticLayout::new(config, program.globals()).unwrap();
    let portal = PortalPlan::new(&program).unwrap();
    let mut emitter = AbiEmitter::new(
        &program,
        &layouts,
        &static_layout,
        &portal,
        config,
        ProfileGranularity::Continuation,
    );
    emitter.nibble_transfer = nibble_transfer;
    emitter.initialize_main(false).unwrap();
    emitter.with_profile_site_infallible(
        "fixture",
        "fixture.stack",
        "live caller chunks",
        |emitter| {
            for chunk in 1..=extra_chunks {
                emitter.set((chunk * config.stride()) as isize, 1);
            }
            emitter.migrate_context((extra_chunks * config.stride()) as isize);
        },
    );
    let condition = emitter.current_abi_offset(AbiField::Active).unwrap();
    emitter.move_to(condition);
    emitter.emit_operation(AnnotatedBfOperation::Input);
    let body = emitter
        .capture(|emitter| {
            for route in 0..7 {
                let Location::Relative(offset) = emitter.route_location(route)? else {
                    unreachable!()
                };
                emitter.move_to(offset);
                emitter.emit_operation(AnnotatedBfOperation::Input);
            }
            emitter.move_to(0);
            emitter.with_profile_site(
                "abi",
                "abi.portal.router.global.0",
                "global portal router",
                |emitter| {
                    emitter.move_global_portal_request(AggregateRegion::Global(GlobalId::new(0)))
                },
            )?;
            // Observe every transferred request byte, then restore the zero-prefix
            // contract. No dispatch interprets these controlled values as real PCs.
            emitter.with_profile_site(
                "fixture",
                "fixture.check",
                "observe request and cleanup",
                |emitter| {
                    for field in [
                        AbiField::Index,
                        AbiField::Scratch0,
                        AbiField::Value,
                        AbiField::NextPcLow,
                        AbiField::NextPcHigh,
                        AbiField::ReturnPcLow,
                        AbiField::ReturnPcHigh,
                    ] {
                        let location = emitter.portal_field_location(
                            AggregateRegion::Global(GlobalId::new(0)),
                            field,
                            program.main(),
                        )?;
                        emitter.move_context_to_location(location);
                        emitter.emit_operation(AnnotatedBfOperation::Output);
                        emitter.clear_current();
                        emitter.move_location_to_context(location);
                    }
                    emitter.move_to(condition);
                    emitter.emit_operation(AnnotatedBfOperation::Input);
                    Ok(())
                },
            )
        })
        .unwrap();
    emitter.emit_loop(body);
    let annotated = AnnotatedBfProgram::new(emitter.output, emitter.sites);
    let mut plain = Vec::new();
    optimize_bf(&annotated.clone().into_plain())
        .write_compressed_source(&mut plain)
        .unwrap();
    let artifact = optimize_annotated_bf(&annotated).profile_artifact(true);
    assert_eq!(
        artifact.source.as_bytes(),
        plain,
        "profile labels must not change optimized BF"
    );
    (
        artifact,
        layouts[&program.main()].frame.frame_chunks() + extra_chunks,
    )
}

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

#[test]
#[ignore = "exports benchmark artifacts only when explicitly requested"]
fn export_portal_transport_fixtures() {
    let root =
        std::env::var_os("BFC_PORTAL_PROBE_OUTPUT").expect("BFC_PORTAL_PROBE_OUTPUT required");
    let root = Path::new(&root).canonicalize().unwrap();
    let tmp = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp")
        .canonicalize()
        .unwrap();
    assert!(
        root.starts_with(tmp),
        "artifacts must be under repository tmp"
    );
    let nibble_transfer = match std::env::var("BFC_PORTAL_NIBBLE_TRANSFER").as_deref() {
        Ok("1") => true,
        Err(std::env::VarError::NotPresent) | Ok("0") => false,
        _ => panic!("BFC_PORTAL_NIBBLE_TRANSFER must be 0 or 1"),
    };
    for padding in [16, 256, 65280] {
        let (artifact, chunks) = transport_fixture(padding, nibble_transfer);
        for (suffix, contents) in [
            ("bf", artifact.source),
            ("bfmap.json", artifact.map.to_json_pretty().unwrap()),
            (
                "fixture.json",
                serde_json::json!({"kind":"transport", "padding_cells":padding,
                "stack_chunks":chunks, "request_bytes":7, "nibble_transfer":nibble_transfer,
                "note":"controlled request bytes; no dispatcher or accessor"})
                .to_string(),
            ),
        ] {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join(format!("transport-{padding}.{suffix}")))
                .unwrap();
            file.write_all(contents.as_bytes()).unwrap();
        }
    }
}
