import { useCallback, useEffect, useMemo, useRef, useState, type DragEvent } from "react";
import { createRoot } from "react-dom/client";
import {
  Background,
  Controls,
  MarkerType,
  MiniMap,
  ReactFlow,
  ReactFlowProvider,
  applyNodeChanges,
  useReactFlow,
  type Edge,
  type Node,
  type NodeChange,
  type NodeMouseHandler,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import "./style.css";
import {
  callEdges,
  functionName,
  parseCir,
  type CirContinuation,
  type CirFunction,
  type CirProgram,
} from "./cir";
import { inspectFile, mergeNames, type SidecarInfo } from "./metadata";

type GraphMode = "call" | "cfg";
type CallScope = "neighborhood" | "all";
type GraphNode = Node<{ label: string; kind: "function" | "continuation"; hot?: number }>;

export default function App() {
  const [program, setProgram] = useState<CirProgram>();
  const [sidecars, setSidecars] = useState<SidecarInfo[]>([]);
  const [selectedFunction, setSelectedFunction] = useState<number>();
  const [mode, setMode] = useState<GraphMode>("call");
  const [callScope, setCallScope] = useState<CallScope>("neighborhood");
  const [warnings, setWarnings] = useState<string[]>([]);
  const [dropMessage, setDropMessage] = useState("Drop a .cir file here");
  const inputRef = useRef<HTMLInputElement>(null);

  const names = useMemo(() => mergeNames(sidecars, program ?? emptyProgram()).names, [program, sidecars]);
  const mergedWarnings = useMemo(() => {
    if (!program) return warnings;
    return [...warnings, ...mergeNames(sidecars, program).warnings];
  }, [program, sidecars, warnings]);
  const selected = program?.functions.find((fn) => fn.id === selectedFunction) ?? program?.functions[0];

  async function loadFiles(files: FileList | File[]): Promise<void> {
    const incoming = [...files];
    if (incoming.length === 0) return;
    const nextWarnings: string[] = [];
    let nextProgram = program;
    let cirLoaded = false;
    const nextSidecars: SidecarInfo[] = [];
    for (const file of incoming) {
      const bytes = new Uint8Array(await file.arrayBuffer());
      if (file.name.toLowerCase().endsWith(".cir") || looksLikeMagic(bytes)) {
        try {
          nextProgram = parseCir(bytes);
          cirLoaded = true;
          setSelectedFunction(nextProgram.mainFunction);
          setMode("call");
          setDropMessage(`${file.name}: loaded`);
        } catch (error) {
          nextWarnings.push(`${file.name}: ${messageOf(error)}`);
        }
      } else {
        nextSidecars.push(await inspectFile(file));
      }
    }
    if (cirLoaded) setProgram(nextProgram);
    if (nextSidecars.length > 0) setSidecars((current) => [...current, ...nextSidecars]);
    setWarnings(nextWarnings);
  }

  function onDrop(event: DragEvent<HTMLDivElement>): void {
    event.preventDefault();
    void loadFiles(event.dataTransfer.files);
  }

  function reset(): void {
    setProgram(undefined);
    setSidecars([]);
    setWarnings([]);
    setSelectedFunction(undefined);
    setDropMessage("Drop a .cir file here");
  }

  function downloadInspection(): void {
    if (!program) return;
    const blob = new Blob([JSON.stringify(program, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "inspection-program.json";
    anchor.click();
    URL.revokeObjectURL(url);
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div>
          <div className="eyebrow">BFCompiler / CIR</div>
          <h1>Continuation IR inspection</h1>
        </div>
        <div className="top-actions">
          <button type="button" onClick={downloadInspection} disabled={!program}>Download JSON</button>
          <button type="button" className="secondary" onClick={reset}>Reset</button>
        </div>
      </header>

      <section
        className={`drop-zone ${program ? "has-program" : ""}`}
        onDragOver={(event) => event.preventDefault()}
        onDrop={onDrop}
        onClick={() => inputRef.current?.click()}
      >
        <input
          ref={inputRef}
          type="file"
          multiple
          accept=".cir,.json,.bfc,.txt"
          onChange={(event) => {
            if (event.target.files) void loadFiles(event.target.files);
          }}
          hidden
        />
        <strong>{dropMessage}</strong>
        <span>Drop .cir, bfmap.json, ir-metrics.json, or source files. Everything stays in this browser.</span>
      </section>

      {mergedWarnings.length > 0 && (
        <section className="warnings">
          {mergedWarnings.map((warning, index) => <div key={`${warning}-${index}`}>⚠ {warning}</div>)}
        </section>
      )}

      {!program ? (
        <section className="empty-state">
          <h2>Start with a self-host CIR artifact</h2>
          <p>The viewer reads the BFCIR binary directly. No compiler or interpreter is invoked.</p>
        </section>
      ) : (
        <section className="workspace">
          <aside className="sidebar">
            <div className="panel-heading">
              <span>Functions</span>
              <small>{program.functions.length}</small>
            </div>
            <div className="program-facts">
              <span>format {program.format}</span>
              <span>static cells {program.staticCells.toLocaleString()}</span>
              <span>main fn#{program.mainFunction}</span>
              <span>{program.continuations.length} continuations</span>
            </div>
            <div className="function-list">
              {program.functions.map((fn) => (
                <button
                  type="button"
                  className={`function-row ${selected?.id === fn.id ? "selected" : ""}`}
                  key={fn.id}
                  onClick={() => {
                    setSelectedFunction(fn.id);
                    setMode("cfg");
                  }}
                >
                  <span className="function-id">fn#{fn.id}</span>
                  <span className="function-name">{functionName(fn.id, names)}</span>
                  <span className="function-meta">entry c{fn.entry} · {continuationCount(program, fn.id)} cont</span>
                </button>
              ))}
            </div>
            <div className="sidecar-list">
              <div className="panel-heading"><span>Session files</span><small>{sidecars.length + 1}</small></div>
              <div className="file-row">✓ CIR artifact</div>
              {sidecars.map((info) => <div className="file-row" title={info.message} key={`${info.fileName}-${info.message}`}>{info.message}</div>)}
            </div>
          </aside>

          <section className="graph-column">
            <div className="view-tabs">
              <button type="button" className={mode === "call" ? "active" : ""} onClick={() => setMode("call")}>Call graph</button>
              <button type="button" className={mode === "cfg" ? "active" : ""} onClick={() => setMode("cfg")} disabled={!selected}>Function CFG</button>
              {mode === "call" && <>
                <button type="button" className={callScope === "neighborhood" ? "active" : ""} onClick={() => setCallScope("neighborhood")}>Main neighborhood</button>
                <button type="button" className={callScope === "all" ? "active" : ""} onClick={() => setCallScope("all")}>All functions</button>
              </>}
              {mode === "cfg" && selected && <span className="selected-label">{functionName(selected.id, names)}</span>}
            </div>
            <GraphPane
              program={program}
              names={names}
              mode={mode}
              callScope={callScope}
              selectedFunction={selected?.id}
              onSelectFunction={(id) => {
                setSelectedFunction(id);
                setMode("cfg");
              }}
            />
          </section>

          <DetailPane program={program} fn={selected} names={names} sidecars={sidecars} />
        </section>
      )}
    </main>
  );
}

function GraphPane({ program, names, mode, callScope, selectedFunction, onSelectFunction }: {
  program: CirProgram;
  names: Map<number, string>;
  mode: GraphMode;
  callScope: CallScope;
  selectedFunction?: number;
  onSelectFunction: (id: number) => void;
}) {
  const { nodes, edges } = useMemo(
    () => mode === "call"
      ? buildCallGraph(program, names, callScope)
      : buildCfg(program, names, selectedFunction),
    [callScope, mode, names, program, selectedFunction],
  );
  return (
    <ReactFlowProvider>
      <GraphCanvas
        nodes={nodes}
        edges={edges}
        onSelectFunction={onSelectFunction}
      />
    </ReactFlowProvider>
  );
}

function GraphCanvas({ nodes, edges, onSelectFunction }: {
  nodes: GraphNode[];
  edges: Edge[];
  onSelectFunction: (id: number) => void;
}) {
  const reactFlow = useReactFlow<GraphNode>();
  const [localNodes, setLocalNodes] = useState(nodes);
  useEffect(() => setLocalNodes(nodes), [nodes]);
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      void reactFlow.fitView({ padding: 0.2, duration: 0 });
    });
    return () => cancelAnimationFrame(frame);
  }, [edges.length, nodes.length, reactFlow]);
  const onNodesChange = useCallback((changes: NodeChange<GraphNode>[]) => {
    setLocalNodes((current) => applyNodeChanges(changes, current));
  }, []);
  const onNodeClick: NodeMouseHandler<GraphNode> = (_event, node) => {
    if (node.data.kind === "function") onSelectFunction(Number(node.id.slice(3)));
  };
  return (
    <div className="graph-pane">
      <ReactFlow<GraphNode>
        nodes={localNodes}
        edges={edges}
        fitView
        fitViewOptions={{ padding: 0.2 }}
        onNodeClick={onNodeClick}
        onNodesChange={onNodesChange}
        nodesDraggable
        elementsSelectable
        minZoom={0.15}
        maxZoom={2}
      >
        <Background color="#273449" gap={24} />
        <Controls />
        <MiniMap pannable zoomable nodeColor={(node) => node.data?.kind === "function" ? "#79c2ff" : "#a78bfa"} />
      </ReactFlow>
    </div>
  );
}

function buildCallGraph(program: CirProgram, names: Map<number, string>, scope: CallScope): { nodes: GraphNode[]; edges: Edge[] } {
  const distances = callDistances(program);
  const visibleFunctions = scope === "all"
    ? program.functions
    : program.functions.filter((fn) => (distances.get(fn.id) ?? Number.POSITIVE_INFINITY) <= 3);
  const visibleIds = new Set(visibleFunctions.map((fn) => fn.id));
  const orderedFunctions = [...visibleFunctions].sort((left, right) =>
    (distances.get(left.id) ?? Number.MAX_SAFE_INTEGER) - (distances.get(right.id) ?? Number.MAX_SAFE_INTEGER) || left.id - right.id,
  );
  const columns = Math.max(1, Math.ceil(Math.sqrt(orderedFunctions.length)));
  const nodes = orderedFunctions.map((fn, index) => ({
    id: `fn-${fn.id}`,
    position: { x: (index % columns) * 230, y: Math.floor(index / columns) * 145 },
    data: {
      kind: "function" as const,
      label: `${functionName(fn.id, names)}\nfn#${fn.id} · ${continuationCount(program, fn.id)} continuations`,
    },
    className: fn.id === program.mainFunction ? "main-node" : "",
    style: { width: 190 },
  }));
  const edges = callEdges(program)
    .filter((edge) => visibleIds.has(edge.caller) && visibleIds.has(edge.callee))
    .map((edge, index) => ({
    id: `call-${edge.caller}-${edge.callee}-${index}`,
    source: `fn-${edge.caller}`,
    target: `fn-${edge.callee}`,
    label: edge.sites.length > 1 ? `${edge.sites.length} calls` : undefined,
    labelStyle: { fill: "#cbd5e1", fontSize: 11 },
    style: { stroke: "#f7b267", strokeWidth: 1, strokeDasharray: "5 4", opacity: 0.45 },
    markerEnd: { type: MarkerType.ArrowClosed, color: "#f7b267" },
    animated: false,
    }));
  return { nodes, edges };
}

function callDistances(program: CirProgram): Map<number, number> {
  const outgoing = new Map<number, number[]>();
  for (const edge of callEdges(program)) {
    const targets = outgoing.get(edge.caller) ?? [];
    targets.push(edge.callee);
    outgoing.set(edge.caller, targets);
  }
  const distance = new Map<number, number>([[program.mainFunction, 0]]);
  const queue = [program.mainFunction];
  for (let index = 0; index < queue.length; index += 1) {
    const caller = queue[index];
    const current = distance.get(caller) ?? 0;
    for (const callee of outgoing.get(caller) ?? []) {
      if (!distance.has(callee)) {
        distance.set(callee, current + 1);
        queue.push(callee);
      }
    }
  }
  return distance;
}

function buildCfg(program: CirProgram, names: Map<number, string>, selectedFunction?: number): { nodes: GraphNode[]; edges: Edge[] } {
  const fn = program.functions.find((candidate) => candidate.id === selectedFunction) ?? program.functions[0];
  if (!fn) return { nodes: [], edges: [] };
  const continuations = program.continuations.filter((continuation) => continuation.functionId === fn.id);
  const columns = Math.max(1, Math.ceil(Math.sqrt(continuations.length)));
  const nodes = continuations.map((continuation, index) => ({
    id: `cont-${continuation.id}`,
    position: { x: (index % columns) * 270, y: Math.floor(index / columns) * 190 },
    data: {
      kind: "continuation" as const,
      label: `c${continuation.id}${continuation.id === fn.entry ? " · entry" : ""}\n${continuation.instructions.length} instructions\n${continuation.terminator.text}`,
    },
    style: { width: 235 },
  }));
  const known = new Set(continuations.map((continuation) => continuation.id));
  const edges: Edge[] = [];
  for (const continuation of continuations) {
    const term = continuation.terminator;
    if (term.kind === "goto") {
      edges.push(cfgEdge(continuation.id, term.target, "goto", known));
    } else if (term.kind === "branch") {
      edges.push(cfgEdge(continuation.id, term.thenTarget, "then", known));
      edges.push(cfgEdge(continuation.id, term.elseTarget, "else", known));
    } else if (term.kind === "call") {
      edges.push(cfgEdge(continuation.id, term.returnTo, `call ${functionName(term.callee, names)} → resume`, known, true));
    }
  }
  return { nodes, edges };
}

function cfgEdge(source: number, target: number, label: string, known: Set<number>, dashed = false): Edge {
  return {
    id: `edge-${source}-${target}-${label}`,
    source: `cont-${source}`,
    target: `cont-${target}`,
    label,
    labelStyle: { fill: "#cbd5e1", fontSize: 11 },
    style: { stroke: dashed ? "#f7b267" : "#79c2ff", strokeWidth: 2, strokeDasharray: dashed ? "6 4" : undefined },
    markerEnd: { type: MarkerType.ArrowClosed, color: dashed ? "#f7b267" : "#79c2ff" },
    hidden: !known.has(target),
  };
}

function DetailPane({ program, fn, names, sidecars }: {
  program: CirProgram;
  fn?: CirFunction;
  names: Map<number, string>;
  sidecars: SidecarInfo[];
}) {
  if (!fn) return <aside className="detail-pane"><p>Select a function.</p></aside>;
  const continuations = program.continuations.filter((continuation) => continuation.functionId === fn.id);
  const metrics = new Map<number, number>();
  for (const info of sidecars) for (const [id, count] of info.continuationCounts) metrics.set(id, count);
  return (
    <aside className="detail-pane">
      <div className="detail-title">
        <div className="eyebrow">FUNCTION</div>
        <h2>{functionName(fn.id, names)}</h2>
        <span>fn#{fn.id} · entry c{fn.entry}</span>
      </div>
      <dl className="facts">
        <div><dt>frame</dt><dd>{fn.frameCells} cells</dd></div>
        <div><dt>return</dt><dd>{fn.returnType}</dd></div>
        <div><dt>parameters</dt><dd>{fn.parameters.length}</dd></div>
        <div><dt>continuations</dt><dd>{continuations.length}</dd></div>
      </dl>
      <div className="body-heading">Continuation body</div>
      <div className="continuation-list">
        {continuations.map((continuation) => <ContinuationCard key={continuation.id} continuation={continuation} metric={metrics.get(continuation.id)} />)}
      </div>
    </aside>
  );
}

function ContinuationCard({ continuation, metric }: { continuation: CirContinuation; metric?: number }) {
  return (
    <article className="continuation-card">
      <div className="continuation-heading">
        <strong>c{continuation.id}</strong>
        {metric !== undefined && <span>{metric.toLocaleString()} executions</span>}
      </div>
      {continuation.instructions.length === 0 ? (
        <div className="instruction muted">(empty body)</div>
      ) : continuation.instructions.map((instruction, index) => <div className="instruction" key={`${instruction.text}-${index}`}>{instruction.text}</div>)}
      <div className={`terminator ${continuation.terminator.kind === "call" ? "call" : ""}`}>{continuation.terminator.text}</div>
    </article>
  );
}

function continuationCount(program: CirProgram, functionId: number): number {
  return program.continuations.filter((continuation) => continuation.functionId === functionId).length;
}

function emptyProgram(): CirProgram {
  return { staticCells: 0, mainFunction: 0, functions: [], continuations: [], format: "BFCIR-1" };
}

function looksLikeMagic(bytes: Uint8Array): boolean {
  return bytes.length >= 8 && bytes[0] === 66 && bytes[1] === 70 && bytes[2] === 67 && bytes[3] === 73;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

createRoot(document.getElementById("root")!).render(<App />);
