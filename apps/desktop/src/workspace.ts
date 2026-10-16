import { invoke } from "@tauri-apps/api/core";
import { insideTauri, sendHttp, asSendError, type HttpRequest, type HttpResponse, type SendError } from "./core";
import { sampleEnvironments, sampleWorkspace } from "./sample";

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

export type TreeNode = ({ type: "request" } & RequestEntry) | { type: "folder"; id: string; name: string; children: TreeNode[] };

export interface Workspace {
  root: string;
  name: string;
  tree: TreeNode[];
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

export function toEntry(node: Extract<TreeNode, { type: "request" }>): RequestEntry {
  const { id, name, protocol, request, scripts } = node;
  return clone({ id, name, protocol, request, scripts: scripts ?? { pre: "", test: "" } });
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
  /** Desktop only: imports a file (or pasted text) with the Rust importers. */
  importInto?(root: string, folderId: string, source: { path?: string; text?: string }): Promise<ImportSummary>;
  pickImportFile?(): Promise<string | null>;
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
};

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
        request: { method: "GET", url: "https://", headers: [], body: null },
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
        report.response = await sendHttp(entry.request, {
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
