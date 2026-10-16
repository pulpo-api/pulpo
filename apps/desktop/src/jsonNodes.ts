/** Where each value of a JSON text is, so the response view can act on the node under the
 * pointer: fold it, copy it, copy its path, filter or test by it. */

export type PathSegment = string | number;

export interface JsonNode {
  path: PathSegment[];
  /** Start of the node: its key in an object, else its value. */
  from: number;
  valueFrom: number;
  to: number;
  container: boolean;
}

/** Every node of a JSON text with its position (null if the text is not JSON). */
export function jsonNodes(text: string): JsonNode[] | null {
  const nodes: JsonNode[] = [];
  let i = 0;
  const ws = () => {
    while (i < text.length && " \t\n\r".includes(text[i])) i++;
  };
  const fail = () => {
    throw new Error("not JSON");
  };
  const str = () => {
    if (text[i] !== '"') fail();
    i++;
    while (i < text.length && text[i] !== '"') i += text[i] === "\\" ? 2 : 1;
    if (i >= text.length) fail();
    i++;
  };
  const value = (path: PathSegment[], from: number): void => {
    ws();
    const valueFrom = i;
    const c = text[i];
    if (c === "{" || c === "[") {
      const close = c === "{" ? "}" : "]";
      i++;
      ws();
      let index = 0;
      while (text[i] !== close) {
        if (c === "{") {
          ws();
          const keyFrom = i;
          str();
          const key = JSON.parse(text.slice(keyFrom, i)) as string;
          ws();
          if (text[i] !== ":") fail();
          i++;
          value([...path, key], keyFrom);
        } else {
          ws();
          value([...path, index++], i);
        }
        ws();
        if (text[i] === ",") i++;
        else if (text[i] !== close) fail();
        ws();
      }
      i++;
    } else if (c === '"') {
      str();
    } else {
      const m = /^(-?\d+(\.\d+)?([eE][+-]?\d+)?|true|false|null)/.exec(text.slice(i, i + 400));
      if (!m) fail();
      i += m![0].length;
    }
    nodes.push({ path, from, valueFrom, to: i, container: c === "{" || c === "[" });
  };
  try {
    ws();
    value([], i);
    ws();
    return i === text.length ? nodes : null;
  } catch {
    return null;
  }
}

/** The innermost node at a position. */
export function nodeAt(nodes: JsonNode[], pos: number): JsonNode | null {
  let best: JsonNode | null = null;
  for (const n of nodes) {
    if (n.from <= pos && pos <= n.to && (!best || n.to - n.from < best.to - best.from)) best = n;
  }
  return best;
}

/** `$.data[0]['first name']`: valid JSONPath (RFC 9535) for the node. */
export function jsonPathOf(path: PathSegment[]): string {
  return (
    "$" +
    path
      .map((s) =>
        typeof s === "number"
          ? `[${s}]`
          : /^[A-Za-z_][A-Za-z0-9_]*$/.test(s)
            ? `.${s}`
            : `['${s.replace(/\\/g, "\\\\").replace(/'/g, "\\'")}']`,
      )
      .join("")
  );
}

export function valueAt(root: unknown, path: PathSegment[]): unknown {
  let node = root;
  for (const s of path) node = (node as Record<string | number, unknown>)?.[s];
  return node;
}
