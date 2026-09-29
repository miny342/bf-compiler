//! Shared CLI subprocess helpers and external-CIR fixtures.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bf_compiler::{
    SelfhostCirContinuation, SelfhostCirFunction, SelfhostCirInstruction, SelfhostCirProgram,
    SelfhostCirReturnType, SelfhostCirTerminator,
};

pub(super) fn run_bfc(root: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bfc"))
        .current_dir(root)
        .arg("--disable-local-control-flow")
        .arg("--disable-function-inline")
        .args(arguments)
        .output()
        .expect("failed to execute bfc")
}

pub(super) fn run_bfc_with_stdin(
    root: &Path,
    arguments: &[&str],
    input: &[u8],
) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bfc"))
        .current_dir(root)
        .arg("--disable-local-control-flow")
        .arg("--disable-function-inline")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to execute bfc");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

pub(super) fn test_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tmp/ir-metrics-cli-regression-{}",
        std::process::id()
    ))
}

pub(super) fn cir_fixture() -> Vec<u8> {
    SelfhostCirProgram::new(
        0,
        0,
        vec![SelfhostCirFunction {
            id: 0,
            entry: 1,
            frame_cells: 1,
            return_type: SelfhostCirReturnType::Void,
            parameters: vec![],
        }],
        vec![SelfhostCirContinuation {
            id: 1,
            function: 0,
            instructions: vec![
                SelfhostCirInstruction::Set {
                    destination: 0,
                    value: b'A',
                },
                SelfhostCirInstruction::Output { source: 0 },
            ],
            terminator: SelfhostCirTerminator::Halt,
        }],
    )
    .unwrap()
    .encode()
    .unwrap()
}
