// Socket.IO v4 packets over WebSocket, as pulpo-core's socketio.rs: Engine.IO `0` open,
// `2`/`3` ping/pong, `4` + Socket.IO `0` connect, `2` event, `4` connect error, `1` disconnect.

export interface SocketIoTarget {
  namespace: string;
  path: string;
  auth?: string;
}

/** `http(s)://host:port` (or `ws(s)://`) → `ws(s)://host:port/socket.io/?EIO=4&transport=websocket`. */
export function socketUrl(url: string, path: string): string {
  const u = url.trim();
  const [scheme, rest0] = u.includes("://") ? [u.split("://")[0].toLowerCase(), u.slice(u.indexOf("://") + 3)] : ["http", u];
  const secure = scheme === "https" || scheme === "wss";
  const [rest, query] = rest0.includes("?") ? [rest0.slice(0, rest0.indexOf("?")), `&${rest0.slice(rest0.indexOf("?") + 1)}`] : [rest0, ""];
  const host = rest.split("/")[0];
  const p = `/${path.trim().replace(/^\/+|\/+$/g, "")}/`;
  return `${secure ? "wss" : "ws"}://${host}${p}?EIO=4&transport=websocket${query}`;
}

function prefix(namespace: string): string {
  const ns = namespace.trim();
  if (!ns || ns === "/") return "";
  return `${ns.startsWith("/") ? ns : `/${ns}`},`;
}

export function connectPacket(target: SocketIoTarget): string {
  return `40${prefix(target.namespace)}${(target.auth ?? "").trim()}`;
}

/** `42["name", data]`: data as JSON when it is, else as text; none when empty. Variables
 * (`{{name}}`) are filled in when the packet is sent, so they stay inside the text. */
export function emitPacket(namespace: string, event: string, body: string): string {
  const data = body.trim();
  let arg = "";
  if (data) {
    let json = true;
    try {
      JSON.parse(data.replace(/"?\{\{[^}]+\}\}"?/g, '"x"'));
    } catch {
      json = false;
    }
    arg = `,${json ? data : JSON.stringify(data)}`;
  }
  return `42${prefix(namespace)}[${JSON.stringify(event)}${arg}]`;
}

export type Packet =
  | { kind: "open" | "ping" | "pong" | "connected" | "disconnected" | "other" }
  | { kind: "connectError"; message: string }
  | { kind: "event"; event: string; args: unknown[] };

export function parsePacket(text: string): Packet {
  if (text.startsWith("0")) return { kind: "open" };
  if (text === "2") return { kind: "ping" };
  if (text === "3") return { kind: "pong" };
  if (!text.startsWith("4")) return { kind: "other" };
  const type = text[1];
  let rest = text.slice(2);
  if (rest.startsWith("/")) rest = rest.includes(",") ? rest.slice(rest.indexOf(",") + 1) : "";
  rest = rest.replace(/^\d+/, "");
  if (type === "0") return { kind: "connected" };
  if (type === "1") return { kind: "disconnected" };
  if (type === "4") {
    try {
      return { kind: "connectError", message: JSON.parse(rest).message ?? rest };
    } catch {
      return { kind: "connectError", message: rest };
    }
  }
  if (type === "2" || type === "3") {
    try {
      const items = JSON.parse(rest);
      if (Array.isArray(items) && items.length) {
        return { kind: "event", event: String(items[0]), args: items.slice(1) };
      }
    } catch {
      /* not an event */
    }
  }
  return { kind: "other" };
}
