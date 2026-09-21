# CIR inspection viewer

This is a small, browser-only viewer for the self-host `BFCIR` binary format.
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

Drop a `.cir` file first. `bfmap.json`, IR metrics JSON, and `.bfc` source files can then be dropped into
the same session. The first version shows a function call graph, a selected function's continuation graph,
and frame-relative pseudo-code. Names from a metrics sidecar are used only when their numeric function IDs
exist in the loaded CIR.

Dependencies are pinned in `package.json` and `package-lock.json`. Installation scripts are not required;
use `npm ci --ignore-scripts` when reviewing or reproducing the dependency installation.
