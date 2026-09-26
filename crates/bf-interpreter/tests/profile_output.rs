use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "bf-interpreter-profile-output-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove temporary directory");
    }
}

#[test]
fn profile_output_does_not_overwrite_program_or_sidecar() {
    let directory = TemporaryDirectory::new();
    let program = directory.path().join("program.bf");
    let sidecar = directory.path().join("program.map");
    fs::write(&program, b"+").expect("write program");
    fs::write(&sidecar, b"not a profile map").expect("write sidecar");

    for output in [&program, &sidecar] {
        let result = Command::new(env!("CARGO_BIN_EXE_bf-interpreter"))
            .args(["--profile-map"])
            .arg(&sidecar)
            .args(["--profile-output"])
            .arg(output)
            .arg(&program)
            .output()
            .expect("run bf-interpreter");

        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("must not overwrite"));
        assert_eq!(fs::read(&program).expect("read program"), b"+");
        assert_eq!(
            fs::read(&sidecar).expect("read sidecar"),
            b"not a profile map"
        );
    }
}

#[test]
fn profile_output_does_not_overwrite_canonical_input_path() {
    let directory = TemporaryDirectory::new();
    let program = directory.path().join("program.bf");
    let sidecar = directory.path().join("program.map");
    let nested = directory.path().join("nested");
    fs::write(&program, b"+").expect("write program");
    fs::write(&sidecar, b"not a profile map").expect("write sidecar");
    fs::create_dir(&nested).expect("create nested directory");

    let output = nested.join("../program.bf");
    let result = Command::new(env!("CARGO_BIN_EXE_bf-interpreter"))
        .args(["--profile-map"])
        .arg(&sidecar)
        .args(["--profile-output"])
        .arg(&output)
        .arg(&program)
        .output()
        .expect("run bf-interpreter");

    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("must not overwrite"));
    assert_eq!(fs::read(&program).expect("read program"), b"+");
}

#[test]
fn compact_profile_defaults_to_sampling_and_reports_function_sizes() {
    let directory = TemporaryDirectory::new();
    let program = directory.path().join("program.bf");
    let report = directory.path().join("profile.json");
    fs::write(
        &program,
        b"@BFCRLE2;@BFCDBG2;@F0001:6d61696e;@ENDDBG;@C0001;+10.",
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_bf-interpreter"))
        .args([
            "--no-progress",
            "--accept-embedded-profile",
            "--profile-format",
            "json",
            "--profile-output",
        ])
        .arg(&report)
        .arg(&program)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, [16]);
    let report: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["version"], 2);
    assert_eq!(report["artifact"]["hash_kind"], "encoded_source");
    assert_eq!(report["artifact"]["instruction_count"], 17);
    assert_eq!(report["profile"]["sampling_interval_ns"], 1_000_000);
    let sites = report["profile"]["sites"].as_array().unwrap();
    let function = sites.iter().find(|s| s["kind"] == "function").unwrap();
    assert_eq!(function["label"], "main");
    assert_eq!(function["static_bf_instructions"], 0);
    assert_eq!(function["inclusive_static_bf_instructions"], 17);
    let continuation = sites.iter().find(|s| s["kind"] == "continuation").unwrap();
    assert_eq!(continuation["static_bf_instructions"], 17);
    assert_eq!(continuation["parent"], function["id"]);
}
