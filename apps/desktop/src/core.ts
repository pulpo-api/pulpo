import { invoke } from "@tauri-apps/api/core";
import { resolveText, type Vars } from "./variables";
import { applyPathParams, type Param } from "./params";
import { bytesToBase64, isBinaryType } from "./responseFiles";

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
  | { type: "apiKey"; key: string; value: string; in: ApiKeyLocation }
  | ({ type: "oauth2" } & OAuth2)
  | { type: "digest"; username: string; password: string }
  | { type: "awsV4"; accessKey: string; secretKey: string; sessionToken: string; region: string; service: string }
  | { type: "hawk"; id: string; key: string; algorithm: "sha256" | "sha1"; ext: string; payload: boolean }
  | { type: "ntlm"; username: string; password: string; domain: string; workstation: string };

/** Auth computed from the request or a server challenge: only the desktop app sends it. */
export const SIGNED_AUTH = ["digest", "awsV4", "hawk", "ntlm"] as const;

export type OAuthGrant = "authorizationCode" | "clientCredentials" | "password";

/** OAuth 2.0 settings. Mirrors `OAuth2` in pulpo-core. */
export interface OAuth2 {
  grant: OAuthGrant;
  authUrl: string;
  tokenUrl: string;
  clientId: string;
  clientSecret: string;
  scope: string;
  username: string;
  password: string;
  /** Empty = http://127.0.0.1:<free port>/callback. */
  redirectUri: string;
  pkce: boolean;
  clientAuth: "header" | "body";
  headerPrefix: string;
  /** Token revocation endpoint (RFC 7009), for "Revoke" in the tokens panel. */
  revokeUrl: string;
}

export type OAuthAuth = { type: "oauth2" } & OAuth2;

export const OAUTH_GRANTS: Record<OAuthGrant, string> = {
  authorizationCode: "Authorization code (browser login)",
  clientCredentials: "Client credentials",
  password: "Password",
};

export function newOAuth2(): OAuthAuth {
  return {
    type: "oauth2",
    grant: "authorizationCode",
    authUrl: "",
    tokenUrl: "",
    clientId: "{{clientId}}",
    clientSecret: "",
    scope: "",
    username: "",
    password: "",
    redirectUri: "",
    pkce: true,
    clientAuth: "header",
    headerPrefix: "Bearer",
    revokeUrl: "",
  };
}

/** What Pulpo knows about a cached token (never the token itself). Mirrors `TokenInfo`. */
export interface TokenInfo {
  key: string;
  grant: OAuthGrant;
  tokenUrl: string;
  clientId: string;
  scope: string;
  obtainedAt: number;
  expiresAt: number | null;
  valid: boolean;
  hasRefresh: boolean;
  preview: string;
}

export interface Discovery {
  authorizationEndpoint: string;
  tokenEndpoint: string;
  revocationEndpoint: string;
  scopesSupported: string[];
}

/** A cached token and the requests whose auth uses it. Mirrors `ActiveToken`. */
export interface ActiveToken extends TokenInfo {
  usedBy: string[];
  canRenew: boolean;
  canRevoke: boolean;
}

/** Every cached OAuth 2.0 token of an environment (empty in the browser preview). */
export async function oauthTokens(root: string, environmentId: string): Promise<ActiveToken[]> {
  if (!insideTauri) return [];
  return invoke<ActiveToken[]>("oauth_tokens", { root, environmentId: environmentId || null });
}

/** Gets a fresh token now: refresh token, token request or browser login. */
export async function oauthRenew(root: string, environmentId: string, key: string): Promise<TokenInfo> {
  if (!insideTauri) throw new Error(OAUTH_DESKTOP_ONLY);
  return invoke<TokenInfo>("oauth_renew", { root, environmentId: environmentId || null, key });
}

/** Revokes a token at the identity provider (when it can) and forgets it; returns a note
 * when it was only forgotten. */
export async function oauthRevoke(root: string, environmentId: string, key: string): Promise<string | null> {
  if (!insideTauri) throw new Error(OAUTH_DESKTOP_ONLY);
  return invoke<string | null>("oauth_revoke", { root, environmentId: environmentId || null, key });
}

export async function oauthForgetKey(root: string, environmentId: string, key: string): Promise<void> {
  if (!insideTauri) return;
  await invoke("oauth_forget_key", { root, environmentId: environmentId || null, key });
}

function oauthConfig(auth: OAuthAuth): OAuth2 {
  const { type: _type, ...config } = auth;
  return config;
}

export const OAUTH_DESKTOP_ONLY = "OAuth 2.0 tokens are obtained by the desktop app; the browser preview cannot.";

/** Gets a new token: browser login for the authorization code grant, a token request otherwise. */
export async function oauthLogin(
  root: string,
  environmentId: string,
  auth: OAuthAuth,
  settings?: Settings,
): Promise<TokenInfo> {
  if (!insideTauri) throw new Error(OAUTH_DESKTOP_ONLY);
  return invoke<TokenInfo>("oauth_login", {
    root,
    environmentId: environmentId || null,
    config: oauthConfig(auth),
    settings: settings ?? null,
  });
}

/** The cached token for this auth in the environment, if any. */
export async function oauthStatus(root: string, environmentId: string, auth: OAuthAuth): Promise<TokenInfo | null> {
  if (!insideTauri) return null;
  return invoke<TokenInfo | null>("oauth_status", { root, environmentId: environmentId || null, config: oauthConfig(auth) });
}

export async function oauthForget(root: string, environmentId: string, auth: OAuthAuth): Promise<void> {
  if (!insideTauri) return;
  await invoke("oauth_forget", { root, environmentId: environmentId || null, config: oauthConfig(auth) });
}

/** Reads an OpenID Connect discovery document to fill the URLs. */
export async function oauthDiscover(issuer: string): Promise<Discovery> {
  if (insideTauri) return invoke<Discovery>("oauth_discover", { issuer });
  const url = issuer.includes("/.well-known/")
    ? issuer
    : `${issuer.trim().replace(/\/+$/, "")}/.well-known/openid-configuration`;
  const doc = await (await fetch(url)).json();
  return {
    authorizationEndpoint: doc.authorization_endpoint ?? "",
    tokenEndpoint: doc.token_endpoint ?? "",
    revocationEndpoint: doc.revocation_endpoint ?? "",
    scopesSupported: doc.scopes_supported ?? [],
  };
}

export const AUTH_LABELS: Record<Auth["type"], string> = {
  none: "No auth",
  bearer: "Bearer token",
  basic: "Basic",
  apiKey: "API key",
  oauth2: "OAuth 2.0",
  digest: "Digest",
  awsV4: "AWS Signature",
  hawk: "Hawk",
  ntlm: "NTLM",
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
    case "digest":
      return { ...auth, username: resolve(auth.username), password: resolve(auth.password) };
    case "awsV4":
      return {
        ...auth,
        accessKey: resolve(auth.accessKey),
        secretKey: resolve(auth.secretKey),
        sessionToken: resolve(auth.sessionToken),
        region: resolve(auth.region),
        service: resolve(auth.service),
      };
    case "hawk":
      return { ...auth, id: resolve(auth.id), key: resolve(auth.key), ext: resolve(auth.ext) };
    case "ntlm":
      return {
        ...auth,
        username: resolve(auth.username),
        password: resolve(auth.password),
        domain: resolve(auth.domain),
        workstation: resolve(auth.workstation),
      };
    case "oauth2":
      return {
        ...auth,
        authUrl: resolve(auth.authUrl),
        tokenUrl: resolve(auth.tokenUrl),
        clientId: resolve(auth.clientId),
        clientSecret: resolve(auth.clientSecret),
        scope: resolve(auth.scope),
        username: resolve(auth.username),
        password: resolve(auth.password),
        redirectUri: resolve(auth.redirectUri),
        revokeUrl: resolve(auth.revokeUrl ?? ""),
        headerPrefix: resolve(auth.headerPrefix),
      };
    default:
      return auth;
  }
}

/** How a request is sent; unset fields are inherited. Mirrors `Settings` in pulpo-core. */
export interface Settings {
  timeoutMs?: number;
  verifySsl?: boolean;
  followRedirects?: boolean;
  /** "system" (default), "none" or a proxy URL. */
  proxy?: string;
}

export const DEFAULT_TIMEOUT_MS = 30_000;

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
export type BodyMode = "none" | "raw" | "graphql" | FormKind | "file";

export function bodyMode(request: HttpRequest): BodyMode {
  if (request.graphql) return "graphql";
  if (request.form) return request.form.kind;
  if (request.bodyFile != null) return "file";
  return request.body != null ? "raw" : "none";
}

export interface HttpRequest {
  method: HttpMethod;
  url: string;
  headers: Header[];
  body: string | null;
  /** Own settings; unset fields come from the folders and the workspace. */
  settings?: Settings;
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
  /** A GraphQL operation, sent as `{query, variables, operationName}`; wins over `body`. */
  graphql?: Graphql | null;
  /** WebSocket requests (`ws://`, `wss://`): saved messages, in order. */
  messages?: WsMessage[];
}

export interface WsMessage {
  name: string;
  body: string;
}

/** `ws://` and `wss://` URLs are WebSocket requests. */
export function isWebSocketUrl(url: string): boolean {
  return /^\s*wss?:\/\//i.test(url);
}

export interface Graphql {
  query: string;
  /** JSON text. */
  variables: string;
  operationName?: string | null;
}

export interface HttpResponse {
  status: number;
  statusText: string;
  httpVersion: string;
  headers: Header[];
  body: string;
  bodyIsText: boolean;
  /** The exact bytes of binary bodies (images, PDF...), in base64. */
  bodyBase64?: string;
  sizeBytes: number;
  durationMs: number;
  /** The URL that answered, after redirects. */
  url?: string;
  redirects?: { status: number; url: string; location: string }[];
  timings?: { dnsMs: number; connectMs: number; waitMs: number; downloadMs: number };
  /** The request as it went out (headers the client adds included). */
  sent?: { method: string; url: string; headers: Header[]; body: string | null };
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

/** The standard introspection query (as in pulpo-core's graphql.rs). */
export const INTROSPECTION_QUERY = `query IntrospectionQuery {
  __schema {
    queryType { name } mutationType { name } subscriptionType { name }
    types { ...FullType }
    directives { name description locations args { ...InputValue } }
  }
}
fragment FullType on __Type {
  kind name description
  fields(includeDeprecated: true) { name description args { ...InputValue } type { ...TypeRef } isDeprecated deprecationReason }
  inputFields { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason }
  possibleTypes { ...TypeRef }
}
fragment InputValue on __InputValue { name description type { ...TypeRef } defaultValue }
fragment TypeRef on __Type {
  kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } }
}`;

/** What a GraphQL operation sends: JSON `{query, variables, operationName}`, or the bare
 * query when the request says `Content-Type: application/graphql` (as pulpo-core does). */
export function graphqlWireBody(request: HttpRequest): string | null {
  const g = request.graphql;
  if (!g) return null;
  const raw = request.headers.some(
    (h) => h.enabled && h.name.trim().toLowerCase() === "content-type" && h.value.toLowerCase().includes("graphql"),
  );
  if (raw) return g.query;
  const out: Record<string, unknown> = { query: g.query };
  const variables = (g.variables ?? "").trim();
  if (variables) {
    try {
      out.variables = JSON.parse(variables);
    } catch {
      out.variables = variables;
    }
  }
  if (g.operationName?.trim()) out.operationName = g.operationName;
  return JSON.stringify(out);
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
    graphql: request.graphql
      ? {
          query: resolveText(request.graphql.query, vars, missing),
          variables: resolveText(request.graphql.variables ?? "", vars, missing),
          operationName: request.graphql.operationName ?? null,
        }
      : null,
    form: request.form
      ? {
          kind: request.form.kind,
          fields: request.form.fields
            .filter((f) => f.enabled && f.name)
            .map((f) => ({ ...f, name: resolveText(f.name, vars, missing), value: resolveText(f.value, vars, missing) })),
        }
      : null,
  };
  if (request.auth?.type === "oauth2") {
    throw { kind: "other", message: OAUTH_DESKTOP_ONLY } satisfies SendError;
  }
  if (request.auth && (SIGNED_AUTH as readonly string[]).includes(request.auth.type)) {
    throw {
      kind: "other",
      message: `${AUTH_LABELS[request.auth.type]} auth is computed by the desktop app; the browser preview cannot send it.`,
    } satisfies SendError;
  }
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

const REASONS: Record<number, string> = {
  200: "OK",
  201: "Created",
  202: "Accepted",
  204: "No Content",
  301: "Moved Permanently",
  302: "Found",
  304: "Not Modified",
  307: "Temporary Redirect",
  308: "Permanent Redirect",
  400: "Bad Request",
  401: "Unauthorized",
  403: "Forbidden",
  404: "Not Found",
  405: "Method Not Allowed",
  409: "Conflict",
  422: "Unprocessable Entity",
  429: "Too Many Requests",
  500: "Internal Server Error",
  502: "Bad Gateway",
  503: "Service Unavailable",
  504: "Gateway Timeout",
};

async function sendWithFetch(request: HttpRequest): Promise<HttpResponse> {
  const headers = new Headers();
  for (const h of request.headers) {
    if (h.enabled && h.name.trim()) headers.append(h.name.trim(), h.value);
  }
  // The browser preview can only apply the timeout; TLS, proxy and redirects need the app.
  const timeout = AbortSignal.timeout(request.settings?.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  let payload: BodyInit | undefined = request.body ?? undefined;
  const graphqlBody = graphqlWireBody(request);
  if (graphqlBody != null) {
    payload = graphqlBody;
    if (!headers.has("content-type")) headers.set("content-type", "application/json");
  }
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
      signal: timeout,
    });
  } catch (e) {
    if (e instanceof DOMException && e.name === "TimeoutError") {
      throw {
        kind: "timeout",
        message: `no response after ${request.settings?.timeoutMs ?? DEFAULT_TIMEOUT_MS} ms`,
      } satisfies SendError;
    }
    throw {
      kind: "connect",
      message: `${e}. In the browser preview, CORS can block requests; the desktop app has no such limit.`,
    } satisfies SendError;
  }
  const buffer = await response.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let bodyIsText = true;
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    bodyIsText = false;
  }
  const body = new TextDecoder().decode(bytes);
  const binary = !bodyIsText || isBinaryType(response.headers.get("content-type") ?? "");
  return {
    status: response.status,
    // HTTP/2 has no reason phrase; reqwest fills the standard one, so do the same.
    statusText: response.statusText || REASONS[response.status] || "",
    httpVersion: "HTTP/1.1",
    headers: [...response.headers.entries()].map(([name, value]) => ({ name, value, enabled: true })),
    body,
    bodyIsText,
    bodyBase64: binary && bytes.length <= 25 * 1024 * 1024 ? bytesToBase64(bytes) : undefined,
    url: response.url,
    sent: {
      method: request.method,
      url: request.url,
      headers: [
        ...[...headers.entries()].map(([name, value]) => ({ name, value, enabled: true })),
        { name: "(browser)", value: "the browser adds its own User-Agent, Origin and others", enabled: true },
      ],
      body:
        typeof payload === "string"
          ? payload
          : payload instanceof URLSearchParams
            ? payload.toString()
            : payload instanceof FormData
              ? [...payload.entries()].map(([k, v]) => `${k}=${v}`).join("\n")
              : null,
    },
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
