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
  format: "BFCIR-1";
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
    throw new Error("not a BFCIR version 1 file");
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

export function functionName(id: number, names: Map<number, string>): string {
  return names.get(id) ?? `fn#${id}`;
}
