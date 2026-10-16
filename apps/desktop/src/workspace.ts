import { invoke } from "@tauri-apps/api/core";
import {
  insideTauri,
  sendHttp,
  asSendError,
  type Auth,
  type Header,
  type HttpRequest,
  type HttpResponse,
  type SendError,
  type Settings,
} from "./core";
import { sampleEnvironments, sampleWorkspace } from "./sample";
import { base64ToBytes } from "./responseFiles";

export type Protocol = "http" | "graphql" | "grpc" | "websocket" | "soap" | "sse";

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

export interface ImportSummary {
  format: "postmanCollection" | "postmanEnvironment" | "openApi" | "curl" | null;
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
  /** Desktop only: picks a file to upload and returns its absolute path. */
  pickUploadFile?(): Promise<string | null>;
  /** Sends the request with its scripts; `entry` may contain unsaved edits. */
  run(root: string, entry: RequestEntry, environmentId: string | undefined): Promise<RunReport>;
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
  run: (root, entry, environmentId) => invoke<RunReport>("run_request", { root, entry, environmentId }),
  importInto: (root, folderId, source) => invoke<ImportSummary>("import_into", { root, folderId, ...source }),
  async pickImportFile() {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      multiple: false,
      title: "Import a collection, environment, OpenAPI document or curl file",
      filters: [{ name: "Collections and specs", extensions: ["json", "yaml", "yml", "txt", "sh"] }],
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

function memoryBackend(): WorkspaceBackend {
  let workspace = sampleWorkspace();
  const environments: Environment[] = sampleEnvironments();
  const secrets = new Map<string, string>();
  const secretKey = (envId: string, name: string) => `${envId}|${name}`;
  // The browser keeps real cookies itself; this jar only lets the panel be tried out.
  const jars = new Map<string, Cookie[]>();
  let certificates: ClientCertificate[] = [];
  const folderScripts = new Map<string, Scripts>();

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
    async run(_root, entry, environmentId) {
      const report: RunReport = {
        id: entry.id,
        name: entry.name,
        response: null,
        error: null,
        tests: [],
        logs: [],
        scriptErrors: [],
      };
      const hasScripts = !!(entry.scripts.pre.trim() || entry.scripts.test.trim());
      if (hasScripts) report.scriptErrors.push("Scripts run in the desktop app; the browser preview only sends the request.");
      try {
        const auth = entry.request.auth ?? inheritedAuth(workspace, entry.id).auth;
        const timeoutMs = entry.request.settings?.timeoutMs ?? inheritedSettings(workspace, entry.id).timeoutMs.value;
        const headers = effectiveHeaders(workspace, entry);
        report.response = await sendHttp({ ...entry.request, auth, headers, settings: { timeoutMs } }, {
          environmentId,
          browserVars: environmentId ? this.browserVariables!(environmentId) : {},
        });
      } catch (e) {
        report.error = asSendError(e);
      }
      return report;
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
