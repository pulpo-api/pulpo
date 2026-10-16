import { invoke } from "@tauri-apps/api/core";
import { insideTauri, type HttpRequest } from "./core";
import { sampleWorkspace } from "./sample";

export type Protocol = "http" | "graphql" | "grpc" | "websocket" | "soap" | "sse";

export interface RequestEntry {
  id: string;
  name: string;
  protocol: Protocol;
  request: HttpRequest;
}

export type TreeNode = ({ type: "request" } & RequestEntry) | { type: "folder"; id: string; name: string; children: TreeNode[] };

export interface Workspace {
  root: string;
  name: string;
  tree: TreeNode[];
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
  const { id, name, protocol, request } = node;
  return clone({ id, name, protocol, request });
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
  };
}

export const backend: WorkspaceBackend = insideTauri ? tauriBackend : memoryBackend();
