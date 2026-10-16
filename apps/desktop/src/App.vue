<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import Sidebar from "./components/Sidebar.vue";
import RequestEditor from "./components/RequestEditor.vue";
import ResponseViewer from "./components/ResponseViewer.vue";
import EnvironmentPanel from "./components/EnvironmentPanel.vue";
import { resolveText } from "./variables";
import { asSendError, coreInfo, sendHttp, type CoreInfo, type HttpResponse, type SendError } from "./core";
import {
  asCollectionError,
  backend,
  toEntry,
  type CollectionError,
  type Environment,
  type RequestEntry,
  type TreeNode,
  type Workspace,
} from "./workspace";

interface Result {
  response: HttpResponse | null;
  error: SendError | null;
  sending: boolean;
}

type RequestNode = Extract<TreeNode, { type: "request" }>;

const LAST_ROOT = "pulpo.lastWorkspace";

const workspace = ref<Workspace | null>(null);
const saved = reactive(new Map<string, string>());
const selectedId = ref("");
const folderId = ref("");
const results = reactive<Record<string, Result>>({});
const problem = ref<CollectionError | null>(null);
const confirmDelete = ref(false);
const environments = ref<Environment[]>([]);
const activeEnvId = ref("");
const showEnvironments = ref(false);

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

const preview = computed(() => {
  const node = selected.value;
  if (!node) return null;
  const missing = new Set<string>();
  const url = resolveText(node.request.url, previewVars.value, missing, true);
  for (const h of node.request.headers.filter((h) => h.enabled)) {
    resolveText(h.name, previewVars.value, missing, true);
    resolveText(h.value, previewVars.value, missing, true);
  }
  if (node.request.body) resolveText(node.request.body, previewVars.value, missing, true);
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
const result = computed<Result>(() => results[selectedId.value] ?? { response: null, error: null, sending: false });

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

function select(entry: RequestEntry) {
  selectedId.value = entry.id;
  confirmDelete.value = false;
  folderId.value = entry.id.includes("/") ? entry.id.slice(0, entry.id.lastIndexOf("/")) : "";
}

async function send() {
  const node = selected.value;
  if (!node || node.protocol !== "http" || results[node.id]?.sending) return;
  results[node.id] = { response: null, error: null, sending: true };
  try {
    const environmentId = activeEnvId.value || undefined;
    const response = await sendHttp(node.request, {
      root: workspace.value?.root,
      environmentId,
      browserVars: environmentId ? backend.browserVariables?.(environmentId) : {},
    });
    results[node.id] = { response, error: null, sending: false };
  } catch (e) {
    results[node.id] = { response: null, error: asSendError(e), sending: false };
  }
}

function onKey(event: KeyboardEvent) {
  if (!(event.ctrlKey || event.metaKey)) return;
  if (event.key === "Enter") {
    event.preventDefault();
    void send();
  } else if (event.key.toLowerCase() === "s") {
    event.preventDefault();
    void save();
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
    />

    <main v-if="workspace && selected">
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
        :key="selected.id"
        :request="selected.request"
        :sending="result.sending"
        :supported="selected.protocol === 'http'"
        :preview="preview"
        @send="send"
      />

      <ResponseViewer :response="result.response" :error="result.error" :sending="result.sending" />

      <footer class="statusbar">
        <span>pulpo-core {{ info?.version ?? "…" }}</span>
        <span class="muted">{{ selected.id }}</span>
      </footer>
    </main>

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

    <main v-if="!(workspace && selected)" class="welcome">
      <div class="hello card">
        <img src="/pulpo.svg" width="72" height="72" alt="" />
        <h1>{{ workspace ? workspace.name : "Welcome to Pulpo" }}</h1>
        <template v-if="workspace">
          <p class="muted">This workspace has no requests yet. Use <strong>+ Request</strong> in the sidebar.</p>
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
  grid-template-columns: 290px 1fr;
  height: 100%;
}
main {
  display: grid;
  grid-template-rows: auto auto minmax(220px, auto) 1fr auto;
  gap: 14px;
  padding: 14px 18px;
  min-width: 0;
  min-height: 0;
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
</style>
