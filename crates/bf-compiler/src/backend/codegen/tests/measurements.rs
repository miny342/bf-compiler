//! Explicit artifact export for scripts/portal-profile; excluded from normal runs.
use super::support::transport_fixture;
use std::{fs, io::Write, path::Path};

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
