import { invoke } from "@tauri-apps/api/core";
import {
  insideTauri,
  sendHttp,
  INTROSPECTION_QUERY,
  SseParser,
  asSendError,
  type Auth,
  type Header,
  type HttpRequest,
  type HttpResponse,
  type SendError,
  type Settings,
} from "./core";
import { sampleEnvironments, sampleWorkspace } from "./sample";
import { resolveText } from "./variables";
import { base64ToBytes } from "./responseFiles";

export type Protocol = "http" | "graphql" | "grpc" | "websocket" | "soap" | "sse" | "socketio" | "mqtt";

export interface Scripts {
  pre: string;
  test: string;
}

export interface RequestEntry {
  id: string;
  name: string;
  protocol: Protocol;
  request: HttpRequest;
  scripts: Scripts;
}

export interface TestResult {
  name: string;
  passed: boolean;
  error: string | null;
}

export interface RunReport {
  id: string;
  name: string;
  response: HttpResponse | null;
  error: SendError | null;
  tests: TestResult[];
  logs: string[];
  scriptErrors: string[];
  /** 0-based iteration in a run with several. */
  iteration?: number;
  /** Times it was sent (more than 1 after retries). */
  attempts?: number;
}

/** A row of a data file: `{{name}}` and `pm.iterationData` for one iteration. */
export type DataRow = Record<string, string>;

/** How one request runs inside the runner. Mirrors pulpo-core's RunOptions. */
export interface RunOptions {
  iteration: number;
  iterations: number;
  data: DataRow;
  retries: number;
  updateSnapshots: boolean;
}

export interface LoadOptions {
  users: number;
  durationSecs: number;
  rampUpSecs: number;
  data: DataRow[];
}

export interface LoadStats {
  name: string;
  count: number;
  failed: number;
  minMs: number;
  avgMs: number;
  p50Ms: number;
  p90Ms: number;
  p95Ms: number;
  p99Ms: number;
  maxMs: number;
  perSecond: number;
}

export interface LoadSummary {
  users: number;
  elapsedMs: number;
  total: LoadStats;
  requests: LoadStats[];
  errors: string[];
}

export interface LoadProgress {
  elapsedSecs: number;
  done: number;
  failed: number;
}

export function passed(report: RunReport): boolean {
  return !report.error && report.scriptErrors.length === 0 && report.tests.every((t) => t.passed);
}

export type TreeNode =
  | ({ type: "request" } & RequestEntry)
  | {
      type: "folder";
      id: string;
      name: string;
      children: TreeNode[];
      auth?: Auth | null;
      settings?: Settings;
      headers?: Header[];
    };

export interface Workspace {
  root: string;
  name: string;
  tree: TreeNode[];
  /** Auth for every request unless a folder or the request sets its own. */
  auth?: Auth | null;
  /** Settings for every request unless a folder or the request sets them. */
  settings?: Settings;
  /** Headers every request sends unless a folder or the request sets the same one. */
  headers?: Header[];
}

/** What a folder (or the workspace) sets for the requests inside. Mirrors pulpo-core. */
export interface FolderMeta {
  name: string;
  headers: Header[];
  auth: Auth | null;
  settings: Settings;
}

/** The folders above an id, from the workspace down (the id itself is not included). */
function levelsAbove(workspace: Workspace, id: string): { id: string; name: string; headers?: Header[] }[] {
  const levels: { id: string; name: string; headers?: Header[] }[] = [
    { id: "", name: workspace.name, headers: workspace.headers },
  ];
  const parts = id.split("/").slice(0, -1);
  let nodes = workspace.tree;
  for (let i = 0; i < parts.length; i++) {
    const folderId = parts.slice(0, i + 1).join("/");
    const folder = nodes.find((n): n is Extract<TreeNode, { type: "folder" }> => n.type === "folder" && n.id === folderId);
    if (!folder) break;
    levels.push({ id: folder.id, name: folder.name, headers: folder.headers });
    nodes = folder.children;
  }
  return levels;
}

/** Headers inherited by a request or folder, with the folder that sets each one. A deeper
 * folder replaces a header with the same name. Mirrors `effective_headers` in pulpo-core. */
export function inheritedHeaders(workspace: Workspace, id: string): { header: Header; from: string }[] {
  let out: { header: Header; from: string }[] = [];
  const same = (a: Header, b: Header) => a.name.trim().toLowerCase() === b.name.trim().toLowerCase();
  for (const level of levelsAbove(workspace, id)) {
    for (const header of level.headers ?? []) {
      out = out.filter((h) => !same(h.header, header));
      if (header.enabled && header.name.trim()) out.push({ header, from: level.name });
    }
  }
  return out;
}

/** The headers a request sends: inherited ones it does not set itself, then its own. */
export function effectiveHeaders(workspace: Workspace, entry: RequestEntry): Header[] {
  const own = entry.request.headers;
  const same = (a: Header, b: Header) => a.name.trim().toLowerCase() === b.name.trim().toLowerCase();
  return [
    ...inheritedHeaders(workspace, entry.id)
      .map((h) => h.header)
      .filter((h) => !own.some((o) => same(o, h))),
    ...own,
  ];
}

/** A setting a request inherits, and who sets it ("" = nobody: the default applies). */
export type InheritedSettings = { [K in keyof Settings]-?: { value: Settings[K]; from: string } };

/** What each setting would be without the request's own: the nearest folder that sets it,
 * else the workspace. Mirrors `effective_settings` in pulpo-core. */
export function inheritedSettings(workspace: Workspace, requestId: string): InheritedSettings {
  const levels: { name: string; settings?: Settings }[] = [{ name: workspace.name, settings: workspace.settings }];
  const parts = requestId.split("/").slice(0, -1);
  let nodes = workspace.tree;
  for (let i = 0; i < parts.length; i++) {
    const id = parts.slice(0, i + 1).join("/");
    const folder = nodes.find((n): n is Extract<TreeNode, { type: "folder" }> => n.type === "folder" && n.id === id);
    if (!folder) break;
    levels.push({ name: folder.name, settings: folder.settings });
    nodes = folder.children;
  }
  const pick = <K extends keyof Settings>(key: K) => {
    const level = [...levels].reverse().find((l) => l.settings?.[key] != null);
    return { value: level?.settings?.[key], from: level?.name ?? "" };
  };
  return {
    timeoutMs: pick("timeoutMs"),
    verifySsl: pick("verifySsl"),
    followRedirects: pick("followRedirects"),
    proxy: pick("proxy"),
  };
}

export type QueryLanguage = "jsonpath" | "jq";

/** A saved response of a request (`<request>.examples/` next to it). Mirrors pulpo-core. */
export interface Example {
  /** File stem, empty for a new one. */
  id: string;
  name: string;
  status: number;
  headers: Header[];
  body: string;
}

/** JSONPath for the browser preview only: $, .key, ['key'], [n], [*] and ..key. The app
 * runs full RFC 9535 JSONPath and jq in pulpo-core. */
function previewJsonPath(root: unknown, expression: string): unknown[] {
  let nodes: unknown[] = [root];
  const tokens = expression.trim().replace(/^\$/, "").match(/\.\.[\w$-]+|\.[\w$-]+|\[\*\]|\.\*|\[\d+\]|\['[^']*'\]/g) ?? [];
  if (tokens.join("") !== expression.trim().replace(/^\$/, "")) {
    throw new Error("The browser preview understands simple paths only ($.a.b[0], [*], ..name); the app runs full JSONPath");
  }
  const children = (n: unknown): unknown[] =>
    Array.isArray(n) ? n : n && typeof n === "object" ? Object.values(n as Record<string, unknown>) : [];
  const descendants = (n: unknown): unknown[] => [n, ...children(n).flatMap(descendants)];
  for (const token of tokens) {
    if (token.startsWith("..")) {
      const key = token.slice(2);
      nodes = nodes.flatMap(descendants).flatMap((n) =>
        n && typeof n === "object" && !Array.isArray(n) && key in (n as object) ? [(n as Record<string, unknown>)[key]] : [],
      );
    } else if (token === "[*]" || token === ".*") {
      nodes = nodes.flatMap(children);
    } else if (token.startsWith("[") && /\d/.test(token[1])) {
      const i = Number(token.slice(1, -1));
      nodes = nodes.flatMap((n) => (Array.isArray(n) && i < n.length ? [n[i]] : []));
    } else {
      const key = token.startsWith("[") ? token.slice(2, -2) : token.slice(1);
      nodes = nodes.flatMap((n) =>
        n && typeof n === "object" && !Array.isArray(n) && key in (n as object) ? [(n as Record<string, unknown>)[key]] : [],
      );
    }
  }
  return nodes;
}

export type CodeLanguage = "curl" | "fetch" | "axios" | "python" | "java" | "go" | "csharp" | "powershell";

export const CODE_LANGUAGES: Record<CodeLanguage, string> = {
  curl: "curl (bash)",
  fetch: "JavaScript fetch",
  axios: "JavaScript axios",
  python: "Python requests",
  java: "Java HttpClient",
  go: "Go net/http",
  csharp: "C# HttpClient",
  powershell: "PowerShell",
};

export interface CodeOptions {
  /** Replace {{variables}} with the environment's values. */
  resolve: boolean;
  /** Also secrets and values set by scripts (usually tokens). */
  includeSecrets: boolean;
}

/** A curl for the browser preview (no Rust there): unresolved, auth and folder headers in. */
function previewCurl(workspace: Workspace, entry: RequestEntry): string {
  const r = entry.request;
  const quote = (text: string) => `'${text.replace(/'/g, "'\\''")}'`;
  const parts = [`curl ${quote(r.url)}`];
  if (r.method !== "GET") parts.push(`-X ${r.method}`);
  const headers = effectiveHeaders(workspace, entry).filter((h) => h.enabled && h.name.trim());
  for (const h of headers) parts.push(`-H ${quote(`${h.name}: ${h.value}`)}`);
  const auth = r.auth ?? inheritedAuth(workspace, entry.id).auth;
  if (auth.type === "bearer") parts.push(`-H ${quote(`Authorization: Bearer ${auth.token}`)}`);
  if (auth.type === "basic") parts.push(`-u ${quote(`${auth.username}:${auth.password}`)}`);
  if (auth.type === "apiKey" && auth.in === "header") parts.push(`-H ${quote(`${auth.key}: ${auth.value}`)}`);
  if (r.form) {
    for (const f of r.form.fields.filter((f) => f.enabled && f.name)) {
      parts.push(r.form.kind === "urlencoded" ? `--data-urlencode ${quote(`${f.name}=${f.value}`)}` : `-F ${quote(`${f.name}=${f.file ? "@" : ""}${f.value}`)}`);
    }
  } else if (r.bodyFile) parts.push(`--data-binary ${quote(`@${r.bodyFile}`)}`);
  else if (r.body) parts.push(`--data-raw ${quote(r.body)}`);
  parts.push("-L");
  return `# Browser preview: path params and query API keys are applied by the desktop app.\n${parts.join(" \\\n  ")}`;
}

/** What an organize operation did: the node's id afterwards and every id that changed
 * (old, new). Ids inside a renamed folder change the same way. */
export interface Organized {
  id: string;
  renamed: [string, string][];
}

/** Applies the id changes of an organize operation to one id. */
export function remapId(id: string, renamed: [string, string][]): string {
  for (const [from, to] of renamed) {
    if (id === from) return to;
    if (id.startsWith(`${from}/`)) return to + id.slice(from.length);
  }
  return id;
}

/** A client certificate (mTLS) of this machine, in `.pulpo/certificates.json`. */
export interface ClientCertificate {
  /** api.bank.test, api.bank.test:8443 or *.bank.test */
  host: string;
  cert?: string;
  key?: string;
  pfx?: string;
  /** Usually a {{secret variable}} of the environment. */
  passphrase?: string;
}

/** The certificate the desktop app would use for a URL (exact host first, then *.domain).
 * Mirrors `certificates::find` in pulpo-core. */
export function certificateFor(all: ClientCertificate[], url: string): ClientCertificate | null {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  const host = parsed.hostname.toLowerCase();
  const port = parsed.port || (parsed.protocol === "https:" ? "443" : "80");
  const matches = (c: ClientCertificate, wildcard: boolean) => {
    const [name, entryPort] = c.host.trim().toLowerCase().split(/:(?=\d+$)/);
    if (entryPort && entryPort !== port) return false;
    return name.startsWith("*.") ? wildcard && host.endsWith(name.slice(1)) : !wildcard && host === name;
  };
  return all.find((c) => matches(c, false)) ?? all.find((c) => matches(c, true)) ?? null;
}

/** Where a request's auth comes from when it does not set its own. */
export interface InheritedAuth {
  auth: Auth;
  /** Folder id, or "" for the workspace (pulpo.yaml). */
  sourceId: string;
  sourceName: string;
  /** False when nothing up the tree sets an auth. */
  found: boolean;
}

/** The auth a request inherits: the nearest folder that sets one, else the workspace.
 * Mirrors `effective_auth` in pulpo-core. */
export function inheritedAuth(workspace: Workspace, requestId: string): InheritedAuth {
  const parts = requestId.split("/").slice(0, -1);
  const folders: { id: string; name: string; auth?: Auth | null }[] = [];
  let nodes = workspace.tree;
  for (let i = 0; i < parts.length; i++) {
    const id = parts.slice(0, i + 1).join("/");
    const folder = nodes.find((n): n is Extract<TreeNode, { type: "folder" }> => n.type === "folder" && n.id === id);
    if (!folder) break;
    folders.push(folder);
    nodes = folder.children;
  }
  for (const folder of folders.reverse()) {
    if (folder.auth) return { auth: folder.auth, sourceId: folder.id, sourceName: folder.name, found: true };
  }
  if (workspace.auth) return { auth: workspace.auth, sourceId: "", sourceName: workspace.name, found: true };
  return { auth: { type: "none" }, sourceId: "", sourceName: workspace.name, found: false };
}

export interface Variable {
  name: string;
  value: string;
  secret: boolean;
  /** For secrets: whether a value is stored in the keychain. */
  hasValue: boolean;
}

export interface Environment {
  id: string;
  name: string;
  variables: Variable[];
}

/** A cookie of an environment's jar (`.pulpo/cookies.json`). Mirrors pulpo-core. */
export interface Cookie {
  name: string;
  value: string;
  domain: string;
  /** Only that exact host gets it back (the server sent no Domain). */
  hostOnly: boolean;
  path: string;
  /** Unix seconds; null = session cookie. */
  expires: number | null;
  secure: boolean;
  httpOnly: boolean;
  sameSite: string | null;
}

export type ExportFormat = "postman" | "openApi" | "http";

export interface Contract {
  path: string;
  kind: "openapi" | "swagger" | "proto" | "wsdl" | "graphql";
  title: string;
  operations: number;
  supported: boolean;
  folder: string | null;
  note?: string;
}

export interface DriftItem {
  key: string;
  name: string;
  method: string;
  url: string;
  target: string;
  changes: string[];
}

export interface Drift {
  folder: string;
  source: string;
  title: string;
  added: DriftItem[];
  removed: DriftItem[];
  changed: DriftItem[];
  unchanged: number;
  repeated: number;
  warnings: string[];
}

export interface Flow {
  id: string;
  version: number;
  name: string;
  description: string;
  steps: unknown[];
}

export interface GraphqlSchema {
  /** `{"__schema": …}` as JSON text. */
  introspection: string;
  added: string[];
  removed: string[];
  compared: boolean;
  fetchedAt: number;
  cached: boolean;
}

/** One line of a WebSocket conversation. */
export interface WsFrame {
  direction: "sent" | "received" | "info" | "error";
  text: string;
  binary?: boolean;
  /** Milliseconds since the connection opened. */
  atMs: number;
}

export interface MockRoute {
  method: string;
  /** `/orders/:id`. */
  path: string;
  name: string;
  /** `example` or `openapi`. */
  source: string;
  answers: { name: string; status: number; headers: Header[]; body: string }[];
}

export interface MockHit {
  method: string;
  path: string;
  status: number;
  /** `Request · example`, empty when no route matched. */
  matched: string;
  atMs: number;
}

export interface SoapOperation {
  /** `Service/Port/Operation`. */
  path: string;
  service: string;
  port: string;
  name: string;
  address: string;
  soapAction: string;
  version: string;
  style: string;
  /** `{namespace}name` of the Body's first element. */
  bodyElement: string;
  envelope: string;
  headers: [string, string][];
  documentation?: string;
}

export interface GrpcMethod {
  /** `package.Service/Method`. */
  path: string;
  service: string;
  name: string;
  clientStreaming: boolean;
  serverStreaming: boolean;
  input: string;
  output: string;
  /** A JSON message with every field. */
  example: string;
}

export interface GrpcSchema {
  source: string;
  methods: GrpcMethod[];
}

/** A line of an MQTT session: a message received or sent, or Pulpo's own `info`/`error`. */
export interface MqttMessage {
  direction: "received" | "sent" | "info" | "error";
  topic: string;
  payload: string;
  qos?: number;
  retain?: boolean;
  atMs: number;
}

export interface MqttSession {
  publish(topic: string, payload: string, qos: number, retain: boolean): Promise<void>;
  close(): Promise<void>;
}

/** One event of a Server-Sent Events stream; `info`/`error` are Pulpo's own lines. */
export interface SseEvent {
  event: string;
  data: string;
  id?: string | null;
  atMs: number;
}

export interface SseStream {
  stop(): Promise<void>;
}

export interface WsConnection {
  send(text: string): Promise<void>;
  close(): Promise<void>;
}

export interface FlowPlanNode {
  path: string;
  depth: number;
  kind: "request" | "flow" | "wait" | "missing";
  condition?: string;
  forEach?: string;
  retry?: string;
  target: string;
  name: string;
  method: string;
  url: string;
  save: string[];
  expect: number | null;
  problem?: string;
}

export interface FlowStepEvent {
  path: string;
  state: "running" | "passed" | "failed" | "skipped";
  report?: RunReport;
  saved?: Record<string, string>;
  iteration?: string;
  note?: string;
}

export interface FlowReport {
  flow: string;
  passed: boolean;
  steps: FlowStepEvent[];
}

export interface LinkTarget {
  link: { repo: string; workspace: string; name: string; request: string };
  root: string | null;
}

export interface DiffItem {
  id: string;
  name: string;
  folder: string[];
  method: string;
  url: string;
}

export interface CollectionDiff {
  from: string;
  to: string;
  added: DiffItem[];
  removed: DiffItem[];
  changed: { item: DiffItem; was: DiffItem | null; changes: string[] }[];
  folders: string[];
  environments: string[];
}

export interface GitPanel {
  status: {
    repo: boolean;
    branch: string;
    upstream: string | null;
    ahead: number;
    behind: number;
    files: { path: string; state: string }[];
  };
  branches: string[];
  changes: CollectionDiff | null;
}

export interface Exported {
  text: string;
  fileName: string;
  requests: number;
  warnings: string[];
}

export interface ImportSummary {
  format:
    | "postmanCollection"
    | "postmanEnvironment"
    | "openApi"
    | "curl"
    | "insomnia"
    | "bruno"
    | "hoppscotch"
    | "har"
    | "httpFile"
    | null;
  folderId: string;
  folders: number;
  requests: number;
  environments: string[];
  warnings: string[];
}

export interface CollectionError {
  kind: "notAWorkspace" | "alreadyAWorkspace" | "invalidFile" | "invalidPath" | "io";
  message: string;
}

export function asCollectionError(error: unknown): CollectionError {
  if (error && typeof error === "object" && "kind" in error && "message" in error) return error as CollectionError;
  return { kind: "io", message: String(error) };
}

/** Plain deep copy. Works on Vue reactive proxies, unlike structuredClone. */
export function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

/** The request with every field present and in a fixed key order, so that comparing
 * snapshots (unsaved changes) does not depend on the order fields were added in. */
export function canonicalRequest(r: HttpRequest): HttpRequest {
  const s = r.settings ?? {};
  const settings: Settings = {};
  if (s.timeoutMs != null) settings.timeoutMs = s.timeoutMs;
  if (s.verifySsl != null) settings.verifySsl = s.verifySsl;
  if (s.followRedirects != null) settings.followRedirects = s.followRedirects;
  if (s.proxy != null) settings.proxy = s.proxy;
  return {
    method: r.method,
    url: r.url,
    headers: r.headers,
    body: r.body ?? null,
    form: r.form ?? null,
    bodyFile: r.bodyFile ?? null,
    disabledParams: r.disabledParams ?? [],
    pathParams: r.pathParams ?? [],
    auth: r.auth ?? null,
    cookies: r.cookies ?? true,
    settings,
    graphql: r.graphql
      ? { query: r.graphql.query, variables: r.graphql.variables ?? "", operationName: r.graphql.operationName ?? null }
      : null,
    messages: (r.messages ?? []).map((m) => ({ name: m.name, body: m.body })),
    grpc: r.grpc ? { method: r.grpc.method ?? "", protoFiles: [...(r.grpc.protoFiles ?? [])] } : null,
    soap: r.soap ? { wsdl: r.soap.wsdl ?? "", operation: r.soap.operation ?? "" } : null,
    socketio: r.socketio
      ? { namespace: r.socketio.namespace ?? "/", path: r.socketio.path ?? "/socket.io", auth: r.socketio.auth ?? "" }
      : null,
    mqtt: r.mqtt
      ? {
          clientId: r.mqtt.clientId ?? "",
          username: r.mqtt.username ?? "",
          password: r.mqtt.password ?? "",
          subscribe: [...(r.mqtt.subscribe ?? [])],
          qos: r.mqtt.qos ?? 0,
          retain: r.mqtt.retain ?? false,
        }
      : null,
  };
}

export function toEntry(node: Extract<TreeNode, { type: "request" }>): RequestEntry {
  const { id, name, protocol, request, scripts } = node;
  return clone({ id, name, protocol, request: canonicalRequest(request), scripts: scripts ?? { pre: "", test: "" } });
}

/** Where the workspace lives: real files in the desktop app, memory in the browser preview. */
export interface WorkspaceBackend {
  readonly persistent: boolean;
  pickFolder(): Promise<string | null>;
  open(root: string): Promise<Workspace>;
  init(root: string, name: string): Promise<Workspace>;
  save(root: string, entry: RequestEntry): Promise<void>;
  createRequest(root: string, folderId: string, name: string): Promise<RequestEntry>;
  createFolder(root: string, parentId: string, name: string): Promise<string>;
  deleteRequest(root: string, id: string): Promise<void>;
  listEnvironments(root: string): Promise<Environment[]>;
  saveEnvironment(root: string, env: Environment): Promise<void>;
  createEnvironment(root: string, name: string): Promise<Environment>;
  deleteEnvironment(root: string, id: string): Promise<void>;
  /** The request as code; folders give it their auth, headers and settings. */
  generateCode(
    root: string,
    entry: RequestEntry,
    environmentId: string,
    language: CodeLanguage,
    options: CodeOptions,
  ): Promise<string>;
  listExamples(root: string, requestId: string): Promise<Example[]>;
  /** Saves an example (new when its id is empty) and returns it with its id. */
  saveExample(root: string, requestId: string, example: Example): Promise<Example>;
  deleteExample(root: string, requestId: string, id: string): Promise<void>;
  /** Runs a JSONPath or jq query over a response body. */
  queryJson(body: string, language: QueryLanguage, expression: string): Promise<unknown[]>;
  /** Saves a response body to a file the user picks; returns where, or null if cancelled. */
  saveResponse(response: HttpResponse, suggestedName: string): Promise<string | null>;
  /** Values set by scripts for an environment ("" = none), from `.pulpo/local.json`. */
  localValues(root: string, environmentId: string): Promise<Record<string, string>>;
  /** What a folder ("" = the workspace) sets for the requests inside. */
  folderMeta(root: string, folderId: string): Promise<FolderMeta>;
  /** Saves headers, auth and settings of a folder (the name is kept). */
  saveFolderMeta(root: string, folderId: string, meta: FolderMeta): Promise<void>;
  folderScripts(root: string, folderId: string): Promise<Scripts>;
  saveFolderScripts(root: string, folderId: string, scripts: Scripts): Promise<void>;
  /** Renames a request or folder; its file name follows. */
  renameNode(root: string, id: string, name: string): Promise<Organized>;
  /** Moves a request or folder into `target` at `index` (counted without the node). */
  moveNode(root: string, id: string, target: string, index: number): Promise<Organized>;
  /** Copies a request or folder right after it. */
  duplicateNode(root: string, id: string): Promise<Organized>;
  deleteFolder(root: string, id: string): Promise<void>;
  /** Client certificates (mTLS) of this machine. */
  listCertificates(root: string): Promise<ClientCertificate[]>;
  saveCertificates(root: string, certificates: ClientCertificate[]): Promise<void>;
  /** Replaces the settings of a folder ("" = the workspace). */
  saveFolderSettings(root: string, folderId: string, settings: Settings): Promise<void>;
  /** The cookie jar of an environment ("" = no environment). */
  listCookies(root: string, environmentId: string): Promise<Cookie[]>;
  /** Replaces the cookie jar of an environment. */
  saveCookies(root: string, environmentId: string, cookies: Cookie[]): Promise<void>;
  /** Sets (or clears, with null) the auth of a folder; "" = the workspace. */
  saveFolderAuth(root: string, folderId: string, auth: Auth | null): Promise<void>;
  /** Desktop only: imports a file (or pasted text) with the Rust importers. */
  importInto?(root: string, folderId: string, source: { path?: string; text?: string }): Promise<ImportSummary>;
  pickImportFile?(): Promise<string | null>;
  /** Desktop only: a folder ("" = everything) as Postman, OpenAPI or a .http file. */
  exportCollection?(root: string, folderId: string, format: ExportFormat): Promise<Exported>;
  /** Desktop only: an environment as a Postman environment. */
  exportEnvironment?(root: string, environmentId: string): Promise<Exported>;
  /** Desktop only: finds which response feeds which request and saves it as a flow. */
  chainToFlow?(root: string, folderId: string, environmentId: string, run: boolean, name: string): Promise<[string, string]>;
  /** Desktop only: API contracts of the repository and the folders made from them. */
  scanContracts?(root: string): Promise<Contract[]>;
  syncStatus?(root: string): Promise<({ Ok: Drift } | { Err: string })[]>;
  syncLink?(root: string, folderId: string, contract: string): Promise<Drift>;
  syncImport?(root: string, contract: string): Promise<ImportSummary>;
  syncApply?(root: string, folderId: string, selection: { add: string[]; update: string[]; remove: string[] }): Promise<{
    added: string[];
    updated: string[];
    removed: string[];
  }>;
  /** Desktop only: flows (features told as a sequence of requests). */
  listFlows?(root: string): Promise<Flow[]>;
  flowPlan?(root: string, id: string): Promise<FlowPlanNode[]>;
  flowYaml?(root: string, id: string): Promise<string>;
  saveFlowYaml?(root: string, id: string, yaml: string): Promise<Flow>;
  createFlow?(root: string, name: string): Promise<Flow>;
  deleteFlow?(root: string, id: string): Promise<void>;
  addFlowStep?(root: string, flowId: string, requestId: string): Promise<Flow>;
  moveFlowStep?(root: string, flowId: string, from: number, to: number): Promise<Flow>;
  /** What the mock server would answer for a folder (`""` = the workspace). */
  mockRoutes?(root: string, folder: string): Promise<MockRoute[]>;
  mockStart?(root: string, folder: string, port: number): Promise<{ url: string; folder: string }>;
  mockStop?(): Promise<void>;
  mockStatus?(): Promise<{ url: string; folder: string } | null>;
  onMockHit?(handler: (hit: MockHit) => void): Promise<() => void>;
  /** Connects to an MQTT broker and subscribes; every message goes to `onMessage`. */
  mqttConnect?(root: string, entry: RequestEntry, environmentId: string, onMessage: (m: MqttMessage) => void): Promise<MqttSession>;
  /** The SOAP operations of a WSDL (workspace file or URL), with sample envelopes. */
  soapOperations?(root: string, wsdl: string): Promise<SoapOperation[]>;
  /** The methods of a gRPC request: by reflection, or from its `.proto` files. */
  grpcSchema?(root: string, entry: RequestEntry, environmentId: string): Promise<GrpcSchema>;
  /** Listens to a request's event stream (SSE); every event goes to `onEvent`. */
  sseListen?(root: string, entry: RequestEntry, environmentId: string, onEvent: (event: SseEvent) => void): Promise<SseStream>;
  /** Opens a WebSocket for a request; every frame (sent, received, closed) goes to `onFrame`. */
  wsConnect?(root: string, entry: RequestEntry, environmentId: string, onFrame: (frame: WsFrame) => void): Promise<WsConnection>;
  /** The schema of a GraphQL endpoint: cached (`refresh` false) or asked again. */
  graphqlSchema?(root: string, entry: RequestEntry, environmentId: string, refresh: boolean): Promise<GraphqlSchema | null>;
  flowUsage?(root: string): Promise<Record<string, string[]>>;
  runFlow?(root: string, id: string, environmentId: string): Promise<FlowReport>;
  onFlowStep?(handler: (event: FlowStepEvent) => void): Promise<() => void>;
  /** Desktop only: the pulpo:// link to a request. */
  requestLink?(root: string, requestId: string): Promise<string>;
  /** Desktop only: reads a pulpo:// link and finds its workspace among `roots`. */
  resolveLink?(link: string, roots: string[]): Promise<LinkTarget>;
  /** Desktop only: links the app was started with, then links clicked while it runs. */
  onLinks?(handler: (links: string[]) => void): Promise<void>;
  /** Desktop only: git status, branches and the collection's changes since HEAD. */
  gitPanel?(root: string): Promise<GitPanel>;
  gitCommit?(root: string, message: string): Promise<string>;
  gitSwitch?(root: string, branch: string, create: boolean): Promise<void>;
  gitPull?(root: string): Promise<string>;
  gitPush?(root: string): Promise<string>;
  /** Desktop only: asks where to save and writes the text; false when cancelled. */
  saveTextFile?(defaultName: string, text: string): Promise<boolean>;
  /** Desktop only: picks a file to upload and returns its absolute path. */
  pickUploadFile?(): Promise<string | null>;
  /** Sends the request with its scripts; `entry` may contain unsaved edits. */
  run(root: string, entry: RequestEntry, environmentId: string | undefined, options?: RunOptions): Promise<RunReport>;
  /** Desktop only: rows of a CSV or JSON data file (relative paths are inside the workspace). */
  loadData?(root: string, path: string): Promise<DataRow[]>;
  pickDataFile?(): Promise<string | null>;
  /** Desktop only: saves a run report (HTML or JUnit) where the user picks. */
  saveRunReport?(name: string, reports: RunReport[], format: "html" | "junit"): Promise<string | null>;
  /** Desktop only: a load test; `onProgress` about once a second. */
  loadTest?(
    root: string,
    entries: RequestEntry[],
    environmentId: string | undefined,
    options: LoadOptions,
    onProgress: (progress: LoadProgress) => void,
  ): Promise<LoadSummary>;
  stopLoadTest?(): Promise<void>;
  /** Only in the browser preview: the values used to resolve requests (secrets included). */
  browserVariables?(environmentId: string): Record<string, string>;
}

const tauriBackend: WorkspaceBackend = {
  persistent: true,
  async pickFolder() {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ directory: true, multiple: false, title: "Choose a workspace folder" });
    return typeof picked === "string" ? picked : null;
  },
  open: (root) => invoke<Workspace>("open_workspace", { root }),
  init: (root, name) => invoke<Workspace>("init_workspace", { root, name }),
  save: (root, entry) => invoke<void>("save_request", { root, entry }),
  createRequest: (root, folderId, name) => invoke<RequestEntry>("create_request", { root, folderId, name }),
  createFolder: (root, parentId, name) => invoke<string>("create_folder", { root, parentId, name }),
  deleteRequest: (root, id) => invoke<void>("delete_request", { root, id }),
  listEnvironments: (root) => invoke<Environment[]>("list_environments", { root }),
  saveEnvironment: (root, env) => invoke<void>("save_environment", { root, env }),
  createEnvironment: (root, name) => invoke<Environment>("create_environment", { root, name }),
  deleteEnvironment: (root, id) => invoke<void>("delete_environment", { root, id }),
  saveFolderAuth: (root, folderId, auth) => invoke<void>("save_folder_auth", { root, folderId, auth }),
  generateCode: (root, entry, environmentId, language, options) =>
    invoke<string>("generate_code", { root, entry, environmentId: environmentId || null, language, options }),
  queryJson: (body, language, expression) => invoke<unknown[]>("query_json", { body, language, expression }),
  listExamples: (root, requestId) => invoke<Example[]>("list_examples", { root, requestId }),
  saveExample: (root, requestId, example) => invoke<Example>("save_example", { root, requestId, example }),
  deleteExample: (root, requestId, id) => invoke<void>("delete_example", { root, requestId, id }),
  async saveResponse(response, suggestedName) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({ title: "Save the response", defaultPath: suggestedName });
    if (!path) return null;
    await invoke<void>("save_response", {
      path,
      text: response.bodyBase64 ? null : response.body,
      base64: response.bodyBase64 ?? null,
    });
    return path;
  },
  localValues: (root, environmentId) =>
    invoke<Record<string, string>>("local_values", { root, environmentId: environmentId || null }),
  folderMeta: (root, folderId) => invoke<FolderMeta>("folder_meta", { root, folderId }),
  saveFolderMeta: (root, folderId, meta) => invoke<void>("save_folder_meta", { root, folderId, meta }),
  folderScripts: (root, folderId) => invoke<Scripts>("folder_scripts", { root, folderId }),
  saveFolderScripts: (root, folderId, scripts) => invoke<void>("save_folder_scripts", { root, folderId, scripts }),
  renameNode: (root, id, name) => invoke<Organized>("rename_node", { root, id, name }),
  moveNode: (root, id, target, index) => invoke<Organized>("move_node", { root, id, target, index }),
  duplicateNode: (root, id) => invoke<Organized>("duplicate_node", { root, id }),
  deleteFolder: (root, id) => invoke<void>("delete_folder", { root, id }),
  listCertificates: (root) => invoke<ClientCertificate[]>("list_certificates", { root }),
  saveCertificates: (root, certificates) => invoke<void>("save_certificates", { root, certificates }),
  saveFolderSettings: (root, folderId, settings) => invoke<void>("save_folder_settings", { root, folderId, settings }),
  listCookies: (root, environmentId) => invoke<Cookie[]>("list_cookies", { root, environmentId: environmentId || null }),
  saveCookies: (root, environmentId, cookies) =>
    invoke<void>("save_cookies", { root, environmentId: environmentId || null, cookies }),
  run: (root, entry, environmentId, options) =>
    invoke<RunReport>("run_request", { root, entry, environmentId, options: options ?? null }),
  loadData: (root, path) => invoke<DataRow[]>("load_data", { root, path }),
  async pickDataFile() {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      multiple: false,
      title: "Choose a data file (one row per iteration)",
      filters: [{ name: "CSV or JSON", extensions: ["csv", "json"] }],
    });
    return typeof picked === "string" ? picked : null;
  },
  async saveRunReport(name, reports, format) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      title: format === "html" ? "Save the HTML report" : "Save the JUnit report",
      defaultPath: format === "html" ? "pulpo-report.html" : "pulpo-junit.xml",
    });
    if (!path) return null;
    const text = await invoke<string>("run_report", { name, reports, format });
    await invoke<void>("save_response", { path, text, base64: null });
    return path;
  },
  async loadTest(root, entries, environmentId, options, onProgress) {
    const { listen } = await import("@tauri-apps/api/event");
    const stop = await listen<LoadProgress>("load-progress", (event) => onProgress(event.payload));
    try {
      return await invoke<LoadSummary>("load_test", { root, entries, environmentId: environmentId || null, options });
    } finally {
      stop();
    }
  },
  stopLoadTest: () => invoke<void>("stop_load_test"),
  importInto: (root, folderId, source) => invoke<ImportSummary>("import_into", { root, folderId, ...source }),
  chainToFlow: (root, folderId, environmentId, run, name) =>
    invoke<[string, string]>("chain_to_flow", { root, folderId, environmentId: environmentId || null, run, name }),
  scanContracts: (root) => invoke<Contract[]>("scan_contracts", { root }),
  syncStatus: (root) => invoke<({ Ok: Drift } | { Err: string })[]>("sync_status", { root }),
  syncLink: (root, folderId, contract) => invoke<Drift>("sync_link", { root, folderId, contract }),
  syncImport: (root, contract) => invoke<ImportSummary>("sync_import", { root, contract }),
  syncApply: (root, folderId, selection) => invoke("sync_apply", { root, folderId, selection }),
  listFlows: (root) => invoke<Flow[]>("list_flows", { root }),
  flowPlan: (root, id) => invoke<FlowPlanNode[]>("flow_plan", { root, id }),
  flowYaml: (root, id) => invoke<string>("flow_yaml", { root, id }),
  saveFlowYaml: (root, id, yaml) => invoke<Flow>("save_flow_yaml", { root, id, yaml }),
  createFlow: (root, name) => invoke<Flow>("create_flow", { root, name }),
  deleteFlow: (root, id) => invoke<void>("delete_flow", { root, id }),
  addFlowStep: (root, flowId, requestId) => invoke<Flow>("add_flow_step", { root, flowId, requestId }),
  moveFlowStep: (root, flowId, from, to) => invoke<Flow>("move_flow_step", { root, flowId, from, to }),
  graphqlSchema: (root, entry, environmentId, refresh) =>
    invoke<GraphqlSchema | null>("graphql_schema", { root, entry, environmentId: environmentId || null, refresh }),
  soapOperations: (root, wsdl) => invoke<SoapOperation[]>("soap_operations", { root, wsdl }),
  mockRoutes: (root, folder) => invoke<MockRoute[]>("mock_routes", { root, folder }),
  mockStart: (root, folder, port) => invoke<{ url: string; folder: string }>("mock_start", { root, folder, port }),
  mockStop: () => invoke<void>("mock_stop"),
  mockStatus: () => invoke<{ url: string; folder: string } | null>("mock_status"),
  async onMockHit(handler) {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<MockHit>("mock-hit", (event) => handler(event.payload));
  },
  async mqttConnect(root, entry, environmentId, onMessage) {
    const { listen } = await import("@tauri-apps/api/event");
    let id: string | null = null;
    const early: { id: string; message: MqttMessage }[] = [];
    const deliver = (m: MqttMessage) => {
      onMessage(m);
      if (m.direction === "error" || (m.direction === "info" && /^(Disconnected|Closed)/.test(m.payload))) unlisten();
    };
    const unlisten = await listen<{ id: string; message: MqttMessage }>("mqtt-message", (e) => {
      if (id === null) early.push(e.payload);
      else if (e.payload.id === id) deliver(e.payload.message);
    });
    try {
      id = await invoke<string>("mqtt_connect", { root, entry, environmentId: environmentId || null });
    } catch (e) {
      unlisten();
      throw e;
    }
    for (const e of early) if (e.id === id) deliver(e.message);
    const session = id;
    return {
      publish: (topic, payload, qos, retain) => invoke<void>("mqtt_publish", { id: session, topic, payload, qos, retain }),
      close: () => invoke<void>("mqtt_close", { id: session }),
    };
  },
  grpcSchema: (root, entry, environmentId) =>
    invoke<GrpcSchema>("grpc_schema", { root, entry, environmentId: environmentId || null }),
  async sseListen(root, entry, environmentId, onEvent) {
    const { listen } = await import("@tauri-apps/api/event");
    let id: string | null = null;
    const early: { id: string; event: SseEvent }[] = [];
    const deliver = (event: SseEvent) => {
      onEvent(event);
      if (event.event === "error" || (event.event === "info" && !event.data.startsWith("Listening"))) unlisten();
    };
    const unlisten = await listen<{ id: string; event: SseEvent }>("sse-event", (e) => {
      if (id === null) early.push(e.payload);
      else if (e.payload.id === id) deliver(e.payload.event);
    });
    try {
      id = await invoke<string>("sse_listen", { root, entry, environmentId: environmentId || null });
    } catch (e) {
      unlisten();
      throw e;
    }
    for (const e of early) if (e.id === id) deliver(e.event);
    const stream = id;
    return { stop: () => invoke<void>("sse_stop", { id: stream }) };
  },
  async wsConnect(root, entry, environmentId, onFrame) {
    const { listen } = await import("@tauri-apps/api/event");
    // Frames can arrive before the id does: keep them until it is known.
    let id: string | null = null;
    const early: { id: string; frame: WsFrame }[] = [];
    const deliver = (frame: WsFrame) => {
      onFrame(frame);
      if (frame.direction === "error" || (frame.direction === "info" && frame.text.startsWith("Closed"))) unlisten();
    };
    const unlisten = await listen<{ id: string; frame: WsFrame }>("ws-frame", (event) => {
      if (id === null) early.push(event.payload);
      else if (event.payload.id === id) deliver(event.payload.frame);
    });
    try {
      id = await invoke<string>("ws_connect", { root, entry, environmentId: environmentId || null });
    } catch (e) {
      unlisten();
      throw e;
    }
    for (const e of early) if (e.id === id) deliver(e.frame);
    const connection = id;
    return {
      send: (text) => invoke<void>("ws_send", { id: connection, text }),
      close: () => invoke<void>("ws_close", { id: connection }),
    };
  },
  flowUsage: (root) => invoke<Record<string, string[]>>("flow_usage", { root }),
  runFlow: (root, id, environmentId) =>
    invoke<FlowReport>("run_flow", { root, id, environmentId: environmentId || null }),
  async onFlowStep(handler) {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<FlowStepEvent>("flow-step", (event) => handler(event.payload));
  },
  requestLink: (root, requestId) => invoke<string>("request_link", { root, requestId }),
  resolveLink: (link, roots) => invoke<LinkTarget>("resolve_link", { link, roots }),
  async onLinks(handler) {
    const { listen } = await import("@tauri-apps/api/event");
    await listen<string[]>("pulpo-link", (event) => handler(event.payload));
    const startup = await invoke<string[]>("startup_links");
    if (startup.length) handler(startup);
  },
  gitPanel: (root) => invoke<GitPanel>("git_panel", { root }),
  gitCommit: (root, message) => invoke<string>("git_commit", { root, message }),
  gitSwitch: (root, branch, create) => invoke<void>("git_switch", { root, branch, create }),
  gitPull: (root) => invoke<string>("git_pull", { root }),
  gitPush: (root) => invoke<string>("git_push", { root }),
  exportCollection: (root, folderId, format) => invoke<Exported>("export_collection", { root, folderId, format }),
  exportEnvironment: (root, environmentId) => invoke<Exported>("export_environment", { root, environmentId }),
  async saveTextFile(defaultName, text) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({ defaultPath: defaultName, title: "Export" });
    if (!path) return false;
    await invoke("write_text_file", { path, text });
    return true;
  },
  async pickImportFile() {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      multiple: false,
      title: "Import a collection, environment, OpenAPI document, HAR, .http or curl file (for Bruno, its bruno.json)",
      filters: [
        {
          name: "Collections and specs",
          extensions: ["json", "yaml", "yml", "har", "http", "rest", "txt", "sh"],
        },
      ],
    });
    return typeof picked === "string" ? picked : null;
  },
  async pickUploadFile() {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ multiple: false, title: "Choose a file inside the workspace" });
    return typeof picked === "string" ? picked : null;
  },
};

/** A picked absolute path as a workspace-relative one, or null when it is outside the
 * workspace (pulpo-core refuses to send those). */
export function toWorkspacePath(root: string, absolute: string): string | null {
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
  const base = norm(root);
  const path = norm(absolute);
  const windows = /^[a-z]:\//i.test(base);
  const same = (a: string, b: string) => (windows ? a.toLowerCase() === b.toLowerCase() : a === b);
  if (!same(path.slice(0, base.length + 1), `${base}/`)) return null;
  const relative = path.slice(base.length + 1);
  return relative && !relative.split("/").includes("..") ? relative : null;
}

function slugify(name: string): string {
  const slug = name
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || "untitled";
}

/** `Type` and `Type.field` names of an introspection result (as pulpo-core's `names_of`). */
function schemaNames(introspection: string): Set<string> {
  const out = new Set<string>();
  const types = (JSON.parse(introspection).__schema?.types ?? []) as {
    name: string;
    fields?: { name: string }[] | null;
    inputFields?: { name: string }[] | null;
    enumValues?: { name: string }[] | null;
  }[];
  for (const t of types) {
    if (t.name.startsWith("__")) continue;
    out.add(t.name);
    for (const f of [...(t.fields ?? []), ...(t.inputFields ?? []), ...(t.enumValues ?? [])]) out.add(`${t.name}.${f.name}`);
  }
  return out;
}

function memoryBackend(): WorkspaceBackend {
  // The browser preview keeps schemas for this session only.
  const graphqlCache = new Map<string, GraphqlSchema>();
  let workspace = sampleWorkspace();
  const environments: Environment[] = sampleEnvironments();
  const secrets = new Map<string, string>();
  const secretKey = (envId: string, name: string) => `${envId}|${name}`;
  // The browser keeps real cookies itself; this jar only lets the panel be tried out.
  const jars = new Map<string, Cookie[]>();
  let certificates: ClientCertificate[] = [];
  const folderScripts = new Map<string, Scripts>();
  const examples = new Map<string, Example[]>();

  function folderNode(id: string): Extract<TreeNode, { type: "folder" }> {
    let found: Extract<TreeNode, { type: "folder" }> | null = null;
    const walk = (nodes: TreeNode[]) =>
      nodes.forEach((n) => {
        if (n.type !== "folder") return;
        if (n.id === id) found = n;
        else walk(n.children);
      });
    walk(workspace.tree);
    if (!found) throw { kind: "invalidPath", message: id } satisfies CollectionError;
    return found;
  }

  function childrenOf(folderId: string): TreeNode[] {
    if (!folderId) return workspace.tree;
    const find = (nodes: TreeNode[]): TreeNode[] | null => {
      for (const node of nodes) {
        if (node.type !== "folder") continue;
        if (node.id === folderId) return node.children;
        const found = find(node.children);
        if (found) return found;
      }
      return null;
    };
    const children = find(workspace.tree);
    if (!children) throw { kind: "invalidPath", message: folderId } satisfies CollectionError;
    return children;
  }

  function nextOrder(nodes: TreeNode[]): string {
    const max = Math.max(0, ...nodes.map((n) => parseInt(n.id.split("/").pop() ?? "", 10) || 0));
    return String(max + 1).padStart(2, "0");
  }

  const prefix = (folderId: string) => (folderId ? `${folderId}/` : "");

  // Organizing works like pulpo-core, simplified: after each change every id is rebuilt
  // from the node's position and slug, and the ids that changed are reported.
  const slugOverride = new Map<TreeNode, string>();
  const slugOf = (node: TreeNode) =>
    slugOverride.get(node) ??
    (node.id.split("/").pop() ?? "").replace(/\.pulpo\.yaml$/, "").replace(/^\d+-/, "");

  function reid(nodes: TreeNode[], parent: string, renamed: [string, string][]) {
    nodes.forEach((node, i) => {
      const stem = `${String(i + 1).padStart(2, "0")}-${slugOf(node)}`;
      const id = `${parent}${stem}${node.type === "request" ? ".pulpo.yaml" : ""}`;
      if (node.id && node.id !== id) renamed.push([node.id, id]);
      node.id = id;
      if (node.type === "folder") reid(node.children, `${id}/`, renamed);
    });
  }

  function rebuild(node: TreeNode): Organized {
    const renamed: [string, string][] = [];
    reid(workspace.tree, "", renamed);
    slugOverride.clear();
    return { id: node.id, renamed };
  }

  function locate(id: string): { list: TreeNode[]; index: number; node: TreeNode } {
    const parent = id.includes("/") ? id.slice(0, id.lastIndexOf("/")) : "";
    const list = childrenOf(parent);
    const index = list.findIndex((n) => n.id === id);
    if (index < 0) throw { kind: "invalidPath", message: id } satisfies CollectionError;
    return { list, index, node: list[index] };
  }

  function clearIds(node: TreeNode) {
    node.id = "";
    if (node.type === "folder") node.children.forEach(clearIds);
  }

  return {
    persistent: false,
    pickFolder: async () => null,
    open: async () => clone(workspace),
    init: async () => clone(workspace),
    async save(_root, entry) {
      const replace = (nodes: TreeNode[]): boolean =>
        nodes.some((node, i) => {
          if (node.type === "request" && node.id === entry.id) {
            nodes[i] = { type: "request", ...clone(entry) };
            return true;
          }
          return node.type === "folder" && replace(node.children);
        });
      replace(workspace.tree);
    },
    async createRequest(_root, folderId, name) {
      const children = childrenOf(folderId);
      const entry: RequestEntry = {
        id: `${prefix(folderId)}${nextOrder(children)}-${slugify(name)}.pulpo.yaml`,
        name,
        protocol: "http",
        request: { method: "GET", url: "https://", headers: [], body: null, disabledParams: [], pathParams: [] },
        scripts: { pre: "", test: "" },
      };
      children.push({ type: "request", ...clone(entry) });
      return entry;
    },
    async createFolder(_root, parentId, name) {
      const children = childrenOf(parentId);
      const id = `${prefix(parentId)}${nextOrder(children)}-${slugify(name)}`;
      children.push({ type: "folder", id, name, children: [] });
      return id;
    },
    async deleteRequest(_root, id) {
      const remove = (nodes: TreeNode[]): boolean => {
        const index = nodes.findIndex((n) => n.type === "request" && n.id === id);
        if (index >= 0) {
          nodes.splice(index, 1);
          return true;
        }
        return nodes.some((n) => n.type === "folder" && remove(n.children));
      };
      remove(workspace.tree);
    },
    async listEnvironments() {
      return clone(environments).map((env) => ({
        ...env,
        variables: env.variables.map((v) =>
          v.secret ? { ...v, value: "", hasValue: secrets.has(secretKey(env.id, v.name)) } : { ...v, hasValue: true },
        ),
      }));
    },
    async saveEnvironment(_root, env) {
      for (const v of env.variables) {
        if (v.secret && v.value) secrets.set(secretKey(env.id, v.name), v.value);
      }
      const index = environments.findIndex((e) => e.id === env.id);
      const stored = { ...clone(env), variables: env.variables.map((v) => ({ ...v, value: v.secret ? "" : v.value })) };
      if (index >= 0) environments[index] = stored;
      else environments.push(stored);
    },
    async createEnvironment(_root, name) {
      let id = `environments/${slugify(name)}.yaml`;
      for (let n = 2; environments.some((e) => e.id === id); n++) id = `environments/${slugify(name)}-${n}.yaml`;
      const env: Environment = { id, name, variables: [] };
      environments.push(clone(env));
      return env;
    },
    async deleteEnvironment(_root, id) {
      const index = environments.findIndex((e) => e.id === id);
      if (index >= 0) environments.splice(index, 1);
    },
    async listExamples(_root, requestId) {
      return clone(examples.get(requestId) ?? []);
    },
    async saveExample(_root, requestId, example) {
      const list = examples.get(requestId) ?? [];
      const saved = { ...clone(example), id: example.id || `${String(list.length + 1).padStart(2, "0")}-${slugify(example.name)}` };
      const index = list.findIndex((e) => e.id === saved.id);
      if (index >= 0) list[index] = saved;
      else list.push(saved);
      examples.set(requestId, list);
      return clone(saved);
    },
    async deleteExample(_root, requestId, id) {
      examples.set(requestId, (examples.get(requestId) ?? []).filter((e) => e.id !== id));
    },
    // The preview has no Rust: a small JSONPath subset, and jq only in the app.
    async queryJson(body, language, expression) {
      if (language === "jq") throw new Error("jq runs in the desktop app (the browser preview has JSONPath only)");
      return previewJsonPath(JSON.parse(body), expression);
    },
    // The browser downloads it to its download folder.
    async saveResponse(response, suggestedName) {
      const data = response.bodyBase64 ? base64ToBytes(response.bodyBase64) : response.body;
      const url = URL.createObjectURL(new Blob([data]));
      const link = Object.assign(document.createElement("a"), { href: url, download: suggestedName });
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      return suggestedName;
    },
    // pulpo-core generates code in the desktop app; the preview only has a plain curl.
    async generateCode(_root, entry, _environmentId, language) {
      if (language !== "curl") return `// ${CODE_LANGUAGES[language]}: code generation runs in the desktop app.`;
      return previewCurl(workspace, entry);
    },
    // Scripts only run in the desktop app, so nothing sets local values here.
    async localValues() {
      return {};
    },
    async folderMeta(_root, folderId) {
      const source = folderId ? folderNode(folderId) : workspace;
      return clone({
        name: source.name,
        headers: source.headers ?? [],
        auth: source.auth ?? null,
        settings: source.settings ?? {},
      });
    },
    async saveFolderMeta(_root, folderId, meta) {
      const target = folderId ? folderNode(folderId) : workspace;
      target.headers = clone(meta.headers);
      target.auth = clone(meta.auth);
      target.settings = clone(meta.settings);
    },
    async folderScripts(_root, folderId) {
      return clone(folderScripts.get(folderId) ?? { pre: "", test: "" });
    },
    async saveFolderScripts(_root, folderId, scripts) {
      folderScripts.set(folderId, clone(scripts));
    },
    async renameNode(_root, id, name) {
      const { node } = locate(id);
      node.name = name.trim();
      slugOverride.set(node, slugify(node.name));
      return rebuild(node);
    },
    async moveNode(_root, id, target, index) {
      const { list, index: from, node } = locate(id);
      if (node.type === "folder" && (target === id || target.startsWith(`${id}/`))) {
        throw { kind: "invalidPath", message: "a folder cannot go inside itself" } satisfies CollectionError;
      }
      list.splice(from, 1);
      const into = childrenOf(target);
      into.splice(Math.min(index, into.length), 0, node);
      return rebuild(node);
    },
    async duplicateNode(_root, id) {
      const { list, index, node } = locate(id);
      const copy = clone(node);
      copy.name = `${node.name} copy`;
      clearIds(copy);
      slugOverride.set(copy, slugify(copy.name));
      list.splice(index + 1, 0, copy);
      return rebuild(copy);
    },
    async deleteFolder(_root, id) {
      const { list, index, node } = locate(id);
      if (node.type !== "folder") throw { kind: "invalidPath", message: `${id} is not a folder` } satisfies CollectionError;
      list.splice(index, 1);
    },
    async listCertificates() {
      return clone(certificates);
    },
    async saveCertificates(_root, list) {
      certificates = clone(list);
    },
    async saveFolderSettings(_root, folderId, settings) {
      if (!folderId) {
        workspace.settings = clone(settings);
        return;
      }
      const find = (nodes: TreeNode[]): boolean =>
        nodes.some((node) => {
          if (node.type !== "folder") return false;
          if (node.id === folderId) {
            node.settings = clone(settings);
            return true;
          }
          return find(node.children);
        });
      if (!find(workspace.tree)) throw { kind: "invalidPath", message: folderId } satisfies CollectionError;
    },
    async listCookies(_root, environmentId) {
      return clone(jars.get(environmentId) ?? []);
    },
    async saveCookies(_root, environmentId, cookies) {
      jars.set(environmentId, clone(cookies));
    },
    async saveFolderAuth(_root, folderId, auth) {
      if (!folderId) {
        workspace.auth = clone(auth);
        return;
      }
      const find = (nodes: TreeNode[]): boolean =>
        nodes.some((node) => {
          if (node.type !== "folder") return false;
          if (node.id === folderId) {
            node.auth = clone(auth);
            return true;
          }
          return find(node.children);
        });
      if (!find(workspace.tree)) throw { kind: "invalidPath", message: folderId } satisfies CollectionError;
    },
    async run(_root, entry, environmentId, options) {
      const report: RunReport = {
        id: entry.id,
        name: entry.name,
        response: null,
        error: null,
        tests: [],
        logs: [],
        scriptErrors: [],
        iteration: options?.iteration ?? 0,
        attempts: 1,
      };
      if (/^\s*grpcs?:\/\//i.test(entry.request.url)) {
        report.error = { kind: "other", message: "gRPC calls are made by the desktop app; a browser can only speak gRPC-Web." };
        return report;
      }
      const hasScripts = !!(entry.scripts.pre.trim() || entry.scripts.test.trim());
      if (hasScripts) report.scriptErrors.push("Scripts run in the desktop app; the browser preview only sends the request.");
      try {
        const auth = entry.request.auth ?? inheritedAuth(workspace, entry.id).auth;
        const timeoutMs = entry.request.settings?.timeoutMs ?? inheritedSettings(workspace, entry.id).timeoutMs.value;
        const headers = effectiveHeaders(workspace, entry);
        report.response = await sendHttp({ ...entry.request, auth, headers, settings: { timeoutMs } }, {
          environmentId,
          // Iteration data wins over the environment, as in pulpo-core.
          browserVars: { ...(environmentId ? this.browserVariables!(environmentId) : {}), ...(options?.data ?? {}) },
        });
      } catch (e) {
        report.error = asSendError(e);
      }
      return report;
    },
    async mqttConnect(_root, entry, environmentId, onMessage) {
      const vars = environmentId ? this.browserVariables!(environmentId) : {};
      const missing = new Set<string>();
      const r = (t: string) => resolveText(t, vars, missing);
      const url = r(entry.request.url);
      const target = entry.request.mqtt ?? {};
      const username = r(target.username ?? "");
      const password = r(target.password ?? "");
      const topics = (target.subscribe ?? []).map(r).filter(Boolean);
      if (missing.size) throw `unresolved variables: ${[...missing].sort().join(", ")}`;
      if (!/^wss?:\/\//i.test(url)) {
        throw "The browser preview reaches brokers over WebSocket only (ws:// or wss://); mqtt:// and mqtts:// work in the desktop app.";
      }
      const mqtt = await import("./mqtt");
      const started = performance.now();
      const at = () => Math.round(performance.now() - started);
      const socket = new WebSocket(url, ["mqtt"]);
      socket.binaryType = "arraybuffer";
      await new Promise<void>((resolve, reject) => {
        socket.onopen = () => resolve();
        socket.onerror = () => reject(`could not connect to ${url}`);
      });
      const reader = new mqtt.MqttReader();
      const qos = target.qos ?? 0;
      let nextId = 1;
      let ready: (() => void) | null = null;
      let failed: ((e: string) => void) | null = null;
      const acked = new Promise<void>((resolve, reject) => {
        ready = resolve;
        failed = reject;
      });
      socket.onmessage = (event) => {
        for (const p of reader.feed(new Uint8Array(event.data as ArrayBuffer))) {
          if (p.kind === "connack") {
            if (p.code !== 0) {
              // A refused attempt ends here, quietly: its close is not part of the next one.
              socket.onclose = null;
              socket.close();
              failed?.(`the broker refused the connection: ${mqtt.CONNACK_REASONS[p.code] ?? "refused"}`);
            }
            else if (topics.length) socket.send(mqtt.subscribePacket(nextId++, topics, qos));
            else ready?.();
          } else if (p.kind === "suback") {
            ready?.();
          } else if (p.kind === "publish") {
            if (p.id !== null) socket.send(mqtt.pubackPacket(p.id));
            onMessage({ direction: "received", topic: p.topic, payload: p.payload, qos: p.qos, retain: p.retain, atMs: at() });
          }
        }
      };
      socket.onclose = () => onMessage({ direction: "info", topic: "", payload: "Closed", atMs: at() });
      const clientId = r(target.clientId ?? "") || `pulpo-${Math.random().toString(16).slice(2, 10)}`;
      socket.send(mqtt.connectPacket(clientId, username, password));
      await acked;
      onMessage({ direction: "info", topic: "", payload: `Connected to ${url}`, atMs: at() });
      if (topics.length) onMessage({ direction: "info", topic: "", payload: `Subscribed to ${topics.join(", ")}`, atMs: at() });
      const ping = setInterval(() => socket.readyState === WebSocket.OPEN && socket.send(mqtt.PINGREQ), 15000);
      return {
        async publish(topic, payload, q, retain) {
          const t = resolveText(topic, vars, missing);
          const body = resolveText(payload, vars, missing);
          if (missing.size) throw `unresolved variables: ${[...missing].sort().join(", ")}`;
          socket.send(mqtt.publishPacket(nextId++, t, body, q, retain));
          onMessage({ direction: "sent", topic: t, payload: body, qos: q, retain, atMs: at() });
        },
        async close() {
          clearInterval(ping);
          if (socket.readyState === WebSocket.OPEN) socket.send(mqtt.DISCONNECT);
          socket.close();
        },
      };
    },
    async sseListen(_root, entry, environmentId, onEvent) {
      const vars = environmentId ? this.browserVariables!(environmentId) : {};
      const missing = new Set<string>();
      const url = resolveText(entry.request.url, vars, missing);
      const headers = new Headers();
      for (const h of effectiveHeaders(workspace, entry)) {
        if (h.enabled && h.name.trim()) headers.append(h.name.trim(), resolveText(h.value, vars, missing));
      }
      const auth = entry.request.auth ?? inheritedAuth(workspace, entry.id).auth;
      if (auth?.type === "bearer") headers.set("authorization", `Bearer ${resolveText(auth.token, vars, missing)}`);
      const body = entry.request.body ? resolveText(entry.request.body, vars, missing) : undefined;
      if (missing.size) throw `unresolved variables: ${[...missing].sort().join(", ")}`;
      const controller = new AbortController();
      const started = performance.now();
      const at = () => Math.round(performance.now() - started);
      let response: Response;
      try {
        response = await fetch(url, {
          method: entry.request.method,
          headers,
          body: ["GET", "HEAD"].includes(entry.request.method) ? undefined : body,
          signal: controller.signal,
        });
      } catch (e) {
        throw `could not connect: ${e}. In the browser preview, CORS can block it; the desktop app has no such limit.`;
      }
      const kind = response.headers.get("content-type") ?? "";
      if (!response.ok || !kind.includes("text/event-stream")) {
        throw `${response.status} ${response.statusText}: not an event stream (${kind || "no Content-Type"})`;
      }
      onEvent({ event: "info", data: `Listening to ${url}`, atMs: at() });
      const reader = response.body!.pipeThrough(new TextDecoderStream()).getReader();
      const parser = new SseParser();
      let stopped = false;
      void (async () => {
        try {
          for (;;) {
            const { value, done } = await reader.read();
            if (done) {
              if (!stopped) onEvent({ event: "info", data: "The server ended the stream", atMs: at() });
              break;
            }
            for (const e of parser.feed(value)) onEvent({ ...e, atMs: at() });
          }
        } catch (e) {
          if (!stopped) onEvent({ event: "error", data: String(e), atMs: at() });
        }
      })();
      return {
        async stop() {
          stopped = true;
          controller.abort();
          onEvent({ event: "info", data: "Stopped", atMs: at() });
        },
      };
    },
    async wsConnect(_root, entry, environmentId, onFrame) {
      const vars = environmentId ? this.browserVariables!(environmentId) : {};
      const missing = new Set<string>();
      const url = resolveText(entry.request.url, vars, missing);
      if (missing.size) throw `unresolved variables: ${[...missing].sort().join(", ")}`;
      const started = performance.now();
      const at = () => Math.round(performance.now() - started);
      const auth = entry.request.auth ?? inheritedAuth(workspace, entry.id).auth;
      const all = effectiveHeaders(workspace, entry).filter((h) => h.enabled && h.name.trim());
      // The one handshake header a browser can send: the subprotocols.
      const isProtocol = (h: { name: string }) => h.name.trim().toLowerCase() === "sec-websocket-protocol";
      const protocols = all
        .filter(isProtocol)
        .flatMap((h) => h.value.split(","))
        .map((p) => p.trim())
        .filter(Boolean);
      const headers = all.filter((h) => !isProtocol(h));
      const socket = protocols.length ? new WebSocket(url, protocols) : new WebSocket(url);
      await new Promise<void>((resolve, reject) => {
        socket.onopen = () => resolve();
        socket.onerror = () => reject(`could not connect to ${url}`);
      });
      onFrame({ direction: "info", text: `Connected to ${url}${socket.protocol ? ` · protocol ${socket.protocol}` : ""}`, atMs: at() });
      if (headers.length || (auth && auth.type !== "none")) {
        onFrame({
          direction: "info",
          text: "The browser preview cannot send headers or auth in the handshake; the desktop app does.",
          atMs: at(),
        });
      }
      socket.onmessage = (event) =>
        onFrame(
          typeof event.data === "string"
            ? { direction: "received", text: event.data, atMs: at() }
            : { direction: "received", text: "binary message", binary: true, atMs: at() },
        );
      socket.onclose = (event) =>
        onFrame({ direction: "info", text: event.code === 1000 || event.code === 1005 ? "Closed" : `Closed: ${event.code} ${event.reason}`, atMs: at() });
      return {
        async send(text) {
          const missingNow = new Set<string>();
          const resolved = resolveText(text, vars, missingNow);
          if (missingNow.size) throw `unresolved variables: ${[...missingNow].sort().join(", ")}`;
          if (socket.readyState !== WebSocket.OPEN) throw "the connection is closed";
          socket.send(resolved);
          onFrame({ direction: "sent", text: resolved, atMs: at() });
        },
        async close() {
          socket.close(1000);
        },
      };
    },
    async graphqlSchema(root, entry, environmentId, refresh) {
      const key = entry.request.url.trim();
      const previous = graphqlCache.get(key) ?? null;
      if (!refresh) return previous ? { ...previous, cached: true, added: [], removed: [], compared: false } : null;
      const probe = clone(entry);
      probe.request.method = "POST";
      probe.request.body = null;
      probe.request.form = null;
      probe.request.bodyFile = null;
      probe.request.graphql = { query: INTROSPECTION_QUERY, variables: "", operationName: "IntrospectionQuery" };
      probe.scripts = { pre: "", test: "" };
      const report = await this.run(root, probe, environmentId);
      if (!report.response) throw report.error?.message ?? "no response";
      let body: { data?: { __schema?: unknown }; errors?: { message?: string }[] };
      try {
        body = JSON.parse(report.response.body);
      } catch {
        throw `${report.response.status} ${report.response.statusText}: the answer is not JSON, is this a GraphQL endpoint?`;
      }
      if (!body.data?.__schema) {
        const reason = body.errors?.[0]?.message ?? `${report.response.status} ${report.response.statusText}`;
        throw `no schema in the answer: ${reason} (is introspection turned off?)`;
      }
      const introspection = JSON.stringify({ __schema: body.data.__schema });
      const before = previous ? schemaNames(previous.introspection) : null;
      const after = schemaNames(introspection);
      const schema: GraphqlSchema = {
        introspection,
        added: before ? [...after].filter((n) => !before.has(n)).sort() : [],
        removed: before ? [...before].filter((n) => !after.has(n)).sort() : [],
        compared: !!before,
        fetchedAt: Math.floor(Date.now() / 1000),
        cached: false,
      };
      graphqlCache.set(key, schema);
      return schema;
    },
    browserVariables(environmentId) {
      const env = environments.find((e) => e.id === environmentId);
      const vars: Record<string, string> = {};
      for (const v of env?.variables ?? []) {
        const value = v.secret ? secrets.get(secretKey(environmentId, v.name)) : v.value;
        if (value !== undefined) vars[v.name] = value;
      }
      return vars;
    },
  };
}

export const backend: WorkspaceBackend = insideTauri ? tauriBackend : memoryBackend();
