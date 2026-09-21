import type { CirProgram } from "./cir";

export type SidecarInfo = {
  fileName: string;
  kind: "bfmap" | "metrics" | "source" | "unknown";
  message: string;
  functionNames: Map<number, string>;
  continuationCounts: Map<number, number>;
};

type JsonObject = Record<string, unknown>;

export async function inspectFile(file: File): Promise<SidecarInfo> {
  const lower = file.name.toLowerCase();
  if (lower.endsWith(".bfc") || lower.endsWith(".txt")) {
    return {
      fileName: file.name,
      kind: "source",
      message: `${file.name}: source file loaded (source mapping is not inferred yet)`,
      functionNames: new Map(),
      continuationCounts: new Map(),
    };
  }
  if (!lower.endsWith(".json")) {
    return emptyInfo(file.name, "unknown", `${file.name}: ignored (drop a .cir or JSON sidecar)`);
  }
  try {
    const value: unknown = JSON.parse(await file.text());
    const functionNames = new Map<number, string>();
    const continuationCounts = new Map<number, number>();
    const functions = arrayAt(value, "functions");
    for (const item of functions) {
      if (!isObject(item)) continue;
      const id = numberAt(item, "id");
      const name = stringAt(item, "name") ?? stringAt(item, "function_name");
      if (id !== undefined && name) functionNames.set(id, name);
    }
    const continuations = arrayAt(value, "continuations");
    for (const item of continuations) {
      if (!isObject(item)) continue;
      const id = numberAt(item, "id");
      const executions = numberAt(item, "executions");
      if (id !== undefined && executions !== undefined) continuationCounts.set(id, executions);
    }
    const looksLikeMetrics = typeof value === "object" && value !== null &&
      (functions.length > 0 || continuations.length > 0 || stringAt(value, "format")?.includes("metrics") === true);
    const kind = looksLikeMetrics ? "metrics" : "bfmap";
    if (kind === "bfmap") {
      const siteCount = arrayAt(value, "sites").length;
      const fileCount = arrayAt(value, "files").length;
      return {
        fileName: file.name,
        kind,
        message: `${file.name}: profile map loaded (${siteCount} sites, ${fileCount} source files; static BF metadata only)`,
        functionNames,
        continuationCounts,
      };
    }
    const details = [
      `${functions.length} function records`,
      `${continuations.length} continuation records`,
      functionNames.size ? `${functionNames.size} names` : "no CIR names",
    ];
    return { fileName: file.name, kind, message: `${file.name}: metrics loaded (${details.join(", ")})`, functionNames, continuationCounts };
  } catch (error) {
    return emptyInfo(file.name, "unknown", `${file.name}: invalid JSON (${messageOf(error)})`);
  }
}

export function mergeNames(infos: SidecarInfo[], program: CirProgram): { names: Map<number, string>; warnings: string[] } {
  const names = new Map<number, string>();
  const warnings: string[] = [];
  for (const info of infos) {
    for (const [id, name] of info.functionNames) {
      const existing = names.get(id);
      if (existing && existing !== name) warnings.push(`function ${id} has conflicting names: ${existing} / ${name}`);
      else names.set(id, name);
    }
  }
  for (const id of names.keys()) {
    if (!program.functions.some((fn) => fn.id === id)) {
      warnings.push(`sidecar names function ${id}, but the loaded CIR does not contain it`);
    }
  }
  if (!infos.some((info) => info.kind === "metrics") && program.functions.length > 0) {
    warnings.push("no IR metrics sidecar loaded; graph is static and has no execution heat");
  }
  return { names, warnings };
}

function emptyInfo(fileName: string, kind: SidecarInfo["kind"], message: string): SidecarInfo {
  return { fileName, kind, message, functionNames: new Map(), continuationCounts: new Map() };
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function arrayAt(value: unknown, key: string): unknown[] {
  return isObject(value) && Array.isArray(value[key]) ? value[key] : [];
}

function numberAt(value: JsonObject, key: string): number | undefined {
  const candidate = value[key];
  return typeof candidate === "number" && Number.isFinite(candidate) ? candidate : undefined;
}

function stringAt(value: unknown, key: string): string | undefined {
  return isObject(value) && typeof value[key] === "string" ? value[key] : undefined;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
