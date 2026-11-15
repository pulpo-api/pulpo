// GraphQL subscriptions over WebSocket: the `graphql-transport-ws` protocol (graphql-ws
// library) and the older `graphql-ws` one (subscriptions-transport-ws, Apollo). The
// server picks one in the handshake; the messages differ only in a few names.

import { backend, clone, type RequestEntry, type WsFrame } from "./workspace";

export type OperationType = "query" | "mutation" | "subscription";

/** The type of the operation that will run: the named one, else the first. */
export function operationType(query: string, operationName?: string | null): OperationType {
  const text = query.replace(/#[^\n]*/g, "");
  const ops = [...text.matchAll(/(^|[\s}])(query|mutation|subscription)\b\s*([_A-Za-z][_0-9A-Za-z]*)?/g)].map((m) => ({
    type: m[2] as OperationType,
    name: m[3] ?? "",
  }));
  if (!ops.length) return "query";
  const named = operationName?.trim() ? ops.find((o) => o.name === operationName.trim()) : undefined;
  return (named ?? ops[0]).type;
}

/** The WebSocket URL of a GraphQL endpoint: same address, ws/wss scheme. */
export function toWsUrl(url: string): string {
  return url.trim().replace(/^http(s?):\/\//i, (_, s: string) => `ws${s}://`);
}

export interface SubEvent {
  kind: "info" | "data" | "error" | "done";
  text: string;
  atMs: number;
}

export interface Subscription {
  stop(): Promise<void>;
}

/** Subscribes with the request's operation; every event goes to `onEvent`. */
export async function subscribe(
  root: string,
  entry: RequestEntry,
  environmentId: string,
  onEvent: (event: SubEvent) => void,
): Promise<Subscription> {
  if (!backend.wsConnect) throw "WebSocket is not available here";
  const g = entry.request.graphql;
  if (!g) throw "not a GraphQL request";
  const socket = clone(entry);
  socket.protocol = "websocket";
  socket.request.url = toWsUrl(entry.request.url);
  socket.request.graphql = null;
  socket.request.messages = [];
  socket.request.body = null;
  // Offer both protocols; the server says which one it speaks.
  socket.request.headers = [
    ...socket.request.headers.filter((h) => h.name.trim().toLowerCase() !== "sec-websocket-protocol"),
    { name: "Sec-WebSocket-Protocol", value: "graphql-transport-ws, graphql-ws", enabled: true },
  ];
  const id = "1";
  let legacy = false;
  let done = false;
  let send: (text: string) => Promise<void> = async () => {};
  // Variables go in as written: `{{name}}` is filled in when the message is sent, so a
  // value like {{userId}} without quotes becomes valid JSON then.
  const variables = (g.variables ?? "").trim() || "{}";
  const operation = g.operationName?.trim() ? `,"operationName":${JSON.stringify(g.operationName.trim())}` : "";
  const payload = `{"query":${JSON.stringify(g.query)},"variables":${variables}${operation}}`;

  const onFrame = (frame: WsFrame) => {
    if (frame.direction === "info") {
      if (/protocol graphql-ws\b/.test(frame.text)) legacy = true;
      if (frame.text.startsWith("Closed") && !done) onEvent({ kind: "info", text: frame.text, atMs: frame.atMs });
      if (frame.text.startsWith("Connected")) onEvent({ kind: "info", text: frame.text, atMs: frame.atMs });
      return;
    }
    if (frame.direction === "error") {
      onEvent({ kind: "error", text: frame.text, atMs: frame.atMs });
      return;
    }
    if (frame.direction !== "received") return;
    let message: { type?: string; id?: string; payload?: unknown };
    try {
      message = JSON.parse(frame.text);
    } catch {
      onEvent({ kind: "info", text: frame.text, atMs: frame.atMs });
      return;
    }
    switch (message.type) {
      case "connection_ack":
        onEvent({ kind: "info", text: `Subscribed (${legacy ? "graphql-ws" : "graphql-transport-ws"})`, atMs: frame.atMs });
        void send(`{"id":"${id}","type":"${legacy ? "start" : "subscribe"}","payload":${payload}}`);
        break;
      case "ping":
        void send(JSON.stringify({ type: "pong" }));
        break;
      case "ka":
      case "pong":
        break;
      case "next":
      case "data":
        onEvent({ kind: "data", text: JSON.stringify(message.payload), atMs: frame.atMs });
        break;
      case "error":
      case "connection_error":
        onEvent({ kind: "error", text: JSON.stringify(message.payload), atMs: frame.atMs });
        break;
      case "complete":
        done = true;
        onEvent({ kind: "done", text: "The server ended the subscription", atMs: frame.atMs });
        void connection.close();
        break;
      default:
        onEvent({ kind: "info", text: frame.text, atMs: frame.atMs });
    }
  };

  const connection = await backend.wsConnect(root, socket, environmentId, onFrame);
  send = (text) => connection.send(text);
  await connection.send(JSON.stringify({ type: "connection_init", payload: {} }));
  return {
    async stop() {
      if (!done) {
        done = true;
        try {
          await connection.send(JSON.stringify({ id, type: legacy ? "stop" : "complete" }));
        } catch {
          /* already closed */
        }
      }
      await connection.close();
    },
  };
}
