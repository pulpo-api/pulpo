import { invoke } from "@tauri-apps/api/core";
import { resolveText, type Vars } from "./variables";
import { applyPathParams, type Param } from "./params";

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

export type ApiKeyLocation = "header" | "query";

/** How a request authenticates. Mirrors `Auth` in pulpo-core. */
export type Auth =
  | { type: "none" }
  | { type: "bearer"; token: string }
  | { type: "basic"; username: string; password: string }
  | { type: "apiKey"; key: string; value: string; in: ApiKeyLocation };

export const AUTH_LABELS: Record<Auth["type"], string> = {
  none: "No auth",
  bearer: "Bearer token",
  basic: "Basic",
  apiKey: "API key",
};

function base64Utf8(text: string): string {
  let binary = "";
  for (const byte of new TextEncoder().encode(text)) binary += String.fromCharCode(byte);
  return btoa(binary);
}

/** Percent-encodes a query value but leaves `{{variables}}` readable (only the URL preview
 * still has them; sending fails before that if one is missing). */
function encodeKeepingVars(text: string): string {
  return text
    .split(/(\{\{[^{}]*\}\})/)
    .map((part) => (part.startsWith("{{") && part.endsWith("}}") ? part : encodeURIComponent(part)))
    .join("");
}

/** Adds the (resolved) auth to the URL or headers. A header written by hand wins.
 * Mirrors `apply_auth` in pulpo-core. */
export function applyAuth(url: string, headers: Header[], auth: Auth | null | undefined): string {
  if (!auth) return url;
  const has = (name: string) => headers.some((h) => h.enabled && h.name.trim().toLowerCase() === name.toLowerCase());
  switch (auth.type) {
    case "bearer":
      if (!has("authorization")) headers.push({ name: "Authorization", value: `Bearer ${auth.token}`, enabled: true });
      return url;
    case "basic":
      if (!has("authorization")) {
        headers.push({ name: "Authorization", value: `Basic ${base64Utf8(`${auth.username}:${auth.password}`)}`, enabled: true });
      }
      return url;
    case "apiKey": {
      if (auth.in === "header") {
        if (!has(auth.key)) headers.push({ name: auth.key, value: auth.value, enabled: true });
        return url;
      }
      const hashAt = url.indexOf("#");
      const base = hashAt >= 0 ? url.slice(0, hashAt) : url;
      const hash = hashAt >= 0 ? url.slice(hashAt) : "";
      const pair = `${encodeKeepingVars(auth.key)}=${encodeKeepingVars(auth.value)}`;
      return `${base}${base.includes("?") ? "&" : "?"}${pair}${hash}`;
    }
    default:
      return url;
  }
}

/** Applies `resolve` to every text field of the auth. */
export function resolveAuth(auth: Auth, resolve: (text: string) => string): Auth {
  switch (auth.type) {
    case "bearer":
      return { ...auth, token: resolve(auth.token) };
    case "basic":
      return { ...auth, username: resolve(auth.username), password: resolve(auth.password) };
    case "apiKey":
      return { ...auth, key: resolve(auth.key), value: resolve(auth.value) };
    default:
      return auth;
  }
}

export type FormKind = "urlencoded" | "multipart";

/** A form field. When `file` is true, `value` is a path relative to the workspace. */
export interface FormField {
  name: string;
  value: string;
  file: boolean;
  enabled: boolean;
}

export interface Form {
  kind: FormKind;
  fields: FormField[];
}

/** How the body is sent; derived from which of `form`, `bodyFile` and `body` is set. */
export type BodyMode = "none" | "raw" | FormKind | "file";

export function bodyMode(request: HttpRequest): BodyMode {
  if (request.form) return request.form.kind;
  if (request.bodyFile != null) return "file";
  return request.body != null ? "raw" : "none";
}

export interface HttpRequest {
  method: HttpMethod;
  url: string;
  headers: Header[];
  body: string | null;
  timeoutMs?: number;
  /** Query params kept but not sent; the enabled ones live in `url`. */
  disabledParams: Param[];
  /** Values for `:name` path segments. */
  pathParams: Param[];
  /** Own auth; `null`/missing = inherit from the folder. */
  auth?: Auth | null;
  /** A form body; wins over `bodyFile` and `body`. */
  form?: Form | null;
  /** A workspace file sent as the whole body; wins over `body`. */
  bodyFile?: string | null;
  /** Send and store cookies with the environment's jar (default true). */
  cookies?: boolean;
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
  kind: "invalidUrl" | "invalidHeader" | "timeout" | "connect" | "unresolved" | "file" | "other";
  message: string;
}

export const insideTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function coreInfo(): Promise<CoreInfo> {
  if (!insideTauri) {
    return { version: "browser preview", protocols: ["http", "graphql", "grpc", "websocket", "soap", "sse"] };
  }
  return invoke<CoreInfo>("core_info");
}

/** Sends a request. In the app, variables are resolved by pulpo-core with the chosen
 * environment (secrets come from the OS keychain). In the browser preview, `browserVars` is used. */
export async function sendHttp(
  request: HttpRequest,
  context: { root?: string; environmentId?: string; browserVars?: Vars } = {},
): Promise<HttpResponse> {
  if (insideTauri) {
    return invoke<HttpResponse>("send_http", { request, root: context.root, environmentId: context.environmentId });
  }
  const missing = new Set<string>();
  const vars = context.browserVars ?? {};
  const resolved: HttpRequest = {
    ...request,
    url: applyPathParams(
      resolveText(request.url, vars, missing),
      request.pathParams.map((p) => ({ name: p.name, value: resolveText(p.value, vars, missing) })),
    ),
    headers: request.headers.map((h) =>
      h.enabled ? { ...h, name: resolveText(h.name, vars, missing), value: resolveText(h.value, vars, missing) } : h,
    ),
    body: request.body == null ? null : resolveText(request.body, vars, missing),
    form: request.form
      ? {
          kind: request.form.kind,
          fields: request.form.fields
            .filter((f) => f.enabled && f.name)
            .map((f) => ({ ...f, name: resolveText(f.name, vars, missing), value: resolveText(f.value, vars, missing) })),
        }
      : null,
  };
  const auth = request.auth ? resolveAuth(request.auth, (text) => resolveText(text, vars, missing)) : null;
  resolved.url = applyAuth(resolved.url, resolved.headers, auth);
  if (missing.size) {
    throw { kind: "unresolved", message: [...missing].sort().join(", ") } satisfies SendError;
  }
  if (resolved.bodyFile != null || resolved.form?.fields.some((f) => f.file)) {
    throw {
      kind: "file",
      message: "Uploading files works in the desktop app; the browser preview cannot read your workspace.",
    } satisfies SendError;
  }
  return sendWithFetch(resolved);
}

async function sendWithFetch(request: HttpRequest): Promise<HttpResponse> {
  const headers = new Headers();
  for (const h of request.headers) {
    if (h.enabled && h.name.trim()) headers.append(h.name.trim(), h.value);
  }
  let payload: BodyInit | undefined = request.body ?? undefined;
  if (request.form?.kind === "urlencoded") {
    payload = new URLSearchParams(request.form.fields.map((f) => [f.name, f.value]));
  } else if (request.form?.kind === "multipart") {
    // The browser writes the boundary in Content-Type, so a hand-written one must go.
    headers.delete("content-type");
    const data = new FormData();
    for (const f of request.form.fields) data.append(f.name, f.value);
    payload = data;
  }
  const started = performance.now();
  let response: Response;
  try {
    response = await fetch(request.url, {
      method: request.method,
      headers,
      body: payload && !["GET", "HEAD"].includes(request.method) ? payload : undefined,
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
