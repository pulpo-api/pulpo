<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch, watchEffect } from "vue";
import { varContext } from "./varContext";
import Sidebar from "./components/Sidebar.vue";
import RequestEditor from "./components/RequestEditor.vue";
import ResponseViewer from "./components/ResponseViewer.vue";
import EnvironmentPanel from "./components/EnvironmentPanel.vue";
import CookiesPanel from "./components/CookiesPanel.vue";
import FolderEditor from "./components/FolderEditor.vue";
import CodePanel from "./components/CodePanel.vue";
import ImportDialog from "./components/ImportDialog.vue";
import { resolveText } from "./variables";
import { applyPathParams } from "./params";
import { applyAuth, asSendError, coreInfo, resolveAuth, type CoreInfo, type HttpResponse, type SendError } from "./core";
import {
  asCollectionError,
  backend,
  inheritedAuth,
  inheritedHeaders,
  inheritedSettings,
  remapId,
  type FolderMeta,
  toEntry,
  type Organized,
  type CollectionError,
  type Environment,
  type RequestEntry,
  type TestResult,
  type TreeNode,
  type Workspace,
} from "./workspace";

interface Result {
  response: HttpResponse | null;
  error: SendError | null;
  sending: boolean;
  tests: TestResult[];
  logs: string[];
  scriptErrors: string[];
}

const EMPTY: Result = { response: null, error: null, sending: false, tests: [], logs: [], scriptErrors: [] };
const HISTORY_SIZE = 10;

interface HistoryEntry {
  at: Date;
  result: Result;
}

type RequestNode = Extract<TreeNode, { type: "request" }>;

const LAST_ROOT = "pulpo.lastWorkspace";

const workspace = ref<Workspace | null>(null);
const saved = reactive(new Map<string, string>());
const selectedId = ref("");
const folderId = ref("");
/** Last responses per request, newest first. In memory only: responses can hold sensitive data. */
const history = reactive<Record<string, HistoryEntry[]>>({});
const historyPick = reactive<Record<string, number>>({});
const sending = reactive(new Set<string>());
const openTabs = ref<string[]>([]);
const sidebar = ref<InstanceType<typeof Sidebar> | null>(null);
const showShortcuts = ref(false);
const problem = ref<CollectionError | null>(null);
const showImport = ref(false);
const importDialog = ref<InstanceType<typeof ImportDialog> | null>(null);
const importBlocked = computed(() =>
  dirty.value.size ? `Save your ${dirty.value.size} unsaved request(s) first (Ctrl+S), so the import does not overwrite them.` : null,
);

async function runImport(source: { path?: string; text?: string }) {
  const ws = workspace.value;
  if (!ws || !backend.importInto) return;
  try {
    const summary = await backend.importInto(ws.root, folderId.value, source);
    const keep = selectedId.value;
    adopt(await backend.open(ws.root));
    if (requests.value.some((n) => n.id === keep)) selectedId.value = keep;
    folderId.value = summary.folderId;
    importDialog.value?.show({ summary });
  } catch (e) {
    importDialog.value?.show({ error: String(e) });
  }
}

async function importFile() {
  const path = await backend.pickImportFile?.();
  if (path) await runImport({ path });
  else importDialog.value?.show({});
}

const confirmDelete = ref(false);
const environments = ref<Environment[]>([]);
const activeEnvId = ref("");
/** Values scripts set for the active environment; they win over it (as in pulpo-core). */
const localValues = ref<Record<string, string>>({});

async function loadLocalValues() {
  const ws = workspace.value;
  if (!ws) return;
  try {
    localValues.value = await backend.localValues(ws.root, activeEnvId.value);
  } catch {
    localValues.value = {};
  }
}
watch([() => workspace.value?.root, activeEnvId], loadLocalValues, { immediate: true });

// What the editors use to colour {{variables}} and show their values.
watchEffect(() => {
  const env = environments.value.find((e) => e.id === activeEnvId.value);
  varContext.value = { environment: env?.name ?? "", variables: env?.variables ?? [], local: localValues.value };
});
const showEnvironments = ref(false);
const showCookies = ref(false);
const showCode = ref(false);
/** A short message in the top bar ("Copied as curl"). */
const notice = ref("");
function flash(message: string) {
  notice.value = message;
  setTimeout(() => (notice.value = notice.value === message ? "" : notice.value), 2000);
}

const requestEditor = ref<InstanceType<typeof RequestEditor> | null>(null);

/** Keeps the shown response next to the request, named after its status. */
async function saveExample(response: HttpResponse) {
  const ws = workspace.value;
  const node = selected.value;
  if (!ws || !node) return;
  try {
    await backend.saveExample(ws.root, node.id, {
      id: "",
      name: `${response.status} ${response.statusText}`.trim(),
      status: response.status,
      headers: response.headers.filter((h) => h.name.toLowerCase() === "content-type"),
      body: response.bodyBase64 ? "" : response.body,
    });
    pinTab(node.id);
    requestEditor.value?.openExamples(false);
    flash("Saved as an example: see the Examples tab");
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

/** "Add example" from the tree: open the request with a new example to fill in. */
async function addExample(id: string) {
  editingFolder.value = null;
  selectedId.value = id;
  pinTab(id);
  await nextTick();
  requestEditor.value?.openExamples(true);
}

/** A test made from the response filter goes to the end of the request's test script. */
function addTest(code: string) {
  const node = selected.value;
  if (!node) return;
  const current = node.scripts.test.trimEnd();
  node.scripts.test = current ? `${current}\n\n${code}` : code;
  flash("Test added: see the Tests tab, then save");
}

/** "Copy as curl" from the tree: no variables resolved, no secrets. */
async function copyCurl(id: string) {
  const ws = workspace.value;
  const node = requests.value.find((n) => n.id === id);
  if (!ws || !node) return;
  try {
    const code = await backend.generateCode(ws.root, toEntry(node), activeEnvId.value, "curl", {
      resolve: false,
      includeSecrets: false,
    });
    await navigator.clipboard.writeText(code);
    problem.value = null;
    flash(`Copied “${node.name}” as curl`);
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}
/** The folder being edited ("" = the collection); null = editing requests. */
const editingFolder = ref<string | null>(null);
const folderEditor = ref<InstanceType<typeof FolderEditor> | null>(null);

/** Keeps the tree in step with a saved folder, so requests show what they inherit. */
function onFolderSaved(folderId: string, meta: FolderMeta) {
  const ws = workspace.value;
  if (!ws) return;
  let target: { headers?: FolderMeta["headers"]; auth?: FolderMeta["auth"]; settings?: FolderMeta["settings"] } = ws;
  if (folderId) walk(ws.tree, (n) => n.type === "folder" && n.id === folderId && (target = n));
  target.headers = meta.headers;
  target.auth = meta.auth;
  target.settings = meta.settings;
}

const envKey = () => `pulpo.env:${workspace.value?.root ?? ""}`;
watch(activeEnvId, (id) => {
  if (workspace.value) localStorage.setItem(envKey(), id);
});

/** Values shown in previews. Secrets are masked; in the app their real values never reach the UI. */
const previewVars = computed(() => {
  const env = environments.value.find((e) => e.id === activeEnvId.value);
  const vars: Record<string, string> = {};
  for (const v of env?.variables ?? []) {
    if (v.secret) {
      if (v.hasValue) vars[v.name] = "••••••";
    } else {
      vars[v.name] = v.value;
    }
  }
  return vars;
});

const selectedInheritedAuth = computed(() =>
  workspace.value && selected.value
    ? inheritedAuth(workspace.value, selected.value.id)
    : { auth: { type: "none" as const }, sourceId: "", sourceName: "", found: false },
);

const selectedInheritedHeaders = computed(() =>
  workspace.value && selected.value ? inheritedHeaders(workspace.value, selected.value.id) : [],
);

const selectedInheritedSettings = computed(() =>
  workspace.value && selected.value
    ? inheritedSettings(workspace.value, selected.value.id)
    : inheritedSettings({ root: "", name: "", tree: [] }, ""),
);

const preview = computed(() => {
  const node = selected.value;
  if (!node) return null;
  const missing = new Set<string>();
  const pathed = applyPathParams(
    resolveText(node.request.url, previewVars.value, missing, true),
    node.request.pathParams.map((p) => ({ name: p.name, value: resolveText(p.value, previewVars.value, missing, true) })),
  );
  for (const h of node.request.headers.filter((h) => h.enabled)) {
    resolveText(h.name, previewVars.value, missing, true);
    resolveText(h.value, previewVars.value, missing, true);
  }
  if (node.request.form) {
    for (const f of node.request.form.fields.filter((f) => f.enabled)) {
      resolveText(f.name, previewVars.value, missing, true);
      resolveText(f.value, previewVars.value, missing, true);
    }
  } else if (node.request.bodyFile != null) {
    resolveText(node.request.bodyFile, previewVars.value, missing, true);
  } else if (node.request.body) {
    resolveText(node.request.body, previewVars.value, missing, true);
  }
  const auth = resolveAuth(node.request.auth ?? selectedInheritedAuth.value.auth, (text) =>
    resolveText(text, previewVars.value, missing, true),
  );
  // Only the URL is shown, so headers added by the auth are thrown away.
  const url = applyAuth(pathed, [...node.request.headers], auth);
  return { url, changed: url !== node.request.url, missing: [...missing].sort() };
});

async function loadEnvironments() {
  if (!workspace.value) return;
  try {
    environments.value = await backend.listEnvironments(workspace.value.root);
    const remembered = localStorage.getItem(envKey());
    const ids = environments.value.map((e) => e.id);
    activeEnvId.value =
      remembered !== null && (remembered === "" || ids.includes(remembered)) ? remembered : (ids[0] ?? "");
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

async function saveEnvironment(env: Environment) {
  if (!workspace.value) return;
  try {
    await backend.saveEnvironment(workspace.value.root, env);
    environments.value = await backend.listEnvironments(workspace.value.root);
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

async function createEnvironment(name: string) {
  if (!workspace.value) return;
  try {
    const env = await backend.createEnvironment(workspace.value.root, name);
    environments.value = await backend.listEnvironments(workspace.value.root);
    activeEnvId.value = env.id;
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

async function deleteEnvironment(id: string) {
  if (!workspace.value) return;
  try {
    await backend.deleteEnvironment(workspace.value.root, id);
    environments.value = await backend.listEnvironments(workspace.value.root);
    if (activeEnvId.value === id) activeEnvId.value = environments.value[0]?.id ?? "";
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}
const info = ref<CoreInfo | null>(null);
const theme = ref<"light" | "dark">(window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");

watch(theme, (value) => document.documentElement.setAttribute("data-theme", value), { immediate: true });

function walk(nodes: TreeNode[], visit: (node: TreeNode) => void) {
  for (const node of nodes) {
    visit(node);
    if (node.type === "folder") walk(node.children, visit);
  }
}

function snapshot(node: RequestNode): string {
  return JSON.stringify(toEntry(node));
}

const requests = computed(() => {
  const list: RequestNode[] = [];
  if (workspace.value) walk(workspace.value.tree, (n) => n.type === "request" && list.push(n));
  return list;
});

const selected = computed(() => requests.value.find((n) => n.id === selectedId.value) ?? null);
const dirty = computed(() => new Set(requests.value.filter((n) => saved.get(n.id) !== snapshot(n)).map((n) => n.id)));
const result = computed<Result>(() => {
  const id = selectedId.value;
  if (sending.has(id)) return { ...EMPTY, sending: true };
  return history[id]?.[historyPick[id] ?? 0]?.result ?? EMPTY;
});

const historyItems = computed(() =>
  (history[selectedId.value] ?? []).map((h) => ({
    at: h.at.toISOString(),
    label: `${h.at.toLocaleTimeString()} · ${
      h.result.response ? `${h.result.response.status} · ${h.result.response.durationMs} ms` : (h.result.error?.kind ?? "error")
    }`,
    response: h.result.response,
  })),
);

const tabs = computed(() =>
  openTabs.value
    .map((id) => requests.value.find((n) => n.id === id))
    .filter((n): n is RequestNode => !!n),
);

/** The temporary tab, as in Postman and VS Code: one click opens a request in it and the
 * next click replaces it. It stays open (pinned) once edited, sent or double-clicked. */
const previewTab = ref<string | null>(null);

watch(selectedId, (id) => {
  if (!id || openTabs.value.includes(id)) return;
  const index = previewTab.value ? openTabs.value.indexOf(previewTab.value) : -1;
  if (index >= 0) openTabs.value.splice(index, 1, id);
  else openTabs.value.push(id);
  previewTab.value = id;
});

function pinTab(id: string) {
  if (previewTab.value === id) previewTab.value = null;
}

// Editing a request keeps its tab.
watch(
  () => previewTab.value !== null && dirty.value.has(previewTab.value),
  (edited) => {
    if (edited) previewTab.value = null;
  },
);

watch(requests, (list) => {
  openTabs.value = openTabs.value.filter((id) => list.some((n) => n.id === id));
});

function closeTab(id: string) {
  const index = openTabs.value.indexOf(id);
  if (index < 0) return;
  openTabs.value.splice(index, 1);
  if (previewTab.value === id) previewTab.value = null;
  if (selectedId.value === id) {
    const next = openTabs.value[Math.min(index, openTabs.value.length - 1)];
    selectedId.value = next ?? "";
  }
}

function cycleTab(step: number) {
  if (openTabs.value.length < 2) return;
  const index = openTabs.value.indexOf(selectedId.value);
  const next = (index + step + openTabs.value.length) % openTabs.value.length;
  selectedId.value = openTabs.value[next];
}

const folderName = computed(() => {
  const id = selectedId.value;
  let name = workspace.value?.name ?? "";
  if (workspace.value) {
    walk(workspace.value.tree, (n) => {
      if (n.type === "folder" && id.startsWith(`${n.id}/`)) name = n.name;
    });
  }
  return name;
});

function adopt(ws: Workspace) {
  workspace.value = ws;
  saved.clear();
  walk(ws.tree, (n) => n.type === "request" && saved.set(n.id, snapshot(n)));
  openTabs.value = [];
  selectedId.value = requests.value[0]?.id ?? "";
  folderId.value = "";
  problem.value = null;
  void loadEnvironments();
}

async function openRoot(root: string) {
  try {
    adopt(await backend.open(root));
    localStorage.setItem(LAST_ROOT, root);
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

async function openFolder() {
  const root = await backend.pickFolder();
  if (root) await openRoot(root);
}

async function createWorkspace() {
  const root = await backend.pickFolder();
  if (!root) return;
  const name = root.split(/[\\/]/).filter(Boolean).pop() ?? "My API";
  try {
    adopt(await backend.init(root, name));
    localStorage.setItem(LAST_ROOT, root);
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

function childrenOf(id: string): TreeNode[] | null {
  if (!workspace.value) return null;
  if (!id) return workspace.value.tree;
  let found: TreeNode[] | null = null;
  walk(workspace.value.tree, (n) => {
    if (n.type === "folder" && n.id === id) found = n.children;
  });
  return found;
}

async function save(node: RequestNode | null = selected.value) {
  if (!workspace.value || !node) return;
  try {
    await backend.save(workspace.value.root, toEntry(node));
    saved.set(node.id, snapshot(node));
    problem.value = null;
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

async function newRequest(name: string) {
  if (!workspace.value) return;
  try {
    const entry = await backend.createRequest(workspace.value.root, folderId.value, name);
    const node: RequestNode = { type: "request", ...entry };
    childrenOf(folderId.value)?.push(node);
    saved.set(entry.id, snapshot(node));
    selectedId.value = entry.id;
    pinTab(entry.id);
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

async function newFolder(name: string) {
  if (!workspace.value) return;
  try {
    const id = await backend.createFolder(workspace.value.root, folderId.value, name);
    childrenOf(folderId.value)?.push({ type: "folder", id, name, children: [] });
    folderId.value = id;
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

async function deleteSelected() {
  const node = selected.value;
  if (!workspace.value || !node) return;
  if (!confirmDelete.value) {
    confirmDelete.value = true;
    setTimeout(() => (confirmDelete.value = false), 3000);
    return;
  }
  confirmDelete.value = false;
  try {
    await backend.deleteRequest(workspace.value.root, node.id);
    const parent = childrenOf(node.id.includes("/") ? node.id.slice(0, node.id.lastIndexOf("/")) : "");
    const index = parent?.findIndex((n) => n.id === node.id) ?? -1;
    if (parent && index >= 0) parent.splice(index, 1);
    saved.delete(node.id);
    selectedId.value = requests.value[0]?.id ?? "";
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

/** Runs an organize operation (rename, move, duplicate, delete), reloads the tree and
 * follows the ids that changed in tabs, selection and history. */
async function organize(run: (root: string) => Promise<Organized | void>): Promise<Organized | void> {
  const ws = workspace.value;
  if (!ws) return;
  if (dirty.value.size) {
    problem.value = {
      kind: "io",
      message: `Save your ${dirty.value.size} unsaved request(s) first (Ctrl+S): organizing moves files and reloads the tree.`,
    };
    return;
  }
  try {
    const result = await run(ws.root);
    const fresh = await backend.open(ws.root);
    const renamed = result?.renamed ?? [];
    const map = (id: string) => remapId(id, renamed);
    workspace.value = fresh;
    saved.clear();
    walk(fresh.tree, (n) => n.type === "request" && saved.set(n.id, snapshot(n)));
    openTabs.value = openTabs.value.map(map);
    if (previewTab.value) previewTab.value = map(previewTab.value);
    folderId.value = map(folderId.value);
    const entries = Object.entries(history);
    const picks = Object.entries(historyPick);
    for (const [id] of entries) delete history[id];
    for (const [id] of picks) delete historyPick[id];
    for (const [id, list] of entries) history[map(id)] = list;
    for (const [id, pick] of picks) historyPick[map(id)] = pick;
    const next = map(selectedId.value);
    selectedId.value = requests.value.some((n) => n.id === next)
      ? next
      : (openTabs.value.find((id) => requests.value.some((n) => n.id === id)) ?? requests.value[0]?.id ?? "");
    problem.value = null;
    return result;
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

const renameNode = (id: string, name: string) => organize((root) => backend.renameNode(root, id, name));
const moveNode = (id: string, target: string, index: number) =>
  organize((root) => backend.moveNode(root, id, target, index));

async function duplicateNode(id: string) {
  const result = await organize((root) => backend.duplicateNode(root, id));
  // Open the copy of a request right away.
  if (result && result.id.endsWith(".pulpo.yaml")) {
    selectedId.value = result.id;
    pinTab(result.id);
  }
}

function deleteNode(node: TreeNode) {
  return organize((root) =>
    node.type === "folder" ? backend.deleteFolder(root, node.id) : backend.deleteRequest(root, node.id),
  );
}

function select(entry: RequestEntry) {
  editingFolder.value = null;
  selectedId.value = entry.id;
  confirmDelete.value = false;
  folderId.value = entry.id.includes("/") ? entry.id.slice(0, entry.id.lastIndexOf("/")) : "";
}

function remember(id: string, result: Result) {
  const list = history[id] ?? [];
  list.unshift({ at: new Date(), result });
  history[id] = list.slice(0, HISTORY_SIZE);
  historyPick[id] = 0;
}

async function send() {
  const node = selected.value;
  if (!node || node.protocol !== "http" || sending.has(node.id)) return;
  // Sending a request keeps its tab.
  pinTab(node.id);
  sending.add(node.id);
  try {
    const report = await backend.run(workspace.value!.root, toEntry(node), activeEnvId.value || undefined);
    remember(node.id, { ...report, sending: false });
    // Scripts may have set values ({{token}} after a login, for instance).
    void loadLocalValues();
  } catch (e) {
    remember(node.id, { ...EMPTY, error: asSendError(e) });
  } finally {
    sending.delete(node.id);
  }
}

const SHORTCUTS: [string, string][] = [
  ["Ctrl+Enter", "Send the request"],
  ["Ctrl+S", "Save the request"],
  ["Ctrl+K", "Search requests"],
  ["Ctrl+W", "Close the tab"],
  ["Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"],
  ["Ctrl+/", "Show these shortcuts"],
  ["Esc", "Close dialogs"],
];

function onKey(event: KeyboardEvent) {
  if (event.key === "Escape") {
    showShortcuts.value = false;
    return;
  }
  // The code editor handles Ctrl+Enter and Ctrl+S itself and marks the event as handled.
  if (event.defaultPrevented || !(event.ctrlKey || event.metaKey)) return;
  const key = event.key.toLowerCase();
  if (key === "enter") {
    event.preventDefault();
    if (editingFolder.value === null) void send();
  } else if (key === "s") {
    event.preventDefault();
    if (editingFolder.value !== null) void folderEditor.value?.save();
    else void save();
  } else if (key === "k") {
    event.preventDefault();
    sidebar.value?.focusSearch();
  } else if (key === "w") {
    event.preventDefault();
    closeTab(selectedId.value);
  } else if (key === "tab") {
    event.preventDefault();
    cycleTab(event.shiftKey ? -1 : 1);
  } else if (key === "/") {
    event.preventDefault();
    showShortcuts.value = !showShortcuts.value;
  }
}

onMounted(async () => {
  window.addEventListener("keydown", onKey);
  info.value = await coreInfo();
  const last = backend.persistent ? localStorage.getItem(LAST_ROOT) : "memory";
  if (last) await openRoot(last);
});

onBeforeUnmount(() => window.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="layout">
    <Sidebar
      ref="sidebar"
      :workspace="workspace"
      :selected-id="selectedId"
      :folder-id="folderId"
      :dirty="dirty"
      :persistent="backend.persistent"
      @select="select"
      @folder="folderId = $event"
      @open="openFolder"
      @new-request="newRequest"
      @new-folder="newFolder"
      @import="showImport = true"
      @rename="renameNode"
      @duplicate="duplicateNode"
      @delete="deleteNode"
      @move="moveNode"
      @edit-folder="editingFolder = $event"
      @copy-curl="copyCurl"
      @add-example="addExample"
    />

    <main v-if="workspace && editingFolder !== null" class="folder-main">
      <p v-if="problem" class="problem">{{ problem.message }}</p>
      <FolderEditor
        ref="folderEditor"
        :key="editingFolder"
        :workspace="workspace"
        :folder-id="editingFolder"
        @close="editingFolder = null"
        @saved="onFolderSaved"
      />
    </main>

    <main v-if="workspace && selected && editingFolder === null">
      <nav class="tabbar">
        <button
          v-for="tab in tabs"
          :key="tab.id"
          class="tab"
          :class="{ on: tab.id === selectedId, preview: tab.id === previewTab }"
          :title="tab.id === previewTab ? `${tab.id} · preview: double-click to keep it open` : tab.id"
          @click="selectedId = tab.id"
          @dblclick="pinTab(tab.id)"
          @auxclick.middle.prevent="closeTab(tab.id)"
        >
          <span class="badge" :class="`m-${tab.request.method.toLowerCase()}`">{{
            tab.protocol === "http" ? tab.request.method : tab.protocol.toUpperCase().slice(0, 4)
          }}</span>
          <span class="tab-name">{{ tab.name }}</span>
          <span v-if="dirty.has(tab.id)" class="dirty" title="Unsaved changes"></span>
          <span class="close" title="Close (Ctrl+W)" @click.stop="closeTab(tab.id)">✕</span>
        </button>
        <span class="spacer"></span>
        <button class="ghost small" title="Keyboard shortcuts (Ctrl+/)" @click="showShortcuts = true">⌨</button>
      </nav>

      <header class="topbar">
        <div class="crumbs">
          <span class="muted">{{ folderName }}</span>
          <span class="muted">/</span>
          <input v-model="selected.name" class="title" spellcheck="false" />
        </div>
        <div class="actions">
          <select v-model="activeEnvId" class="env" title="Environment used to resolve variables">
            <option value="">No environment</option>
            <option v-for="env in environments" :key="env.id" :value="env.id">{{ env.name }}</option>
          </select>
          <button class="ghost" title="Manage environments" @click="showEnvironments = true">⚙</button>
          <button class="ghost" title="Cookies of this environment" @click="showCookies = true">🍪</button>
          <button class="ghost" title="Show as code: curl, fetch, Python, Java, Go, C#…" @click="showCode = true">&lt;/&gt;</button>
          <span v-if="notice" class="notice" role="status">{{ notice }}</span>
          <button class="ghost danger" :class="{ armed: confirmDelete }" @click="deleteSelected">
            {{ confirmDelete ? "Click again to delete" : "Delete" }}
          </button>
          <button class="ghost" :disabled="!dirty.has(selected.id)" title="Ctrl+S" @click="save()">
            {{ dirty.has(selected.id) ? "Save" : "Saved" }}
          </button>
          <button
            class="ghost"
            :title="`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`"
            @click="theme = theme === 'dark' ? 'light' : 'dark'"
          >
            {{ theme === "dark" ? "☀" : "☾" }}
          </button>
        </div>
      </header>

      <p v-if="problem" class="problem">{{ problem.message }}</p>

      <RequestEditor
        ref="requestEditor"
        :key="selected.id"
        :request="selected.request"
        :scripts="selected.scripts"
        :sending="result.sending"
        :supported="selected.protocol === 'http'"
        :preview="preview"
        :inherited-auth="selectedInheritedAuth"
        :inherited-settings="selectedInheritedSettings"
        :inherited-headers="selectedInheritedHeaders"
        :request-id="selected.id"
        :version="info?.version ?? ''"
        :environment-name="environments.find((e) => e.id === activeEnvId)?.name ?? ''"
        :root="workspace?.root ?? ''"
        @send="send"
        @save="save()"
      />

      <ResponseViewer
        :response="result.response"
        :error="result.error"
        :sending="result.sending"
        :tests="result.tests"
        :logs="result.logs"
        :script-errors="result.scriptErrors"
        :history="historyItems"
        :history-index="historyPick[selectedId] ?? 0"
        :url="preview?.url ?? selected.request.url"
        @pick="historyPick[selectedId] = $event"
        @add-test="addTest"
        @save-example="saveExample"
      />

      <footer class="statusbar">
        <span>pulpo-core {{ info?.version ?? "…" }}</span>
        <span class="muted">{{ selected.id }}</span>
      </footer>
    </main>

    <div v-if="showShortcuts" class="backdrop" @click.self="showShortcuts = false">
      <section class="card shortcuts" role="dialog" aria-label="Keyboard shortcuts">
        <h2>Keyboard shortcuts</h2>
        <dl>
          <template v-for="[keys, action] in SHORTCUTS" :key="keys">
            <dt><kbd>{{ keys }}</kbd></dt>
            <dd>{{ action }}</dd>
          </template>
        </dl>
      </section>
    </div>

    <EnvironmentPanel
      v-if="showEnvironments && workspace"
      :environments="environments"
      :active-id="activeEnvId"
      :persistent="backend.persistent"
      @close="showEnvironments = false"
      @save="saveEnvironment"
      @create="createEnvironment"
      @remove="deleteEnvironment"
    />

    <CodePanel
      v-if="showCode && workspace && selected"
      :root="workspace.root"
      :entry="toEntry(selected)"
      :environment-id="activeEnvId"
      :environment-name="environments.find((e) => e.id === activeEnvId)?.name ?? ''"
      @close="showCode = false"
    />

    <CookiesPanel
      v-if="showCookies && workspace"
      :root="workspace.root"
      :environment-id="activeEnvId"
      :environment-name="environments.find((e) => e.id === activeEnvId)?.name ?? ''"
      @close="showCookies = false"
    />

    <ImportDialog
      v-if="showImport && workspace"
      ref="importDialog"
      :target="folderId"
      :available="!!backend.importInto"
      :blocked="importBlocked"
      @close="showImport = false"
      @file="importFile"
      @text="runImport({ text: $event })"
    />

    <main v-if="!(workspace && (selected || editingFolder !== null))" class="welcome">
      <div class="hello card">
        <img src="/pulpo.svg" width="72" height="72" alt="" />
        <h1>{{ workspace ? workspace.name : "Welcome to Pulpo" }}</h1>
        <template v-if="workspace">
          <p v-if="requests.length" class="muted">Pick a request in the sidebar, or press <kbd>Ctrl+K</kbd> to search.</p>
          <p v-else class="muted">This workspace has no requests yet. Use <strong>+ Request</strong> or <strong>Import</strong> in the sidebar.</p>
        </template>
        <template v-else>
          <p class="muted">Your collections live as plain files in a folder of your repo.</p>
          <div class="buttons">
            <button class="primary" @click="openFolder">Open a workspace folder</button>
            <button class="ghost" @click="createWorkspace">Create a workspace in a folder</button>
          </div>
        </template>
        <p v-if="problem" class="problem">{{ problem.message }}</p>
      </div>
    </main>
  </div>
</template>

<style scoped>
.layout {
  display: grid;
  grid-template-columns: clamp(200px, 24vw, 290px) minmax(0, 1fr);
  height: 100%;
}
main {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 14px 18px;
  min-width: 0;
  min-height: 0;
  overflow-y: auto;
}
.topbar,
.actions,
.crumbs,
.statusbar {
  display: flex;
  align-items: center;
  gap: 10px;
}
.topbar {
  justify-content: space-between;
}
.muted {
  color: var(--muted);
}
.title {
  border: 1px solid transparent;
  background: transparent;
  font-weight: 700;
  padding: 2px 6px;
  border-radius: 6px;
  min-width: 240px;
}
.title:hover,
.title:focus {
  border-color: var(--border);
  outline: none;
}
.env {
  border: 1px solid var(--border);
  background: var(--surface);
  border-radius: 8px;
  padding: 6px 8px;
  cursor: pointer;
  max-width: 180px;
}
.ghost {
  border: 1px solid var(--border);
  background: var(--surface);
  border-radius: 8px;
  padding: 6px 12px;
  cursor: pointer;
}
.ghost:disabled {
  color: var(--muted);
  cursor: default;
}
.danger.armed {
  border-color: var(--m-delete);
  color: var(--m-delete);
}
.problem {
  margin: 0;
  padding: 8px 12px;
  border-left: 3px solid var(--m-delete);
  background: var(--surface);
  font-family: var(--mono);
  font-size: 12px;
}
.statusbar {
  justify-content: space-between;
  font-size: 12px;
}
.tabbar {
  display: flex;
  align-items: center;
  gap: 4px;
  overflow-x: auto;
  min-width: 0;
  margin: -4px 0 -6px;
}
.tab {
  display: flex;
  align-items: center;
  gap: 6px;
  max-width: 220px;
  border: 1px solid transparent;
  border-radius: 8px;
  background: transparent;
  padding: 5px 8px;
  cursor: pointer;
  color: var(--muted);
  flex: none;
}
.tab:hover {
  background: var(--surface);
}
.tab.on {
  background: var(--surface);
  border-color: var(--border);
  color: var(--text);
}
.tab .badge {
  font: 600 10px var(--mono);
}
.tab-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tab .dirty {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--accent);
  flex: none;
}
.tab .close {
  opacity: 0;
  font-size: 11px;
  padding: 0 2px;
}
.tab:hover .close,
.tab.on .close {
  opacity: 0.7;
}
.m-get { color: var(--m-get); }
.m-post { color: var(--m-post); }
.m-put { color: var(--m-put); }
.m-patch { color: var(--m-patch); }
.m-delete { color: var(--m-delete); }
.spacer {
  flex: 1;
}
.small {
  padding: 3px 8px;
}
.backdrop {
  position: fixed;
  inset: 0;
  background: rgba(10, 12, 20, 0.45);
  display: grid;
  place-items: center;
  z-index: 10;
}
.shortcuts {
  min-width: 380px;
}
.shortcuts h2 {
  margin: 0 0 12px;
  font-size: 17px;
}
.shortcuts dl {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 8px 18px;
  margin: 0;
}
.shortcuts dd {
  margin: 0;
  color: var(--muted);
}
kbd {
  font: 12px var(--mono);
  padding: 2px 6px;
  border: 1px solid var(--border);
  border-bottom-width: 2px;
  border-radius: 5px;
  background: var(--surface-2);
}
main > * {
  flex: none;
}
/* The response panel takes the remaining height; below 240 px the page scrolls instead of overlapping. */
main > :deep(.viewer) {
  flex: 1 0 240px;
}
.welcome {
  display: grid;
  place-items: center;
}
.hello {
  max-width: 460px;
  text-align: center;
  padding: 36px;
}
.hello h1 {
  margin: 12px 0 4px;
  font-size: 22px;
}
.buttons {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 20px;
}
.primary {
  border: 0;
  border-radius: 8px;
  padding: 10px 16px;
  background: var(--accent);
  color: #fff;
  font-weight: 600;
  cursor: pointer;
}
:deep(.card) {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
  padding: 14px;
  min-width: 0;
}
.notice {
  align-self: center;
  font-size: 12px;
  color: var(--m-get);
  white-space: nowrap;
}
.tab.preview .tab-name {
  font-style: italic;
}
</style>
