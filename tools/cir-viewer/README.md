# CIR inspection viewer

This is a small, browser-only viewer for the self-host `BFCIR` binary format and
the Rust compiler's serialized `ContinuationProgram` format.
It intentionally does not run the compiler or interpreter and does not upload dropped files.

## Run from WSL

```text
npm ci
npm run dev -- --host
```

Open the printed address from a Windows browser. For a static build:

```text
npm run build
npm run preview -- --host
```

Drop a binary self-host `.cir` or a Rust internal `.cir` first. `bfmap.json`, IR
metrics JSON, and `.bfc` source files can then be dropped into the same session.
The viewer shows a function call graph, a selected function's continuation graph,
and frame-relative pseudo-code. Rust internal CIR carries function names directly;
binary self-host CIR can use names from a metrics sidecar when their numeric
function IDs exist in the loaded CIR.

Generate the Rust internal artifact with:

```text
target/release/bfc --cir-output rust-internal.cir stage2-compiler.bfc
```

The Rust internal artifact is JSON with a `.cir` extension so the viewer can
load the complete optimized `ContinuationProgram`; it is distinct from the
executable binary self-host `BFCIR-1` format.

Dependencies are pinned in `package.json` and `package-lock.json`. Installation scripts are not required;
use `npm ci --ignore-scripts` when reviewing or reproducing the dependency installation.
