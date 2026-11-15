<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch, watchEffect } from "vue";
import { varContext } from "./varContext";
import Sidebar from "./components/Sidebar.vue";
import RequestEditor from "./components/RequestEditor.vue";
import ResponseViewer from "./components/ResponseViewer.vue";
import WebSocketPanel from "./components/WebSocketPanel.vue";
import SsePanel from "./components/SsePanel.vue";
import MqttPanel from "./components/MqttPanel.vue";
import EnvironmentPanel from "./components/EnvironmentPanel.vue";
import CookiesPanel from "./components/CookiesPanel.vue";
import TokensPanel from "./components/TokensPanel.vue";
import FolderEditor from "./components/FolderEditor.vue";
import RunnerPanel from "./components/RunnerPanel.vue";
import CodePanel from "./components/CodePanel.vue";
import ImportDialog from "./components/ImportDialog.vue";
import ExportDialog from "./components/ExportDialog.vue";
import GitPanel from "./components/GitPanel.vue";
import FlowView from "./components/FlowView.vue";
import ContractsPanel from "./components/ContractsPanel.vue";
import MockPanel from "./components/MockPanel.vue";
import OpenApiEditor from "./components/OpenApiEditor.vue";
import CommandPalette, { type PaletteItem } from "./components/CommandPalette.vue";
import { AUTHOR, AUTHOR_URL, ISSUES_URL, REPO_URL } from "./about";
import { applyPasted, parsePaste } from "./paste";
import { LOCALES, locale, setLocale, t, type Locale } from "./i18n";
import { resolveText } from "./variables";
import { applyPathParams } from "./params";
import { applyAuth, asSendError, coreInfo, isEventStream, isMqtt, isWebSocketUrl, resolveAuth, type CoreInfo, type HttpResponse, type SendError } from "./core";
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
  type Flow,
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
const RECENT_ROOTS = "pulpo.recentWorkspaces";

/** Workspaces opened on this machine, newest first: where pulpo:// links are looked for. */
function recentRoots(): string[] {
  try {
    const list = JSON.parse(localStorage.getItem(RECENT_ROOTS) ?? "[]");
    return Array.isArray(list) ? list.filter((r): r is string => typeof r === "string") : [];
  } catch {
    return [];
  }
}

function rememberRoot(root: string) {
  try {
    localStorage.setItem(LAST_ROOT, root);
    const list = [root, ...recentRoots().filter((r) => r !== root)].slice(0, 20);
    localStorage.setItem(RECENT_ROOTS, JSON.stringify(list));
  } catch {
    /* storage unavailable: links then ask for the folder */
  }
}

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
/** Folder being exported ("" = the whole collection); null = dialog closed. */
const exporting = ref<string | null>(null);
const showGit = ref(false);

// ---- API contracts
const showContracts = ref(false);
const showMock = ref(false);
const showAbout = ref(false);
const showPalette = ref(false);

/** Folder names of every request ("Orders / Admin"), for the palette. */
const folderLabels = computed(() => {
  const out = new Map<string, string>();
  const walk = (nodes: TreeNode[], path: string[]) => {
    for (const n of nodes) {
      if (n.type === "folder") walk(n.children, [...path, n.name]);
      else out.set(n.id, path.join(" / "));
    }
  };
  walk(workspace.value?.tree ?? [], []);
  return out;
});

const PROTOCOL_BADGES: Record<string, string> = {
  graphql: "GQL",
  websocket: "WS",
  grpc: "gRPC",
  soap: "SOAP",
  sse: "SSE",
  socketio: "SIO",
  mqtt: "MQTT",
};

const paletteItems = computed<PaletteItem[]>(() => {
  const items: PaletteItem[] = requests.value.map((n) => ({
    id: `request:${n.id}`,
    label: n.name,
    group: "Request",
    hint: folderLabels.value.get(n.id) ?? "",
    badge: n.protocol === "http" ? n.request.method : PROTOCOL_BADGES[n.protocol] ?? n.protocol.toUpperCase(),
    run: () => select(n),
  }));
  for (const f of flows.value) {
    items.push({ id: `flow:${f.id}`, label: f.name, group: "Flow", run: () => openFlow(f.id) });
  }
  items.push({ id: "env:", label: t("No environment"), group: "Environment", run: () => (activeEnvId.value = "") });
  for (const env of environments.value) {
    items.push({ id: `env:${env.id}`, label: t("Use {0}", { 0: env.name }), group: "Environment", run: () => (activeEnvId.value = env.id) });
  }
  // English text in, shown translated and still found by its English words.
  const action = (id: string, text: string, run: () => void, hint = "") =>
    items.push({ id: `action:${id}`, label: t(text), keywords: text, group: "Action", hint, run });
  if (selected.value) {
    action("send", "Send the request", () => void send(), "Ctrl+Enter");
    action("save", "Save the request", () => void save(), "Ctrl+S");
    action("close", "Close the tab", () => closeTab(selectedId.value), "Ctrl+W");
    action("code", "Show the request as code", () => (showCode.value = true));
    action("curl", "Copy as curl", () => void copyCurl(selectedId.value));
  }
  action("new-request", "New request", () => void newRequest(t("New request")));
  action("new-folder", "New folder", () => void newFolder(t("New folder")));
  action("import", "Import…", () => (showImport.value = true), t("Postman, OpenAPI, curl…"));
  action("export", "Export…", () => (exporting.value = ""), t("Postman, OpenAPI, .http, HTML"));
  action("run", "Run the collection", () => openRunner(""));
  action("envs", "Manage environments", () => (showEnvironments.value = true));
  action("cookies", "Cookies", () => (showCookies.value = true));
  action("tokens", "OAuth 2.0 tokens", () => (showTokens.value = true));
  if (backend.persistent) {
    action("git", "Git: changes and commit", () => (showGit.value = true));
    action("contracts", "API contracts", () => (showContracts.value = true), "OpenAPI, .proto, WSDL, GraphQL");
    action("mock", "Mock server", () => (showMock.value = true));
    action("open", "Open another folder", () => void openFolder());
  }
  const toggleTheme = () => (theme.value = theme.value === "dark" ? "light" : "dark");
  if (theme.value === "dark") action("theme", "Switch to light theme", toggleTheme);
  else action("theme", "Switch to dark theme", toggleTheme);
  action("shortcuts", "Keyboard shortcuts", () => (showShortcuts.value = true), "Ctrl+/");
  action("about", "About Pulpo", () => (showAbout.value = true));
  for (const l of LOCALES) {
    if (l.code !== locale.value) {
      // "Language" in English too, for whoever lands in a language they cannot read.
      items.push({ id: `lang:${l.code}`, label: `${t("Language")}: ${l.name}`, group: t("Language"), hint: "Language", run: () => void setLocale(l.code) });
    }
  }
  return items;
});

function openLink(url: string) {
  if (backend.openLink) void backend.openLink(url);
  else window.open(url, "_blank", "noopener");
}
/** The OpenAPI file open in the editor (relative to the workspace). */
const editingContract = ref<string | null>(null);
/** Folders out of step with their contract (checked in the background on open). */
const driftCount = ref(0);
async function checkDrift() {
  const ws = workspace.value;
  if (!ws || !backend.syncStatus) return;
  try {
    const status = await backend.syncStatus(ws.root);
    driftCount.value = status.filter(
      (s) => "Ok" in s && (s.Ok.added.length || s.Ok.removed.length || s.Ok.changed.length),
    ).length;
  } catch {
    driftCount.value = 0;
  }
}
watch(() => workspace.value?.root, () => void checkDrift(), { immediate: true });
async function afterContracts() {
  await organize(async () => {});
  await loadFlows();
  await checkDrift();
}

// ---- flows
const flows = ref<Flow[]>([]);
/** The flow open in the main area; null = none. */
const flowView = ref<string | null>(null);
/** Request id → names of the flows that use it. */
const flowUsage = ref<Record<string, string[]>>({});
/** A request waiting for the user to pick the flow it goes into. */
const addingToFlow = ref<string | null>(null);

async function loadFlows() {
  const ws = workspace.value;
  if (!ws || !backend.listFlows) return;
  try {
    flows.value = await backend.listFlows(ws.root);
    flowUsage.value = (await backend.flowUsage?.(ws.root)) ?? {};
  } catch {
    flows.value = [];
  }
}
watch(() => workspace.value?.root, () => void loadFlows(), { immediate: true });

function openFlow(id: string) {
  editingFolder.value = null;
  runnerFolder.value = null;
  flowView.value = id;
}

async function newFlow(name: string) {
  const ws = workspace.value;
  if (!ws || !backend.createFlow) return;
  try {
    const created = await backend.createFlow(ws.root, name);
    await loadFlows();
    openFlow(created.id);
  } catch (e) {
    problem.value = { kind: "io", message: String(e) };
  }
}

async function addToFlow(flowId: string, name?: string) {
  const ws = workspace.value;
  const requestId = addingToFlow.value;
  if (!ws || !requestId || !backend.addFlowStep) return;
  try {
    const target = flowId || (await backend.createFlow!(ws.root, name || t("New flow"))).id;
    await backend.addFlowStep(ws.root, target, requestId);
    addingToFlow.value = null;
    await loadFlows();
    openFlow(target);
    flowKey.value++;
  } catch (e) {
    problem.value = { kind: "io", message: String(e) };
  }
}
const newFlowForStep = ref("");

/** "Make a flow from this folder": the folder waiting for confirmation, and the result. */
const chaining = ref<{ folder: string; name: string; run: boolean; busy: boolean; result: string } | null>(null);
function startChain(folderId: string) {
  let name = "";
  walk(workspace.value?.tree ?? [], (n) => {
    if (n.type === "folder" && n.id === folderId) name = n.name;
  });
  chaining.value = { folder: folderId, name: name || t("Flow"), run: false, busy: false, result: "" };
}
async function confirmChain() {
  const c = chaining.value;
  const ws = workspace.value;
  if (!c || !ws || !backend.chainToFlow) return;
  c.busy = true;
  try {
    const [id, text] = await backend.chainToFlow(ws.root, c.folder, activeEnvId.value, c.run, c.name);
    c.result = text;
    await loadFlows();
    openFlow(id);
    flowKey.value++;
  } catch (e) {
    c.result = String(e);
  }
  c.busy = false;
}
/** Bumped to reload the open flow after it changes elsewhere. */
const flowKey = ref(0);

/** The first flow that uses a request. */
function openFlowOf(requestId: string) {
  const name = flowUsage.value[requestId]?.[0];
  const found = flows.value.find((f) => f.name === name);
  if (found) openFlow(found.id);
}

function openRequestFromFlow(id: string) {
  flowView.value = null;
  const node = requests.value.find((n) => n.id === id);
  if (node) select(toEntry(node));
}
const importDialog = ref<InstanceType<typeof ImportDialog> | null>(null);
const importBlocked = computed(() =>
  dirty.value.size ? `Save your ${dirty.value.size} unsaved request(s) first (Ctrl+S), so the import does not overwrite them.` : null,
);

async function runImport(source: { path?: string; text?: string }) {
  const ws = workspace.value;
  if (!ws || !backend.importInto) return;
  try {
    folderId.value = existingFolder(folderId.value);
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
const showTokens = ref(false);
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
    flash(t("Saved as an example: see the Examples tab"));
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

/** "Open request" from a runner result. */
function openFromRunner(id: string) {
  runnerFolder.value = null;
  selectedId.value = id;
  pinTab(id);
}

/** "Add example" from the tree: open the request with a new example to fill in. */
async function addExample(id: string) {
  editingFolder.value = null;
  runnerFolder.value = null;
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
  flash(t("Test added: see the Tests tab, then save"));
}

/** A pulpo:// link waiting for its workspace folder to be picked. */
const pendingLink = ref<{ text: string; name: string; repo: string; workspace: string } | null>(null);

async function copyLink(id: string) {
  const ws = workspace.value;
  if (!ws || !backend.requestLink) return;
  try {
    await navigator.clipboard.writeText(await backend.requestLink(ws.root, id));
    flash(t("Link copied: whoever has this repository can open it in Pulpo"));
  } catch (e) {
    problem.value = { kind: "io", message: String(e) };
  }
}

/** Opens the request of a pulpo:// link, in this workspace or another known one. */
/** The last link followed: Windows and Linux deliver a clicked link twice (the
 * single-instance and deep-link plugins), and opening a workspace twice at once races. */
let lastLink = { text: "", at: 0 };

async function followLink(text: string, roots = recentRoots()) {
  if (!backend.resolveLink) return;
  const key = text.trim().replace("pulpo://open/?", "pulpo://open?");
  if (key === lastLink.text && Date.now() - lastLink.at < 2000) return;
  lastLink = { text: key, at: Date.now() };
  try {
    const current = workspace.value?.root;
    const candidates = current ? [current, ...roots.filter((r) => r !== current)] : roots;
    const target = await backend.resolveLink(text, candidates);
    if (!target.root) {
      pendingLink.value = { text, ...target.link };
      return;
    }
    pendingLink.value = null;
    if (target.root !== current) {
      if (dirty.value.size) {
        problem.value = {
          kind: "io",
          message: t("Save your {0} unsaved request(s) first (Ctrl+S): the link opens another workspace.", { 0: dirty.value.size }),
        };
        return;
      }
      await openRoot(target.root);
    }
    const node = requests.value.find((n) => n.id === target.link.request);
    if (node) select(toEntry(node));
    else problem.value = { kind: "io", message: t("The link's request is not in this workspace any more ({0}).", { 0: target.link.request }) };
  } catch (e) {
    problem.value = { kind: "io", message: String(e) };
  }
}

/** The link's workspace is not known here yet: the user points at their clone. */
async function pickLinkFolder() {
  const pending = pendingLink.value;
  const root = await backend.pickFolder();
  if (!pending || !root) return;
  const target = await backend.resolveLink!(pending.text, [root]);
  if (target.root) await followLink(pending.text, [root]);
  else
    problem.value = {
      kind: "io",
      message: `That folder is not ${pending.name || "the link's workspace"}${pending.repo ? ` (${pending.repo}${pending.workspace ? `, folder ${pending.workspace}` : ""})` : ""}.`,
    };
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
    flash(t("Copied “{0}” as curl", { 0: node.name }));
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}
/** The folder being edited ("" = the collection); null = editing requests. */
const editingFolder = ref<string | null>(null);
/** The folder whose runner is open ("" = the whole collection). */
const runnerFolder = ref<string | null>(null);
watch(editingFolder, (id) => {
  if (id !== null) {
    runnerFolder.value = null;
    flowView.value = null;
  }
});
function openRunner(id: string) {
  editingFolder.value = null;
  flowView.value = null;
  runnerFolder.value = id;
}
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
const requestNames = computed(() => Object.fromEntries(requests.value.map((n) => [n.id, n.name])));

/** Opens a request picked in the tokens panel. */
function openFromTokens(id: string) {
  const node = requests.value.find((n) => n.id === id);
  if (!node) return;
  showTokens.value = false;
  select(toEntry(node));
}
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
    rememberRoot(root);
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
  const name = root.split(/[\\/]/).filter(Boolean).pop() ?? t("My API");
  try {
    adopt(await backend.init(root, name));
    rememberRoot(root);
  } catch (e) {
    problem.value = asCollectionError(e);
  }
}

/** The folder itself if it still exists, else its nearest existing parent ("" = the root). */
function existingFolder(id: string): string {
  let current = id;
  while (current && childrenOf(current) === null) {
    current = current.includes("/") ? current.slice(0, current.lastIndexOf("/")) : "";
  }
  return current;
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
    folderId.value = existingFolder(folderId.value);
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

/** Ctrl+V outside a text field: a curl command or a URL becomes a new request, saved. */
async function onPaste(event: ClipboardEvent) {
  const target = event.target as HTMLElement | null;
  if (target?.closest("input, textarea, select, [contenteditable], .cm-editor")) return;
  const pasted = parsePaste(event.clipboardData?.getData("text/plain") ?? "");
  if (!pasted || !workspace.value) return;
  event.preventDefault();
  await newRequest(pasted.name);
  const node = selected.value;
  if (!node || node.name !== pasted.name) return;
  applyPasted(node.request, pasted);
  await save(node);
}

async function newFolder(name: string) {
  if (!workspace.value) return;
  try {
    folderId.value = existingFolder(folderId.value);
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
      message: t("Save your {0} unsaved request(s) first (Ctrl+S): organizing moves files and reloads the tree.", { 0: dirty.value.size }),
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
    // A deleted folder is no place to create or import into.
    folderId.value = existingFolder(map(folderId.value));
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
  flowView.value = null;
  runnerFolder.value = null;
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
  if (!node || sending.has(node.id)) return;
  // Live protocols (WebSocket, SSE, Socket.IO, MQTT) connect from their panel, not Send.
  const live =
    ["websocket", "sse", "socketio", "mqtt"].includes(node.protocol) ||
    isWebSocketUrl(node.request.url) ||
    isEventStream(node.request) ||
    !!node.request.socketio ||
    isMqtt(node.request);
  if (live) return;
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
  ["Ctrl+P", "Command palette: requests, flows, environments and actions (> for actions)"],
  ["Ctrl+K", "Search requests"],
  ["Ctrl+W", "Close the tab"],
  ["Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"],
  ["Ctrl+V", "Paste a curl or a URL: new request (in the URL bar: fills this one)"],
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
    if (editingFolder.value === null && runnerFolder.value === null) void send();
  } else if (key === "s") {
    event.preventDefault();
    if (editingFolder.value !== null) void folderEditor.value?.save();
    else void save();
  } else if (key === "p") {
    event.preventDefault();
    showPalette.value = !showPalette.value;
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
  window.addEventListener("paste", onPaste);
  info.value = await coreInfo();
  const last = backend.persistent ? localStorage.getItem(LAST_ROOT) : "memory";
  if (last) await openRoot(last);
  void backend.onLinks?.((links) => {
    const last = links[links.length - 1];
    if (last) void followLink(last);
  });
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("paste", onPaste);
});
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
      @export="exporting = $event"
      @git="showGit = true"
      :drift-count="driftCount"
      @contracts="showContracts = true"
      @mock="showMock = true"
      :flows="flows"
      :open-flow-id="flowView"
      @open-flow="openFlow"
      @new-flow="newFlow"
      @add-to-flow="(id) => { addingToFlow = id; newFlowForStep = ''; }"
      @chain-folder="startChain"
      @rename="renameNode"
      @duplicate="duplicateNode"
      @delete="deleteNode"
      @move="moveNode"
      @edit-folder="editingFolder = $event"
      @copy-curl="copyCurl"
      @copy-link="copyLink"
      @link="followLink"
      @add-example="addExample"
      @run-folder="openRunner"
    />

    <main v-if="workspace && flowView !== null" class="folder-main">
      <FlowView
        :key="`${flowView}-${flowKey}`"
        :root="workspace.root"
        :flow-id="flowView"
        :environment-id="activeEnvId"
        :environment-name="environments.find((e) => e.id === activeEnvId)?.name ?? ''"
        @close="flowView = null"
        @open="openRequestFromFlow"
        @changed="loadFlows"
        @deleted="flowView = null; loadFlows()"
      />
    </main>

    <main v-if="workspace && runnerFolder !== null && editingFolder === null && flowView === null" class="folder-main">
      <RunnerPanel
        :key="runnerFolder"
        :workspace="workspace"
        :folder-id="runnerFolder"
        :environments="environments"
        :environment-id="activeEnvId"
        @close="runnerFolder = null"
        @open="openFromRunner"
      />
    </main>

    <main v-if="workspace && editingFolder !== null && flowView === null" class="folder-main">
      <p v-if="problem" class="problem">{{ problem.message }}</p>
      <FolderEditor
        ref="folderEditor"
        :key="editingFolder"
        :workspace="workspace"
        :folder-id="editingFolder"
        :environment-id="activeEnvId"
        @close="editingFolder = null"
        @saved="onFolderSaved"
      />
    </main>

    <main v-if="workspace && selected && editingFolder === null && runnerFolder === null && flowView === null">
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
            isMqtt(tab.request) ? "MQTT" : tab.request.socketio ? "SIO" : isWebSocketUrl(tab.request.url) ? "WS" : /^\s*grpcs?:\/\//i.test(tab.request.url) ? "gRPC" : isEventStream(tab.request) ? "SSE" : tab.request.soap ? "SOAP" : tab.request.graphql ? "GQL" : tab.protocol === "http" ? tab.request.method : tab.protocol.toUpperCase().slice(0, 4)
          }}</span>
          <span class="tab-name">{{ tab.name }}</span>
          <span v-if="dirty.has(tab.id)" class="dirty" :title="$t('Unsaved changes')"></span>
          <span class="close" :title="$t('Close (Ctrl+W)')" @click.stop="closeTab(tab.id)">✕</span>
        </button>
        <span class="spacer"></span>
        <select
          class="lang"
          :value="locale"
          :title="$t('Language')"
          :aria-label="$t('Language')"
          @change="setLocale(($event.target as HTMLSelectElement).value as Locale)"
        >
          <option v-for="l in LOCALES" :key="l.code" :value="l.code">{{ l.name }}</option>
        </select>
        <button class="ghost small" :title="$t('Keyboard shortcuts (Ctrl+/)')" @click="showShortcuts = true">⌨</button>
        <button class="ghost small" :title="$t('About Pulpo')" @click="showAbout = true">ⓘ</button>
      </nav>

      <header class="topbar">
        <div class="crumbs">
          <span class="muted">{{ folderName }}</span>
          <span class="muted">/</span>
          <input v-model="selected.name" class="title" spellcheck="false" />
          <button
            v-if="flowUsage[selected.id]?.length"
            class="ghost small flows-chip"
            :title="`Used in: ${flowUsage[selected.id].join(', ')}`"
            @click="openFlowOf(selected.id)"
          >
            {{ $t(flowUsage[selected.id].length > 1 ? "⇢ In {0} flows" : "⇢ In 1 flow", { 0: flowUsage[selected.id].length }) }}
          </button>
        </div>
        <div class="actions">
          <select v-model="activeEnvId" class="env" :title="$t('Environment used to resolve variables')">
            <option value="">{{ $t("No environment") }}</option>
            <option v-for="env in environments" :key="env.id" :value="env.id">{{ env.name }}</option>
          </select>
          <button class="ghost" :title="$t('Manage environments')" @click="showEnvironments = true">⚙</button>
          <button class="ghost" :title="$t('Cookies of this environment')" @click="showCookies = true">🍪</button>
          <button class="ghost" :title="$t('OAuth 2.0 tokens of this environment')" @click="showTokens = true">🔑</button>
          <button class="ghost" :title="$t('Show as code: curl, fetch, Python, Java, Go, C#…')" @click="showCode = true">&lt;/&gt;</button>
          <span v-if="notice" class="notice" role="status">{{ notice }}</span>
          <button class="ghost danger" :class="{ armed: confirmDelete }" @click="deleteSelected">
            {{ confirmDelete ? $t("Click again to delete") : $t("Delete") }}
          </button>
          <button class="ghost" :disabled="!dirty.has(selected.id)" :title="$t('Ctrl+S')" @click="save()">
            {{ dirty.has(selected.id) ? $t("Save") : $t("Saved") }}
          </button>
          <button
            class="ghost"
            :title="$t(theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme')"
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
        :supported="['http', 'graphql', 'websocket', 'sse', 'grpc', 'soap', 'socketio', 'mqtt'].includes(selected.protocol) || isWebSocketUrl(selected.request.url) || isMqtt(selected.request)"
        :preview="preview"
        :inherited-auth="selectedInheritedAuth"
        :inherited-settings="selectedInheritedSettings"
        :inherited-headers="selectedInheritedHeaders"
        :request-id="selected.id"
        :version="info?.version ?? ''"
        :environment-name="environments.find((e) => e.id === activeEnvId)?.name ?? ''"
        :root="workspace?.root ?? ''"
        :environment-id="activeEnvId"
        @send="send"
        @save="save()"
      />

      <MqttPanel
        v-if="isMqtt(selected.request)"
        :key="`mqtt-${selected.id}`"
        :request="selected.request"
        :scripts="selected.scripts"
        :request-id="selected.id"
        :name="selected.name"
        :root="workspace?.root ?? ''"
        :environment-id="activeEnvId"
      />

      <WebSocketPanel
        v-else-if="isWebSocketUrl(selected.request.url) || selected.request.socketio"
        :key="`ws-${selected.id}-${!!selected.request.socketio}`"
        :request="selected.request"
        :scripts="selected.scripts"
        :request-id="selected.id"
        :name="selected.name"
        :root="workspace?.root ?? ''"
        :environment-id="activeEnvId"
      />

      <SsePanel
        v-else-if="isEventStream(selected.request)"
        :key="`sse-${selected.id}`"
        :request="selected.request"
        :scripts="selected.scripts"
        :request-id="selected.id"
        :name="selected.name"
        :root="workspace?.root ?? ''"
        :environment-id="activeEnvId"
      />

      <ResponseViewer
        v-else
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
        <button class="link-plain" :title="$t('About Pulpo')" @click="showAbout = true">
          {{ $t("Pulpo {0} · by {1}", { 0: info?.version ?? "…", 1: AUTHOR }) }}
        </button>
        <span class="muted">{{ selected.id }}</span>
      </footer>
    </main>

    <CommandPalette
      v-if="showPalette"
      :items="paletteItems"
      :placeholder="$t('Go to a request, flow or environment, or type > for actions')"
      :empty="$t('Nothing matches')"
      @close="showPalette = false"
    />

    <div v-if="showAbout" class="backdrop" @click.self="showAbout = false" @keydown.esc="showAbout = false">
      <section class="card about" role="dialog" :aria-label="$t('About Pulpo')">
        <h2>{{ $t("Pulpo") }} <span class="muted small">{{ info?.version ?? "" }}</span></h2>
        <p>{{ $t("The API client that follows your code. Free and open source.") }}</p>
        <p>
          {{ $t("Made by") }} <button class="link" @click="openLink(AUTHOR_URL)">{{ AUTHOR }}</button>
        </p>
        <p class="muted small">
          {{ $t("Found a bug or missing something?") }} <button class="link" @click="openLink(ISSUES_URL)">{{ $t("Tell me on GitHub") }}</button>
          {{ $t("and I will fix it.") }}
        </p>
        <p class="muted small">
          {{ $t("MIT or Apache-2.0 ·") }} <button class="link" @click="openLink(REPO_URL)">{{ $t("Source code") }}</button>
        </p>
        <label class="about-lang">
          {{ $t("Language") }}
          <select :value="locale" @change="setLocale(($event.target as HTMLSelectElement).value as Locale)">
            <option v-for="l in LOCALES" :key="l.code" :value="l.code">{{ l.name }}</option>
          </select>
        </label>
        <footer><button class="primary" @click="showAbout = false">{{ $t("Close") }}</button></footer>
      </section>
    </div>

    <div v-if="showShortcuts" class="backdrop" @click.self="showShortcuts = false">
      <section class="card shortcuts" role="dialog" :aria-label="$t('Keyboard shortcuts')">
        <h2>{{ $t("Keyboard shortcuts") }}</h2>
        <dl>
          <template v-for="[keys, action] in SHORTCUTS" :key="keys">
            <dt><kbd>{{ keys }}</kbd></dt>
            <dd>{{ $t(action) }}</dd>
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

    <TokensPanel
      v-if="showTokens && workspace"
      :root="workspace.root"
      :environment-id="activeEnvId"
      :environment-name="environments.find((e) => e.id === activeEnvId)?.name ?? ''"
      :names="requestNames"
      @open="openFromTokens"
      @close="showTokens = false"
    />

    <div v-if="pendingLink" class="link-ask card" role="alertdialog" :aria-label="$t('Open link')">
      <p>
        {{ $t("This link opens") }} <b>{{ pendingLink.name || pendingLink.text }}</b>
        <template v-if="pendingLink.repo">
          {{ $t("from") }} <code>{{ pendingLink.repo }}</code><template v-if="pendingLink.workspace">{{ $t(", folder") }} <code>{{ pendingLink.workspace }}</code></template>
        </template>{{ $t(". Pick that folder in your clone once; Pulpo remembers it.") }}
      </p>
      <div class="link-actions">
        <button class="ghost" @click="pendingLink = null">{{ $t("Cancel") }}</button>
        <button class="primary" @click="pickLinkFolder">{{ $t("Choose folder…") }}</button>
      </div>
    </div>

    <div v-if="chaining" class="link-ask card" role="dialog" :aria-label="$t('Make a flow')">
      <p>
        {{ $t("Pulpo finds which response feeds which request (") }}<code v-pre>{{orderId}}</code> ← <code>id</code> {{ $t("of") }}
        <code>POST /orders</code>{{ $t(") from saved examples and the folder's OpenAPI contract, and makes a flow that passes those values.") }}
      </p>
      <form class="flow-pick" @submit.prevent="confirmChain">
        <input v-model="chaining.name" :placeholder="$t('Flow name')" spellcheck="false" :aria-label="$t('Flow name')" />
        <button class="ghost" :disabled="chaining.busy || !chaining.name.trim()">
          {{ chaining.busy ? $t("Working…") : $t("Make flow") }}
        </button>
      </form>
      <label class="check">
        <input v-model="chaining.run" type="checkbox" />
        {{ $t("Also call the API in order to learn from real responses ({0})", { 0: environments.find((e) => e.id === activeEnvId)?.name || "no environment" }) }}
      </label>
      <pre v-if="chaining.result" class="chain-result">{{ chaining.result }}</pre>
      <div class="link-actions">
        <button class="ghost" @click="chaining = null">{{ $t("Close") }}</button>
      </div>
    </div>

    <div v-if="addingToFlow" class="link-ask card" role="dialog" :aria-label="$t('Add to flow')">
      <p>
        {{ $t("Add") }} <b>{{ requests.find((n) => n.id === addingToFlow)?.name ?? addingToFlow }}</b> {{ $t("as the last step of a flow:") }}
      </p>
      <div class="flow-pick">
        <button v-for="f in flows" :key="f.id" class="ghost" @click="addToFlow(f.id)">{{ f.name }}</button>
      </div>
      <form class="flow-pick" @submit.prevent="newFlowForStep.trim() && addToFlow('', newFlowForStep.trim())">
        <input v-model="newFlowForStep" :placeholder="$t('Or a new flow: Book a visit')" spellcheck="false" />
        <button class="ghost" :disabled="!newFlowForStep.trim()">{{ $t("Create") }}</button>
      </form>
      <div class="link-actions">
        <button class="ghost" @click="addingToFlow = null">{{ $t("Cancel") }}</button>
      </div>
    </div>

    <MockPanel v-if="showMock && workspace" :workspace="workspace" @close="showMock = false" />

    <ContractsPanel
      v-if="showContracts && workspace"
      :workspace="workspace"
      @changed="afterContracts"
      @open="(id) => { showContracts = false; openRequestFromFlow(id); }"
      @edit="(path) => { showContracts = false; editingContract = path; }"
      @close="showContracts = false"
    />

    <OpenApiEditor
      v-if="editingContract && workspace"
      :workspace="workspace"
      :contract="editingContract"
      @contracts="editingContract = null; showContracts = true"
      @close="editingContract = null"
    />

    <GitPanel
      v-if="showGit && workspace"
      :root="workspace.root"
      :unsaved="dirty.size"
      :names="requestNames"
      @open="(id) => { showGit = false; openFromTokens(id); }"
      @reload="organize(async () => {}).then(checkDrift)"
      @close="showGit = false"
    />

    <ExportDialog
      v-if="exporting !== null && workspace"
      :root="workspace.root"
      :folder-id="exporting"
      :environments="environments"
      @close="exporting = null"
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

    <main
      v-if="!(workspace && (selected || editingFolder !== null || runnerFolder !== null || flowView !== null))"
      class="welcome"
    >
      <div class="hello card">
        <img src="/pulpo.svg" width="72" height="72" alt="" />
        <h1>{{ workspace ? workspace.name : $t("Welcome to Pulpo") }}</h1>
        <template v-if="workspace">
          <p v-if="requests.length" class="muted">{{ $t("Pick a request in the sidebar, or press") }} <kbd>Ctrl+K</kbd> {{ $t("to search.") }}</p>
          <p v-else class="muted">{{ $t("This workspace has no requests yet. Use") }} <strong>{{ $t("+ Request") }}</strong> {{ $t("or") }} <strong>{{ $t("Import") }}</strong> {{ $t("in the sidebar.") }}</p>
        </template>
        <template v-else>
          <p class="muted">{{ $t("Your collections live as plain files in a folder of your repo.") }}</p>
          <div class="buttons">
            <button class="primary" @click="openFolder">{{ $t("Open a workspace folder") }}</button>
            <button class="ghost" @click="createWorkspace">{{ $t("Create a workspace in a folder") }}</button>
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
  flex-wrap: wrap;
}
.actions {
  flex-wrap: wrap;
  justify-content: flex-end;
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
.about {
  width: min(420px, 92vw);
}
.about h2 {
  margin: 0 0 8px;
  font-size: 18px;
}
.about p {
  margin: 6px 0;
}
.lang {
  max-width: 120px;
  padding: 3px 6px;
  font-size: 12px;
}
.about-lang {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  margin-top: 10px;
}
.about footer {
  display: flex;
  justify-content: flex-end;
  margin-top: 12px;
}
.about .link,
.link-plain {
  background: none;
  border: none;
  padding: 0;
  font: inherit;
  cursor: pointer;
}
.about .link {
  color: var(--accent);
}
.link-plain {
  color: inherit;
}
.link-plain:hover {
  color: var(--accent);
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
.link-ask {
  position: fixed;
  right: 16px;
  bottom: 16px;
  z-index: 11;
  max-width: min(440px, calc(100vw - 32px));
  display: flex;
  flex-direction: column;
  gap: 10px;
  font-size: 13px;
}
.link-ask p {
  margin: 0;
}
.link-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.flow-pick {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.flow-pick input {
  flex: 1;
  min-width: 0;
}
.flows-chip {
  white-space: nowrap;
  color: var(--accent);
}
.chain-result {
  margin: 0;
  max-height: 220px;
  overflow: auto;
  font: 11px/1.45 var(--mono);
  background: var(--surface-2);
  border-radius: 6px;
  padding: 8px;
  white-space: pre-wrap;
}
</style>
