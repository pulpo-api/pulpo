import { invoke } from "@tauri-apps/api/core";

export interface CoreInfo {
  version: string;
  protocols: string[];
}

export interface Header {
  name: string;
  value: string;
  enabled: boolean;
}

export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS";

export const HTTP_METHODS: HttpMethod[] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

export interface HttpRequest {
  method: HttpMethod;
  url: string;
  headers: Header[];
  body: string | null;
  timeoutMs?: number;
}

export interface HttpResponse {
  status: number;
  statusText: string;
  httpVersion: string;
  headers: Header[];
  body: string;
  bodyIsText: boolean;
  sizeBytes: number;
  durationMs: number;
}

export interface SendError {
  kind: "invalidUrl" | "invalidHeader" | "timeout" | "connect" | "other";
  message: string;
}

const insideTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function coreInfo(): Promise<CoreInfo> {
  if (!insideTauri) {
    return { version: "browser preview", protocols: ["http", "graphql", "grpc", "websocket", "soap", "sse"] };
  }
  return invoke<CoreInfo>("core_info");
}

export async function sendHttp(request: HttpRequest): Promise<HttpResponse> {
  if (insideTauri) {
    return invoke<HttpResponse>("send_http", { request });
  }
  return sendWithFetch(request);
}

async function sendWithFetch(request: HttpRequest): Promise<HttpResponse> {
  const headers = new Headers();
  for (const h of request.headers) {
    if (h.enabled && h.name.trim()) headers.append(h.name.trim(), h.value);
  }
  const started = performance.now();
  let response: Response;
  try {
    response = await fetch(request.url, {
      method: request.method,
      headers,
      body: request.body && !["GET", "HEAD"].includes(request.method) ? request.body : undefined,
    });
  } catch (e) {
    throw {
      kind: "connect",
      message: `${e}. In the browser preview, CORS can block requests; the desktop app has no such limit.`,
    } satisfies SendError;
  }
  const buffer = await response.arrayBuffer();
  const body = new TextDecoder().decode(buffer);
  return {
    status: response.status,
    statusText: response.statusText,
    httpVersion: "HTTP/1.1",
    headers: [...response.headers.entries()].map(([name, value]) => ({ name, value, enabled: true })),
    body,
    bodyIsText: true,
    sizeBytes: buffer.byteLength,
    durationMs: Math.round(performance.now() - started),
  };
}

export function asSendError(error: unknown): SendError {
  if (error && typeof error === "object" && "kind" in error && "message" in error) {
    return error as SendError;
  }
  return { kind: "other", message: String(error) };
}
