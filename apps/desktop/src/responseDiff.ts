import { jsonPathOf, type PathSegment } from "./jsonNodes";

export interface FieldChange {
  path: string;
  kind: "added" | "removed" | "changed";
  before?: unknown;
  after?: unknown;
}

/** Differences between two JSON values, field by field (arrays by position). */
export function jsonDiff(before: unknown, after: unknown, path: PathSegment[] = [], out: FieldChange[] = []): FieldChange[] {
  const isObj = (v: unknown) => v !== null && typeof v === "object";
  if (isObj(before) && isObj(after) && Array.isArray(before) === Array.isArray(after)) {
    const a = before as Record<string, unknown>;
    const b = after as Record<string, unknown>;
    const keys = Array.isArray(before)
      ? [...Array(Math.max((before as unknown[]).length, (after as unknown[]).length)).keys()]
      : [...new Set([...Object.keys(a), ...Object.keys(b)])];
    for (const key of keys) {
      const seg = Array.isArray(before) ? Number(key) : String(key);
      const inA = Array.isArray(before) ? Number(key) < (before as unknown[]).length : key in a;
      const inB = Array.isArray(after) ? Number(key) < (after as unknown[]).length : key in b;
      if (!inA) out.push({ path: jsonPathOf([...path, seg]), kind: "added", after: b[key as string] });
      else if (!inB) out.push({ path: jsonPathOf([...path, seg]), kind: "removed", before: a[key as string] });
      else jsonDiff(a[key as string], b[key as string], [...path, seg], out);
      if (out.length > 1000) break;
    }
  } else if (JSON.stringify(before) !== JSON.stringify(after)) {
    out.push({ path: jsonPathOf(path), kind: "changed", before, after });
  }
  return out;
}

export interface LineChange {
  kind: "same" | "added" | "removed";
  text: string;
}

/** Line diff (longest common subsequence) for text bodies; big bodies are compared roughly. */
export function lineDiff(before: string, after: string): LineChange[] {
  const a = before.split("\n");
  const b = after.split("\n");
  if (a.length * b.length > 4_000_000) {
    return [
      ...a.filter((l) => !b.includes(l)).map((text) => ({ kind: "removed" as const, text })),
      ...b.filter((l) => !a.includes(l)).map((text) => ({ kind: "added" as const, text })),
    ];
  }
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () => new Array(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const out: LineChange[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: "same", text: a[i] });
      i++;
      j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) out.push({ kind: "removed", text: a[i++] });
    else out.push({ kind: "added", text: b[j++] });
  }
  while (i < a.length) out.push({ kind: "removed", text: a[i++] });
  while (j < b.length) out.push({ kind: "added", text: b[j++] });
  return out;
}
