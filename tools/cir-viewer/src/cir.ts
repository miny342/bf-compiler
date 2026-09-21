export type ReturnTypeName = "void" | "cell" | `aggregate(${number})`;

export type Instruction = {
  kind: string;
  text: string;
};

export type Terminator =
  | { kind: "goto"; target: number; text: string }
  | { kind: "branch"; condition: number; thenTarget: number; elseTarget: number; text: string }
  | { kind: "call"; callee: number; returnTo: number; arguments: CallArgument[]; text: string }
  | { kind: "return-void"; text: string }
  | { kind: "return-cell"; source: number; text: string }
  | { kind: "return-aggregate"; source: number; cells: number; text: string }
  | { kind: "halt"; text: string }
  | { kind: "abort"; text: string };

export type CallArgument = { source: number; destination: number; cells: number };

export type CirFunction = {
  id: number;
  entry: number;
  frameCells: number;
  returnType: ReturnTypeName;
  parameters: { destination: number; cells: number }[];
  name?: string;
};

export type CirContinuation = {
  id: number;
  functionId: number;
  instructions: Instruction[];
  terminator: Terminator;
};

export type CirProgram = {
  staticCells: number;
  mainFunction: number;
  functions: CirFunction[];
  continuations: CirContinuation[];
  format: "BFCIR-1" | "BFCIR-Rust-IR-1";
};

class Reader {
  private offset = 0;
  private readonly bytes: Uint8Array;

  constructor(bytes: Uint8Array) {
    this.bytes = bytes;
  }

  get position(): number {
    return this.offset;
  }

  get done(): boolean {
    return this.offset === this.bytes.length;
  }

  bytesLeft(): number {
    return this.bytes.length - this.offset;
  }

  u8(): number {
    this.require(1);
    return this.bytes[this.offset++];
  }

  u16(): number {
    this.require(2);
    const value = this.bytes[this.offset] | (this.bytes[this.offset + 1] << 8);
    this.offset += 2;
    return value;
  }

  u24(): number {
    this.require(3);
    const value =
      this.bytes[this.offset] |
      (this.bytes[this.offset + 1] << 8) |
      (this.bytes[this.offset + 2] << 16);
    this.offset += 3;
    return value;
  }

  private require(count: number): void {
    if (this.offset + count > this.bytes.length) {
      throw new Error(`truncated CIR at byte ${this.offset}`);
    }
  }
}

const MAGIC = [66, 70, 67, 73, 82, 0, 1, 10];

export function looksLikeCir(bytes: Uint8Array): boolean {
  return MAGIC.every((byte, index) => bytes[index] === byte);
}

export function parseCir(input: ArrayBuffer | Uint8Array): CirProgram {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  const reader = new Reader(bytes);
  if (!looksLikeCir(bytes)) {
    try {
      return parseRustIr(JSON.parse(new TextDecoder().decode(bytes)));
    } catch (error) {
      throw new Error(`not a BFCIR version 1 file or Rust continuation IR JSON: ${messageOf(error)}`);
    }
  }
  for (let index = 0; index < MAGIC.length; index += 1) reader.u8();

  const program: CirProgram = {
    staticCells: reader.u24(),
    mainFunction: reader.u16(),
    functions: [],
    continuations: [],
    format: "BFCIR-1",
  };
  let current: { id: number; functionId: number; instructions: Instruction[] } | undefined;

  while (!reader.done) {
    const record = reader.u8();
    if (record === 0xff) {
      if (current) throw new Error(`continuation c${current.id} has no terminator at byte ${reader.position - 1}`);
      if (!reader.done) throw new Error(`trailing bytes after CIR end at byte ${reader.position}`);
      break;
    }
    if (record === 1) {
      if (current) throw new Error("function record inside a continuation");
      const id = reader.u16();
      const entry = reader.u16();
      const frameCells = reader.u8();
      const returnTag = reader.u8();
      const returnCells = reader.u8();
      let returnType: ReturnTypeName;
      if (returnTag === 0 && returnCells === 0) returnType = "void";
      else if (returnTag === 1 && returnCells === 0) returnType = "cell";
      else if (returnTag === 2 && returnCells !== 0) returnType = `aggregate(${returnCells})`;
      else throw new Error(`invalid return type for function ${id}`);
      const parameters = Array.from({ length: reader.u8() }, () => ({
        destination: reader.u8(),
        cells: reader.u8(),
      }));
      program.functions.push({ id, entry, frameCells, returnType, parameters });
      continue;
    }
    if (record === 2) {
      if (current) throw new Error(`continuation c${current.id} has no terminator before byte ${reader.position - 1}`);
      current = { id: reader.u16(), functionId: reader.u16(), instructions: [] };
      continue;
    }
    if (record === 3) {
      if (!current) throw new Error("instruction outside a continuation");
      current.instructions.push(readInstruction(reader));
      continue;
    }
    if (record === 4) {
      if (!current) throw new Error("terminator outside a continuation");
      const continuation = {
        id: current.id,
        functionId: current.functionId,
        instructions: current.instructions,
        terminator: readTerminator(reader),
      };
      program.continuations.push(continuation);
      current = undefined;
      continue;
    }
    throw new Error(`unknown CIR record tag ${record} at byte ${reader.position - 1}`);
  }
  if (current) throw new Error("continuation has no terminator");
  if (!program.functions.some((fn) => fn.id === program.mainFunction)) {
    throw new Error(`main function ${program.mainFunction} is missing`);
  }
  return program;
}

function readInstruction(reader: Reader): Instruction {
  const tag = reader.u8();
  if (tag === 1) {
    const destination = reader.u8();
    const value = reader.u8();
    return { kind: "set", text: `f[${destination}] = ${value}` };
  }
  if (tag === 2) {
    const destination = reader.u8();
    const source = reader.u8();
    return { kind: "copy", text: `f[${destination}] = f[${source}]` };
  }
  if (tag === 3) {
    return { kind: "copy-abi", text: `f[${reader.u8()}] = abi.value` };
  }
  if (tag === 4) {
    return { kind: "input", text: `f[${reader.u8()}] = input()` };
  }
  if (tag === 5) {
    return { kind: "output", text: `output(f[${reader.u8()}])` };
  }
  if (tag >= 8 && tag <= 10) {
    const destination = reader.u8();
    return {
      kind: "unary",
      text: formatArithmetic(tag, destination, destination),
    };
  }
  if ((tag >= 6 && tag <= 7) || (tag >= 11 && tag <= 16)) {
    const destination = reader.u8();
    const source = reader.u8();
    return { kind: "binary", text: formatArithmetic(tag, destination, source) };
  }
  if (tag >= 17 && tag <= 21) {
    const data = reader.u8();
    const address = reader.u24();
    return { kind: "global", text: `${globalName(tag)}(f[${data}], g[${address}])` };
  }
  if (tag >= 22 && tag <= 25) {
    const data = reader.u8();
    const offsetLow = reader.u8();
    const offsetHigh = reader.u8();
    const base = reader.u24();
    const cells = reader.u24();
    const storage = reader.u8() === 0 ? "frame" : "global";
    return {
      kind: "array",
      text: `${arrayName(tag)}(${storage}[${base}..${base + cells}], f[${data}], offset=f[${offsetLow}] + 256*f[${offsetHigh}])`,
    };
  }
  if (tag === 26) {
    const destination = reader.u8();
    const index = reader.u8();
    return { kind: "copy-outbox", text: `f[${destination}] = outbox[${index}]` };
  }
  if (tag === 27) {
    const low = reader.u8();
    const high = reader.u8();
    const source = reader.u8();
    const amount = reader.u16();
    return { kind: "offset", text: `[f[${low}], f[${high}]] += f[${source}] * ${amount}` };
  }
  if (tag === 28) {
    const low = reader.u8();
    const high = reader.u8();
    const amount = reader.u16();
    return { kind: "offset", text: `[f[${low}], f[${high}]] += ${amount}` };
  }
  throw new Error(`unknown instruction tag ${tag}`);
}

function readTerminator(reader: Reader): Terminator {
  const tag = reader.u8();
  if (tag === 1) {
    const target = reader.u16();
    return { kind: "goto", target, text: `goto c${target}` };
  }
  if (tag === 2) {
    const condition = reader.u8();
    const thenTarget = reader.u16();
    const elseTarget = reader.u16();
    return {
      kind: "branch",
      condition,
      thenTarget,
      elseTarget,
      text: `if (f[${condition}] != 0) goto c${thenTarget} else goto c${elseTarget}`,
    };
  }
  if (tag === 3) {
    const callee = reader.u16();
    const returnTo = reader.u16();
    const args = Array.from({ length: reader.u8() }, () => ({
      source: reader.u8(),
      destination: reader.u8(),
      cells: reader.u8(),
    }));
    return {
      kind: "call",
      callee,
      returnTo,
      arguments: args,
      text: `call fn#${callee}(${args.map((arg) => `f[${arg.source}] -> f[${arg.destination}][${arg.cells}]`).join(", ")}) ; resume c${returnTo}`,
    };
  }
  if (tag === 4) return { kind: "return-void", text: "return" };
  if (tag === 5) {
    const source = reader.u8();
    return { kind: "return-cell", source, text: `return f[${source}]` };
  }
  if (tag === 6) return { kind: "halt", text: "halt" };
  if (tag === 7) {
    const source = reader.u8();
    const cells = reader.u8();
    return { kind: "return-aggregate", source, cells, text: `return f[${source}..${source + cells}]` };
  }
  if (tag === 8) return { kind: "abort", text: "abort" };
  throw new Error(`unknown terminator tag ${tag}`);
}

type JsonObject = Record<string, unknown>;

function parseRustIr(value: unknown): CirProgram {
  const root = asObject(value);
  if (root.format !== "bfc-continuation-ir-v1") {
    throw new Error("unknown continuation IR format");
  }
  const raw = asObject(root.program);
  const functions = arrayAt(raw.functions).map((item) => {
    const fn = asObject(item);
    const id = requiredNumber(fn.id, "function id");
    const parameterLocations = arrayAt(fn.parameter_locations);
    return {
      id,
      entry: requiredNumber(fn.entry, `function ${id} entry`),
      frameCells: numberAt(fn.frame_slots) ?? 0,
      returnType: valueTypeName(fn.return_type),
      parameters: parameterLocations.map((parameter) => parameterLocation(parameter)),
      name: stringAt(fn.name),
    };
  });
  const continuations = arrayAt(raw.continuations).map((item) => {
    const continuation = asObject(item);
    const id = requiredNumber(continuation.id, "continuation id");
    return {
      id,
      functionId: requiredNumber(continuation.function, `continuation ${id} function`),
      instructions: arrayAt(continuation.body).map((instruction) => ({
        kind: "frame",
        text: frameInstructionText(instruction),
      })),
      terminator: rustTerminator(continuation.terminator),
    };
  });
  return {
    staticCells: arrayAt(raw.globals).reduce<number>((total, item) => {
      const global = asObject(item);
      return total + valueTypeCells(global.value_type);
    }, 0),
    mainFunction: requiredNumber(raw.main, "main function"),
    functions,
    continuations,
    format: "BFCIR-Rust-IR-1",
  };
}

function rustTerminator(value: unknown): Terminator {
  if (value === "Halt") return { kind: "halt", text: "halt" };
  if (value === "Abort") return { kind: "abort", text: "abort" };
  const term = asObject(value);
  const [tag, payload] = singleEntry(term);
  if (tag === "Goto") {
    const target = requiredNumber(asObject(payload).target, "goto target");
    return { kind: "goto", target, text: `goto c${target}` };
  }
  if (tag === "Branch") {
    const branch = asObject(payload);
    const condition = addressText(branch.condition);
    const thenTarget = requiredNumber(branch.then_target, "branch then target");
    const elseTarget = requiredNumber(branch.else_target, "branch else target");
    return {
      kind: "branch",
      condition: 0,
      thenTarget,
      elseTarget,
      text: `if (${condition} != 0) goto c${thenTarget} else goto c${elseTarget}`,
    };
  }
  if (tag === "BranchWithBodies") {
    const branch = asObject(payload);
    const thenTarget = requiredNumber(branch.then_target, "branch then target");
    const elseTarget = requiredNumber(branch.else_target, "branch else target");
    return {
      kind: "branch",
      condition: 0,
      thenTarget,
      elseTarget,
      text: `if (${addressText(branch.condition)} != 0) { ${bodyText(branch.then_body)} } -> c${thenTarget} else { ${bodyText(branch.else_body)} } -> c${elseTarget}`,
    };
  }
  if (tag === "Call") {
    const call = asObject(payload);
    const callee = requiredNumber(call.callee, "call callee");
    const returnTo = requiredNumber(call.return_to, "call return target");
    const args = arrayAt(call.arguments).map(valueOperandText);
    return {
      kind: "call",
      callee,
      returnTo,
      arguments: [],
      text: `call fn#${callee}(${args.join(", ")}); resume c${returnTo}`,
    };
  }
  if (tag === "Return") {
    const returnedValue = asObject(payload).value;
    const returned = returnedValue === null || returnedValue === undefined
      ? ""
      : ` ${valueOperandText(returnedValue)}`;
    return { kind: "return-void", text: `return${returned}` };
  }
  if (tag === "ArrayLoad" || tag === "ArrayStore" || tag === "AggregateLoad" || tag === "AggregateStore") {
    const operation = asObject(payload);
    const returnTo = requiredNumber(operation.return_to, `${tag} return target`);
    return { kind: "goto", target: returnTo, text: `${tag} ...; resume c${returnTo}` };
  }
  throw new Error(`unknown Rust continuation terminator ${tag}`);
}

function frameInstructionText(value: unknown): string {
  const instruction = asObject(value);
  const [tag, payload] = singleEntry(instruction);
  const fields = asObject(payload);
  if (tag === "SubWithBorrow") {
    return `${addressText(fields.difference)} = ${addressText(fields.left)} - ${addressText(fields.right)}; borrow ${addressText(fields.borrow)}`;
  }
  if (tag === "Compare") {
    return `${addressText(fields.dst)} = (${addressText(fields.left)} < ${addressText(fields.right)}) ? ${fields.true_value} : ${fields.false_value}`;
  }
  if (tag === "Set") return `${addressText(fields.dst)} = ${fields.value}`;
  if (tag === "AddConst") return `${addressText(fields.dst)} += ${fields.value}`;
  if (tag === "Copy") return `${addressText(fields.dst)} = ${addressText(fields.src)}`;
  if (tag === "Transfer") {
    const targets = arrayAt(fields.targets).map((target) => {
      const item = asObject(target);
      return `${addressText(item.dst)} * ${item.factor}`;
    });
    return `${addressText(fields.src)} -> ${targets.join(", ")}`;
  }
  if (tag === "AggregateCopy") {
    return `copy ${regionText(fields.src)} -> ${regionText(fields.dst)} (${fields.cells} cells)`;
  }
  if (tag === "Input") return `${addressText(fields.dst)} = input()`;
  if (tag === "Output") return `output(${addressText(fields.src)})`;
  if (tag === "Loop") return `while (${addressText(fields.condition)} != 0) { ${bodyText(fields.body)} }`;
  if (tag === "Branch") {
    return `if (${addressText(fields.condition)} != 0) { ${bodyText(fields.then_body)} } else { ${bodyText(fields.else_body)} }`;
  }
  return `${tag} ${JSON.stringify(payload)}`;
}

function bodyText(value: unknown): string {
  return arrayAt(value).map(frameInstructionText).join("; ");
}

function addressText(value: unknown): string {
  if (typeof value === "number") return `f[${value}]`;
  if (value === "AbiValue") return "abi.value";
  const object = asObject(value);
  const [tag, payload] = singleEntry(object);
  if (tag === "Frame") return `f[${requiredNumber(payload, "frame slot")}]`;
  if (tag === "Global") return `g[${requiredNumber(payload, "global id")}]`;
  if (tag === "AbiValue") return "abi.value";
  if (tag === "ArrayElement") {
    const item = asObject(payload);
    return `${regionText(item.array)}[${item.index}]`;
  }
  return JSON.stringify(value);
}

function regionText(value: unknown): string {
  if (value === "Outbox") return "outbox";
  const object = asObject(value);
  const [tag, payload] = singleEntry(object);
  if (tag === "Frame") return `frame#${requiredNumber(payload, "frame aggregate")}`;
  if (tag === "Global") return `global#${requiredNumber(payload, "global aggregate")}`;
  if (tag === "Outbox") return "outbox";
  return JSON.stringify(value);
}

function valueOperandText(value: unknown): string {
  const object = asObject(value);
  const [tag, payload] = singleEntry(object);
  if (tag === "Cell") return addressText(payload);
  if (tag === "Array") return regionText(payload);
  if (tag === "Aggregate") {
    const item = asObject(payload);
    return `${regionText(item.region)}[${item.offset}..${Number(item.offset) + Number(item.cells)}]`;
  }
  return JSON.stringify(value);
}

function parameterLocation(value: unknown): { destination: number; cells: number } {
  const object = asObject(value);
  const [tag, payload] = singleEntry(object);
  if (tag === "Cell") return { destination: requiredNumber(payload, "parameter slot"), cells: 1 };
  if (tag === "AggregateElement") {
    const item = asObject(payload);
    return { destination: requiredNumber(item.index, "parameter index"), cells: 1 };
  }
  return { destination: 0, cells: 1 };
}

function valueTypeName(value: unknown): ReturnTypeName {
  if (value === "Void") return "void";
  if (value === "Cell") return "cell";
  const object = typeof value === "object" && value !== null ? value as JsonObject : undefined;
  if (object?.Aggregate !== undefined) {
    const payload = asObject(object.Aggregate);
    return `aggregate(${requiredNumber(payload.cells, "aggregate cells")})`;
  }
  if (object?.Array !== undefined) return `aggregate(${requiredNumber(object.Array, "array cells")})`;
  return "void";
}

function valueTypeCells(value: unknown): number {
  if (value === "Cell") return 1;
  if (value === "Void") return 0;
  const object = typeof value === "object" && value !== null ? value as JsonObject : undefined;
  if (object?.Array !== undefined) return requiredNumber(object.Array, "array cells");
  if (object?.Aggregate !== undefined) return requiredNumber(asObject(object.Aggregate).cells, "aggregate cells");
  return 0;
}

function singleEntry(value: JsonObject): [string, unknown] {
  const entries = Object.entries(value);
  if (entries.length !== 1) throw new Error("expected a tagged Rust enum value");
  return entries[0] as [string, unknown];
}

function asObject(value: unknown): JsonObject {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("expected a JSON object");
  }
  return value as JsonObject;
}

function arrayAt(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function requiredNumber(value: unknown, label: string): number {
  const result = numberAt(value);
  if (result === undefined) throw new Error(`missing numeric ${label}`);
  return result;
}

function numberAt(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function stringAt(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function formatArithmetic(tag: number, destination: number, source: number): string {
  const unary = ["", "", "", "", "", "", "+", "-", "negate", "not", "bool"];
  if (tag >= 8 && tag <= 10) return `f[${destination}] = ${unary[tag]}(f[${destination}])`;
  const operators: Record<number, string> = {
    6: "+",
    7: "-",
    11: "<",
    12: "<=",
    13: ">",
    14: ">=",
    15: "==",
    16: "!=",
  };
  return `f[${destination}] = f[${destination}] ${operators[tag] ?? `op#${tag}`} f[${source}]`;
}

function globalName(tag: number): string {
  return ({ 17: "global.set", 18: "global.copy", 19: "global.store", 20: "global.add", 21: "global.sub" } as Record<number, string>)[tag];
}

function arrayName(tag: number): string {
  return ({ 22: "array.load", 23: "array.store", 24: "array.add", 25: "array.sub" } as Record<number, string>)[tag];
}

export function successors(continuation: CirContinuation): number[] {
  const term = continuation.terminator;
  if (term.kind === "goto") return [term.target];
  if (term.kind === "branch") return [term.thenTarget, term.elseTarget];
  if (term.kind === "call") return [term.returnTo];
  return [];
}

export function callEdges(program: CirProgram): { caller: number; callee: number; sites: string[] }[] {
  const byPair = new Map<string, { caller: number; callee: number; sites: string[] }>();
  for (const continuation of program.continuations) {
    if (continuation.terminator.kind !== "call") continue;
    const { callee, returnTo } = continuation.terminator;
    const key = `${continuation.functionId}:${callee}`;
    const edge = byPair.get(key) ?? { caller: continuation.functionId, callee, sites: [] };
    edge.sites.push(`c${continuation.id} → c${returnTo}`);
    byPair.set(key, edge);
  }
  return [...byPair.values()];
}

export function functionName(id: number, names: Map<number, string>, program?: CirProgram): string {
  return names.get(id) ?? program?.functions.find((fn) => fn.id === id)?.name ?? `fn#${id}`;
}
