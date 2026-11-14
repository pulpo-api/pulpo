<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import CodeEditor from "./CodeEditor.vue";
import { backend, type FlowPlanNode, type FlowStepEvent, type Flow } from "../workspace";

const props = defineProps<{ root: string; flowId: string; environmentId: string; environmentName: string }>();
const emit = defineEmits<{ close: []; open: [requestId: string]; changed: []; deleted: [] }>();

const flow = ref<Flow | null>(null);
const nodes = ref<FlowPlanNode[]>([]);
const view = ref<"diagram" | "yaml">("diagram");
const yaml = ref("");
const savedYaml = ref("");
const problem = ref("");
const note = ref("");
const running = ref(false);
const states = ref<Record<string, FlowStepEvent>>({});
const selectedPath = ref("");
const confirmDelete = ref(false);
let unlisten: (() => void) | null = null;

async function load() {
  try {
    const flows = await backend.listFlows!(props.root);
    flow.value = flows.find((f) => f.id === props.flowId) ?? null;
    nodes.value = await backend.flowPlan!(props.root, props.flowId);
    yaml.value = savedYaml.value = await backend.flowYaml!(props.root, props.flowId);
    problem.value = "";
  } catch (e) {
    problem.value = String(e);
  }
}
onMounted(async () => {
  await load();
  unlisten = (await backend.onFlowStep?.((event) => {
    states.value = { ...states.value, [event.path]: event };
    if (event.state !== "running" && event.state !== "skipped" && !selectedPath.value.startsWith("pinned:")) {
      selectedPath.value = event.path;
    }
  })) ?? null;
});
onUnmounted(() => unlisten?.());
watch(() => props.flowId, load);

const broken = computed(() => nodes.value.filter((n) => n.problem));
const requestCount = computed(() => nodes.value.filter((n) => n.kind === "request").length);
const done = computed(() => Object.values(states.value).filter((s) => s.state === "passed" || s.state === "failed"));
const failed = computed(() => done.value.some((s) => s.state === "failed"));
const totalMs = computed(() => done.value.reduce((sum, s) => sum + (s.report?.response?.durationMs ?? 0), 0));
const selected = computed(() => {
  const path = selectedPath.value.replace("pinned:", "");
  return { node: nodes.value.find((n) => n.path === path) ?? null, event: states.value[path] ?? null };
});

async function run() {
  if (!backend.runFlow) return;
  running.value = true;
  problem.value = "";
  note.value = "";
  states.value = {};
  selectedPath.value = "";
  try {
    const report = await backend.runFlow(props.root, props.flowId, props.environmentId);
    note.value = report.passed ? "Flow passed." : "Flow failed: see the red step.";
  } catch (e) {
    problem.value = String(e);
  }
  running.value = false;
}

async function saveYaml() {
  try {
    flow.value = await backend.saveFlowYaml!(props.root, props.flowId, yaml.value);
    await load();
    note.value = "Saved.";
    emit("changed");
  } catch (e) {
    problem.value = String(e);
  }
}

async function remove() {
  if (!confirmDelete.value) {
    confirmDelete.value = true;
    return;
  }
  await backend.deleteFlow!(props.root, props.flowId);
  emit("deleted");
}

function stateOf(node: FlowPlanNode): string {
  if (node.kind === "missing") return "broken";
  if (node.kind === "flow") return "group";
  return states.value[node.path]?.state ?? "pending";
}

function statusText(node: FlowPlanNode): string {
  const event = states.value[node.path];
  const r = event?.report;
  if (!event) return "";
  if (event.state === "running") return "running…";
  if (event.state === "skipped") return "skipped";
  if (r?.response) return `${r.response.status} · ${r.response.durationMs} ms`;
  if (r?.error) return r.error.message ?? "error";
  return event.state;
}

function failureOf(event: FlowStepEvent | null): string {
  const r = event?.report;
  if (!r) return "";
  const test = r.tests.find((t) => !t.passed);
  if (test) return `${test.name}: ${test.error ?? "failed"}`;
  if (r.scriptErrors.length) return r.scriptErrors[0];
  return r.error?.message ?? "";
}

function prettyBody(body: string): string {
  try {
    return JSON.stringify(JSON.parse(body), null, 2).slice(0, 6000);
  } catch {
    return body.slice(0, 6000);
  }
}
</script>

<template>
  <section class="flow">
    <header>
      <div class="title">
        <span class="kicker">Flow</span>
        <h2>{{ flow?.name ?? flowId }}</h2>
        <p v-if="flow?.description" class="muted">{{ flow.description }}</p>
        <p class="muted small">
          {{ requestCount }} requests · <code>flows/{{ flowId }}.flow.yaml</code>
        </p>
      </div>
      <div class="actions">
        <div class="switch" role="tablist">
          <button :class="{ on: view === 'diagram' }" role="tab" @click="view = 'diagram'">Diagram</button>
          <button :class="{ on: view === 'yaml' }" role="tab" @click="view = 'yaml'">YAML</button>
        </div>
        <button
          class="primary"
          :disabled="running || !!broken.length || !requestCount"
          :title="`Run with ${environmentName || 'no environment'}`"
          @click="run"
        >
          {{ running ? "Running…" : "▶ Run" }}
        </button>
        <button class="ghost danger" :class="{ armed: confirmDelete }" @click="remove">
          {{ confirmDelete ? "Click again to delete" : "Delete" }}
        </button>
        <button class="ghost" title="Close" @click="emit('close')">✕</button>
      </div>
    </header>

    <p v-if="problem" class="problem">{{ problem }}</p>
    <p v-for="b in broken" :key="b.path" class="problem">Step {{ b.path }}: {{ b.problem }}</p>
    <p v-if="done.length" class="summary" :class="{ bad: failed }">
      {{ done.filter((s) => s.state === "passed").length }}/{{ requestCount }} passed · {{ totalMs }} ms
      <template v-if="environmentName"> · {{ environmentName }}</template>
      <span v-if="note"> · {{ note }}</span>
    </p>
    <p v-else-if="note" class="summary">{{ note }}</p>

    <div v-if="view === 'diagram'" class="body">
      <ol class="tree" aria-label="Steps">
        <li v-if="!nodes.length" class="empty">
          No steps yet. Right-click a request in the tree → <b>Add to flow…</b>, or write them in the YAML view.
        </li>
        <li
          v-for="node in nodes"
          :key="node.path"
          class="node"
          :class="[stateOf(node), { chosen: selected.node?.path === node.path }]"
          :style="{ marginLeft: `${node.depth * 28}px` }"
        >
          <button class="card" @click="selectedPath = node.kind === 'request' ? `pinned:${node.path}` : selectedPath">
            <span class="path">{{ node.path }}</span>
            <template v-if="node.kind === 'flow'">
              <span class="sub">↳ flow</span>
              <span class="name">{{ node.name }}</span>
            </template>
            <template v-else>
              <span class="method" :class="node.method.toLowerCase()">{{ node.method || "?" }}</span>
              <span class="name">{{ node.name }}</span>
              <span class="url">{{ node.url }}</span>
              <span v-for="v in node.save" :key="v" class="chip">→ {{ v }}</span>
              <span v-if="node.expect" class="chip">expects {{ node.expect }}</span>
              <span class="status">{{ statusText(node) }}</span>
            </template>
          </button>
        </li>
      </ol>

      <aside v-if="selected.node" class="detail">
        <h3>{{ selected.node.path }} · {{ selected.node.name }}</h3>
        <p class="muted small">
          <code>{{ selected.node.target }}</code>
          <button class="link" @click="emit('open', selected.node.target)">Open request</button>
        </p>
        <template v-if="selected.event?.report">
          <p v-if="failureOf(selected.event)" class="problem">{{ failureOf(selected.event) }}</p>
          <p v-if="selected.event.report.response" class="muted">
            <b>{{ selected.event.report.response.status }} {{ selected.event.report.response.statusText }}</b>
            · {{ selected.event.report.response.durationMs }} ms
          </p>
          <div v-if="Object.keys(selected.event.saved ?? {}).length" class="saved">
            <div v-for="(value, key) in selected.event.saved" :key="key">
              <code>{{ key }}</code> = <code>{{ String(value).slice(0, 80) }}</code>
            </div>
          </div>
          <ul v-if="selected.event.report.tests.length" class="tests">
            <li v-for="(t, i) in selected.event.report.tests" :key="i" :class="{ bad: !t.passed }">
              {{ t.passed ? "✓" : "✗" }} {{ t.name }}<template v-if="t.error">: {{ t.error }}</template>
            </li>
          </ul>
          <pre v-if="selected.event.report.response?.bodyIsText">{{
            prettyBody(selected.event.report.response.body)
          }}</pre>
        </template>
        <p v-else class="muted small">Run the flow to see this step's response.</p>
      </aside>
    </div>

    <div v-else class="yaml">
      <CodeEditor v-model="yaml" language="text" @save="saveYaml" />
      <footer>
        <span class="muted small">
          Steps: <code>- request: &lt;id&gt;</code> (with optional <code>save:</code> and <code>expect: {status}</code>) or
          <code>- flow: &lt;id&gt;</code>. Ctrl+S saves.
        </span>
        <button class="ghost" :disabled="yaml === savedYaml" @click="yaml = savedYaml">Discard</button>
        <button class="primary" :disabled="yaml === savedYaml" @click="saveYaml">Save</button>
      </footer>
    </div>
  </section>
</template>

<style scoped>
.flow {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 16px 20px;
  min-height: 0;
  height: 100%;
  container-type: inline-size;
}
header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
  flex-wrap: wrap;
}
.kicker {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--muted);
}
h2 {
  margin: 2px 0 4px;
  font-size: 19px;
}
h3 {
  margin: 0 0 4px;
  font-size: 14px;
}
.muted {
  color: var(--muted);
  margin: 0;
}
.small {
  font-size: 12px;
}
.actions {
  display: flex;
  gap: 8px;
  align-items: center;
}
.switch {
  display: flex;
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
}
.switch button {
  border: 0;
  background: transparent;
  padding: 6px 12px;
  cursor: pointer;
  color: var(--muted);
}
.switch button.on {
  background: var(--surface-2);
  color: inherit;
}
.problem {
  margin: 0;
  color: var(--m-delete);
  font-size: 13px;
}
.summary {
  margin: 0;
  font-size: 13px;
  color: var(--m-get);
}
.summary.bad {
  color: var(--m-delete);
}
.body {
  display: grid;
  grid-template-columns: minmax(0, 1.2fr) minmax(0, 1fr);
  gap: 16px;
  min-height: 0;
  flex: 1;
  overflow: auto;
}
@container (max-width: 760px) {
  .body {
    grid-template-columns: minmax(0, 1fr);
  }
}
.tree {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0;
}
.node {
  position: relative;
  padding: 0 0 14px 18px;
}
.node::before {
  content: "";
  position: absolute;
  left: 6px;
  top: 0;
  bottom: 0;
  width: 2px;
  background: var(--border);
}
.node:last-child::before {
  bottom: 50%;
}
.node::after {
  content: "";
  position: absolute;
  left: 2px;
  top: 16px;
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: var(--surface-2);
  border: 2px solid var(--border);
}
.node.passed::after {
  background: var(--m-get);
  border-color: var(--m-get);
}
.node.failed::after,
.node.broken::after {
  background: var(--m-delete);
  border-color: var(--m-delete);
}
.node.running::after {
  border-color: var(--m-put);
}
.node.group::after {
  border-color: var(--accent);
}
.card {
  width: 100%;
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 4px 8px;
  text-align: left;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface);
  padding: 8px 10px;
  cursor: pointer;
  color: inherit;
  font: inherit;
}
.node.group .card {
  border-style: dashed;
  background: transparent;
}
.node.chosen .card {
  border-color: var(--accent);
}
.node.failed .card,
.node.broken .card {
  border-color: var(--m-delete);
}
.node.skipped .card {
  opacity: 0.55;
}
.path {
  font-family: var(--mono);
  font-size: 11px;
  color: var(--muted);
}
.sub {
  font-size: 12px;
  color: var(--accent);
}
.method {
  font-family: var(--mono);
  font-size: 11px;
  font-weight: 700;
}
.method.get {
  color: var(--m-get);
}
.method.post {
  color: var(--m-post);
}
.method.put,
.method.patch {
  color: var(--m-put);
}
.method.delete {
  color: var(--m-delete);
}
.name {
  font-weight: 600;
}
.url {
  flex-basis: 100%;
  font-family: var(--mono);
  font-size: 11px;
  color: var(--muted);
  overflow-wrap: anywhere;
}
.chip {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
  background: var(--surface-2);
  color: var(--muted);
}
.status {
  margin-left: auto;
  font-size: 12px;
  color: var(--muted);
}
.node.passed .status {
  color: var(--m-get);
}
.node.failed .status {
  color: var(--m-delete);
}
.detail {
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 10px 12px;
  align-self: start;
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}
.detail pre {
  margin: 0;
  max-height: 340px;
  overflow: auto;
  background: var(--surface-2);
  border-radius: 6px;
  padding: 8px;
  font: 12px/1.45 var(--mono);
}
.saved {
  font-size: 12px;
}
.tests {
  margin: 0;
  padding-left: 16px;
  font-size: 12px;
  color: var(--m-get);
}
.tests .bad {
  color: var(--m-delete);
}
.link {
  background: none;
  border: none;
  color: var(--accent);
  cursor: pointer;
  font-size: 12px;
  margin-left: 8px;
}
.empty {
  color: var(--muted);
  font-size: 13px;
}
.yaml {
  display: flex;
  flex-direction: column;
  gap: 8px;
  flex: 1;
  min-height: 0;
}
.yaml footer {
  display: flex;
  gap: 8px;
  align-items: center;
  justify-content: flex-end;
}
.yaml footer .muted {
  margin-right: auto;
}
.primary,
.ghost {
  border-radius: 8px;
  padding: 7px 14px;
  cursor: pointer;
  font-weight: 600;
}
.primary {
  border: 0;
  background: var(--accent);
  color: #fff;
}
.ghost {
  border: 1px solid var(--border);
  background: var(--surface);
  color: inherit;
}
.armed {
  border-color: var(--m-delete);
}
button:disabled {
  opacity: 0.55;
  cursor: default;
}
</style>
