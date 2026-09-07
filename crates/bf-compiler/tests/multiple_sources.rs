use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use bf_interpreter::run;

static NEXT_TEMP_FILE: AtomicUsize = AtomicUsize::new(0);

struct TempSource(PathBuf);

impl TempSource {
    fn new(name: &str, source: &str) -> Self {
        let serial = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bfc-multiple-sources-{}-{serial}-{name}",
            std::process::id()
        ));
        fs::write(&path, source).unwrap();
        Self(path)
    }
}

impl Drop for TempSource {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn cli_compiles_multiple_sources_in_argument_order() {
    let helper = TempSource::new("helper.bfc", "cell next(cell value) { return value + 1; }");
    let main = TempSource::new("main.bfc", "void main() { output(next('A')); }");

    let output = Command::new(env!("CARGO_BIN_EXE_bfc"))
        .args([&helper.0, &main.0])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bfc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run(&output.stdout, b"").unwrap(), b"B");
    assert!(
        output.stderr.is_empty(),
        "successful compilation emitted diagnostics: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_diagnostic_names_the_source_containing_the_error() {
    let helper = TempSource::new("helper.bfc", "cell helper() { return 1; }");
    let main = TempSource::new("main.bfc", "void main() {\n    output(missing);\n}\n");

    let output = Command::new(env!("CARGO_BIN_EXE_bfc"))
        .args([&helper.0, &main.0])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&format!("{}:2:12:", main.0.display())));
    assert!(stderr.contains("undefined variable \"missing\""));
}
