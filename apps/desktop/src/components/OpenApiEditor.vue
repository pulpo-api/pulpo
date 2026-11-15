<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import CodeEditor from "./CodeEditor.vue";
import { backend, type Drift, type OpenApiCheck, type OpenApiOperation, type Workspace } from "../workspace";

const props = defineProps<{ workspace: Workspace; contract: string }>();
const emit = defineEmits<{ close: []; contracts: [] }>();

const text = ref("");
const saved = ref("");
const loading = ref(true);
const problem = ref("");
const check = ref<OpenApiCheck | null>(null);
const checking = ref(false);
const tab = ref<"problems" | "preview">("problems");
const saving = ref(false);
const savedNote = ref("");
/** Folders linked to this file, read after each save. */
const drifts = ref<Drift[] | null>(null);
const editor = ref<InstanceType<typeof CodeEditor> | null>(null);

const language = computed(() => (props.contract.toLowerCase().endsWith(".json") ? "json" : "yaml"));
const dirty = computed(() => text.value !== saved.value);
const errors = computed(() => check.value?.problems.filter((p) => p.severity === "error").length ?? 0);
const warnings = computed(() => (check.value?.problems.length ?? 0) - errors.value);
const diagnostics = computed(() =>
  (check.value?.problems ?? []).map((p) => ({ line: p.line, severity: p.severity, message: p.message })),
);

/** Operations by their first tag, in document order. */
const groups = computed(() => {
  const out = new Map<string, OpenApiOperation[]>();
  for (const op of check.value?.operations ?? []) {
    const tag = op.tags[0] ?? "Untagged";
    out.set(tag, [...(out.get(tag) ?? []), op]);
  }
  return [...out.entries()];
});

onMounted(async () => {
  try {
    text.value = saved.value = await backend.contractRead!(props.workspace.root, props.contract);
  } catch (e) {
    problem.value = String(e);
  }
  loading.value = false;
});

let timer: ReturnType<typeof setTimeout> | undefined;
let asked = 0;
watch(text, () => {
  clearTimeout(timer);
  timer = setTimeout(runCheck, 300);
});
onBeforeUnmount(() => clearTimeout(timer));

async function runCheck() {
  const ask = ++asked;
  checking.value = true;
  try {
    const result = await backend.openapiCheck!(text.value);
    if (ask === asked) check.value = result;
  } catch (e) {
    problem.value = String(e);
  }
  if (ask === asked) checking.value = false;
}

const sameFile = (a: string, b: string) => {
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/^\.\//, "");
  return norm(a) === norm(b) || norm(a).endsWith(`/${norm(b)}`) || norm(b).endsWith(`/${norm(a)}`);
};

async function save() {
  if (saving.value || !dirty.value) return;
  saving.value = true;
  problem.value = "";
  try {
    const now = text.value;
    await backend.contractSave!(props.workspace.root, props.contract, now);
    saved.value = now;
    const at = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    savedNote.value = errors.value ? `Saved at ${at} with ${errors.value} error(s).` : `Saved at ${at}.`;
    const status = (await backend.syncStatus?.(props.workspace.root)) ?? [];
    drifts.value = status
      .filter((s): s is { Ok: Drift } => "Ok" in s)
      .map((s) => s.Ok)
      .filter((d) => sameFile(d.source, props.contract));
  } catch (e) {
    problem.value = String(e);
  }
  saving.value = false;
}

const folderName = (id: string) => {
  if (!id) return "The whole workspace";
  const find = (nodes: Workspace["tree"], path: string[]): string | null => {
    for (const n of nodes) {
      if (n.type !== "folder") continue;
      const label = [...path, n.name];
      if (n.id === id) return label.join(" / ");
      const inner = find(n.children, label);
      if (inner) return inner;
    }
    return null;
  };
  return find(props.workspace.tree, []) ?? id;
};
const outOfStep = (d: Drift) => d.added.length + d.removed.length + d.changed.length;

function close() {
  if (dirty.value && !window.confirm("Close without saving your changes?")) return;
  emit("close");
}

function onKey(event: KeyboardEvent) {
  if (event.key === "Escape") close();
}
onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));

const go = (line: number | null) => line && editor.value?.goToLine(line);
const methodColor = (m: string) => `var(--m-${m.toLowerCase()}, var(--muted))`;
</script>

<template>
  <div class="backdrop" @click.self="close">
    <section class="card panel" role="dialog" aria-label="OpenAPI editor">
      <header>
        <div class="title">
          <h2>
            {{ check?.title || "OpenAPI" }}
            <span v-if="check?.version" class="muted small">v{{ check.version }}</span>
            <span v-if="check?.openapi" class="pill">OpenAPI {{ check.openapi }}</span>
          </h2>
          <p class="muted small">
            <code>{{ contract }}</code><template v-if="dirty"> · unsaved</template>
            <template v-else-if="savedNote"> · {{ savedNote }}</template>
          </p>
        </div>
        <div class="actions">
          <span v-if="check" class="status" :class="{ bad: errors, good: !errors }" role="status">
            {{ errors ? `${errors} error${errors === 1 ? "" : "s"}` : "Valid" }}<template v-if="warnings"> · {{ warnings }} warning{{ warnings === 1 ? "" : "s" }}</template>
          </span>
          <button class="primary" :disabled="!dirty || saving" title="Ctrl+S" @click="save">{{ saving ? "Saving…" : "Save" }}</button>
          <button class="ghost" title="Close" @click="close">✕</button>
        </div>
      </header>

      <p v-if="problem" class="warn">{{ problem }}</p>
      <div v-if="drifts" class="drift-note" role="status">
        <template v-if="!drifts.length">
          No folder is linked to this file yet: import it or link a folder in
          <button class="link" @click="emit('contracts')">API contracts</button>.
        </template>
        <template v-for="d in drifts" v-else :key="d.folder">
          <span v-if="outOfStep(d)">
            <b>{{ folderName(d.folder) }}</b>: {{ d.added.length }} new · {{ d.removed.length }} gone ·
            {{ d.changed.length }} to complete —
            <button class="link" @click="emit('contracts')">review in API contracts</button>
          </span>
          <span v-else><b>{{ folderName(d.folder) }}</b> is in step with this file.</span>
        </template>
      </div>

      <p v-if="loading" class="muted">Reading {{ contract }}…</p>
      <div v-else class="body">
        <div class="code">
          <CodeEditor
            ref="editor"
            v-model="text"
            :language="language"
            :diagnostics="diagnostics"
            @save="save"
          />
        </div>
        <aside>
          <nav class="tabs" role="tablist">
            <button role="tab" :aria-selected="tab === 'problems'" :class="{ on: tab === 'problems' }" @click="tab = 'problems'">
              Problems <span class="count">{{ check?.problems.length ?? 0 }}</span>
            </button>
            <button role="tab" :aria-selected="tab === 'preview'" :class="{ on: tab === 'preview' }" @click="tab = 'preview'">
              Preview <span class="count">{{ check?.operations.length ?? 0 }}</span>
            </button>
            <span v-if="checking" class="muted small">checking…</span>
          </nav>

          <div v-if="tab === 'problems'" class="scroll">
            <p v-if="check && !check.problems.length" class="ok">
              No problems: the document follows the OpenAPI {{ check.openapi }} schema, every path parameter is declared
              and every <code>$ref</code> points somewhere.
            </p>
            <button
              v-for="(p, i) in check?.problems ?? []"
              :key="i"
              class="problem"
              :class="p.severity"
              :title="p.pointer"
              @click="go(p.line)"
            >
              <span class="where">{{ p.line ? `line ${p.line}` : "—" }}</span>
              <span>{{ p.message }}</span>
            </button>
          </div>

          <div v-else class="scroll">
            <p v-if="check?.servers.length" class="muted small">
              Servers: <code v-for="s in check.servers" :key="s" class="server">{{ s }}</code>
            </p>
            <p v-if="check && !check.operations.length" class="muted">No operations yet: add them under <code>paths</code>.</p>
            <section v-for="[tag, ops] in groups" :key="tag" class="group">
              <h3>{{ tag }}</h3>
              <details v-for="op in ops" :key="`${op.method} ${op.path}`" class="op">
                <summary>
                  <code class="method" :style="{ color: methodColor(op.method) }">{{ op.method }}</code>
                  <code class="path" :class="{ deprecated: op.deprecated }">{{ op.path }}</code>
                  <span class="muted small">{{ op.summary }}</span>
                  <button class="link small jump" title="Go to it in the editor" @click.prevent="go(op.line)">↗</button>
                </summary>
                <dl>
                  <template v-if="op.operationId">
                    <dt>operationId</dt>
                    <dd><code>{{ op.operationId }}</code></dd>
                  </template>
                  <template v-if="op.parameters.length">
                    <dt>Parameters</dt>
                    <dd>
                      <span v-for="p in op.parameters" :key="`${p.location}${p.name}`" class="param">
                        <code>{{ p.name }}</code><span class="muted small"> {{ p.location }}{{ p.required ? " · required" : "" }}</span>
                      </span>
                    </dd>
                  </template>
                  <template v-if="op.body.length">
                    <dt>Body</dt>
                    <dd>{{ op.body.join(", ") }}<span class="muted small">{{ op.bodyRequired ? " · required" : "" }}</span></dd>
                  </template>
                  <dt>Responses</dt>
                  <dd>
                    <div v-for="r in op.responses" :key="r.status">
                      <code>{{ r.status }}</code> <span class="muted small">{{ r.description }}</span>
                    </div>
                    <span v-if="!op.responses.length" class="warn">none</span>
                  </dd>
                </dl>
              </details>
            </section>
          </div>
        </aside>
      </div>
    </section>
  </div>
</template>

<style scoped>
.backdrop {
  position: fixed;
  inset: 0;
  background: rgba(10, 12, 20, 0.45);
  display: grid;
  place-items: center;
  z-index: 10;
}
.panel {
  width: min(1320px, 96vw);
  height: 90vh;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}
h2 {
  margin: 0;
  font-size: 17px;
  display: flex;
  align-items: baseline;
  gap: 8px;
  flex-wrap: wrap;
}
h3 {
  margin: 6px 0 2px;
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--muted);
}
.title p {
  margin: 2px 0 0;
}
.pill {
  font-size: 11px;
  font-weight: 500;
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 1px 8px;
  color: var(--muted);
}
.actions {
  display: flex;
  align-items: center;
  gap: 8px;
}
.status {
  font-size: 12px;
  font-weight: 600;
}
.status.good {
  color: var(--ok);
}
.status.bad {
  color: var(--m-delete);
}
.muted {
  color: var(--muted);
}
.small {
  font-size: 12px;
}
.warn {
  color: var(--m-post);
  margin: 0;
  font-size: 13px;
}
.ok {
  color: var(--ok);
  font-size: 13px;
  margin: 4px 0;
}
.drift-note {
  font-size: 13px;
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 6px 10px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.body {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: minmax(0, 1.4fr) minmax(280px, 1fr);
  gap: 10px;
}
.code {
  min-height: 0;
  display: flex;
}
aside {
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  border-bottom: 1px solid var(--border);
}
.tabs button {
  background: none;
  border: none;
  border-bottom: 2px solid transparent;
  padding: 4px 10px 6px;
  color: var(--muted);
  cursor: pointer;
  font: inherit;
  font-size: 13px;
}
.tabs button.on {
  color: var(--text);
  border-bottom-color: var(--accent);
}
.count {
  font-size: 11px;
  color: var(--muted);
}
.scroll {
  overflow: auto;
  min-height: 0;
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.problem {
  display: grid;
  grid-template-columns: 64px 1fr;
  gap: 8px;
  text-align: left;
  background: none;
  border: none;
  border-left: 3px solid var(--m-delete);
  padding: 4px 8px;
  font: inherit;
  font-size: 12.5px;
  color: var(--text);
  cursor: pointer;
  border-radius: 2px;
}
.problem.warning {
  border-left-color: var(--m-post);
}
.problem:hover {
  background: var(--surface-2);
}
.where {
  color: var(--muted);
  font-family: var(--mono);
  font-size: 11.5px;
}
.server {
  margin-right: 6px;
}
.op {
  border-bottom: 1px solid var(--border);
  padding: 3px 0;
}
.op summary {
  display: flex;
  align-items: baseline;
  gap: 8px;
  cursor: pointer;
  font-size: 13px;
}
.method {
  width: 54px;
  font-size: 11px;
  font-weight: 700;
  flex: none;
}
.path {
  font-size: 12.5px;
  word-break: break-all;
}
.path.deprecated {
  text-decoration: line-through;
}
.jump {
  margin-left: auto;
}
dl {
  display: grid;
  grid-template-columns: 90px 1fr;
  gap: 4px 8px;
  margin: 6px 0 6px 62px;
  font-size: 12.5px;
}
dt {
  color: var(--muted);
}
dd {
  margin: 0;
}
.param {
  display: inline-block;
  margin-right: 10px;
}
.link {
  background: none;
  border: none;
  padding: 0;
  color: var(--accent);
  cursor: pointer;
  font: inherit;
}
@media (max-width: 760px) {
  .body {
    grid-template-columns: 1fr;
    grid-template-rows: 1fr 1fr;
  }
}
</style>
