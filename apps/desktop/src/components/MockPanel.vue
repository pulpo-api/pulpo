<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { backend, type MockHit, type MockRoute, type TreeNode, type Workspace } from "../workspace";

const props = defineProps<{ workspace: Workspace }>();
const emit = defineEmits<{ close: []; open: [requestId: string] }>();

const folder = ref("");
const port = ref(4010);
const routes = ref<MockRoute[]>([]);
const hits = ref<MockHit[]>([]);
const url = ref("");
const problem = ref("");
const busy = ref(false);
const copied = ref(false);
let unlisten: (() => void) | null = null;

const folders = computed(() => {
  const out: { id: string; label: string }[] = [{ id: "", label: props.workspace.name }];
  const walk = (nodes: TreeNode[], depth: number) => {
    for (const n of nodes) {
      if (n.type === "folder") {
        out.push({ id: n.id, label: `${"  ".repeat(depth)}${n.name}` });
        walk(n.children, depth + 1);
      }
    }
  };
  walk(props.workspace.tree, 1);
  return out;
});
const fromOpenapi = computed(() => routes.value.filter((r) => r.source === "openapi").length);

async function loadRoutes() {
  if (!backend.mockRoutes) return;
  problem.value = "";
  try {
    routes.value = await backend.mockRoutes(props.workspace.root, folder.value);
  } catch (e) {
    problem.value = String(e);
  }
}

async function start() {
  if (!backend.mockStart) return;
  busy.value = true;
  problem.value = "";
  try {
    const state = await backend.mockStart(props.workspace.root, folder.value, port.value);
    url.value = state.url;
    hits.value = [];
  } catch (e) {
    problem.value = String(e);
  }
  busy.value = false;
}

async function stop() {
  await backend.mockStop?.();
  url.value = "";
}

async function copy() {
  await navigator.clipboard.writeText(url.value);
  copied.value = true;
  setTimeout(() => (copied.value = false), 1200);
}

onMounted(async () => {
  const running = await backend.mockStatus?.();
  if (running) {
    url.value = running.url;
    folder.value = running.folder;
  }
  await loadRoutes();
  unlisten = (await backend.onMockHit?.((hit) => {
    hits.value = [hit, ...hits.value].slice(0, 200);
  })) ?? null;
});
onUnmounted(() => unlisten?.());
watch(folder, loadRoutes);
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card mock" role="dialog" :aria-label="$t('Mock server')">
      <header>
        <div>
          <h2>{{ $t("Mock server") }}</h2>
          <p class="muted small">
            {{ $t("Your saved examples answered as a fake API, so a front end works without its back end (and in CI with") }}
            <code>pulpo mock</code>).
          </p>
        </div>
        <button class="ghost" :title="$t('Close')" @click="emit('close')">✕</button>
      </header>

      <p v-if="!backend.mockStart" class="note">{{ $t("The mock runs in the desktop app, or with") }} <code>pulpo mock</code>.</p>
      <p v-if="problem" class="warn">{{ problem }}</p>

      <div class="row">
        <label>
          {{ $t("Serve") }}
          <select v-model="folder" :disabled="!!url">
            <option v-for="f in folders" :key="f.id" :value="f.id">{{ f.label }}</option>
          </select>
        </label>
        <label>{{ $t("Port") }} <input v-model.number="port" type="number" min="0" max="65535" :disabled="!!url" /></label>
        <button v-if="!url" class="primary" :disabled="busy || !routes.length || !backend.mockStart" @click="start">
          {{ busy ? $t("Starting…") : $t("Start") }}
        </button>
        <template v-else>
          <code class="url">{{ url }}</code>
          <button class="ghost" @click="copy">{{ copied ? $t("Copied") : $t("Copy") }}</button>
          <button class="ghost danger" @click="stop">{{ $t("Stop") }}</button>
        </template>
      </div>

      <h3>
        {{ $t("{0} routes", { 0: routes.length }) }}<template v-if="fromOpenapi"> {{ $t("· {0} from OpenAPI", { 0: fromOpenapi }) }}</template>
      </h3>
      <p v-if="!routes.length" class="muted small">
        {{ $t("Nothing to serve here yet: save a response as an example (") }}<b>{{ $t("Save as example") }}</b> {{ $t("in the response, or the Examples tab), or link an OpenAPI file with ⟲.") }}
      </p>
      <ul class="routes">
        <li v-for="r in routes" :key="`${r.method} ${r.path}`">
          <span class="method" :class="`m-${r.method.toLowerCase()}`">{{ r.method }}</span>
          <code>{{ r.path }}</code>
          <span class="muted">{{ r.name }}</span>
          <span class="answers">
            <span v-for="a in r.answers" :key="a.name" class="chip" :class="{ bad: a.status >= 400 }">{{ a.status }} {{ a.name }}</span>
          </span>
        </li>
      </ul>
      <p class="hint">
        {{ $t("Another answer: header") }} <code>Prefer: code=404</code> {{ $t("or") }} <code>Prefer: example=&lt;name&gt;</code>{{ $t(", or") }}
        <code>?__example=&lt;name&gt;</code>{{ $t(". CORS is open. Edits apply without a restart.") }}
      </p>

      <template v-if="url">
        <h3>{{ $t("Calls") }}</h3>
        <ol class="hits">
          <li v-if="!hits.length" class="muted small">{{ $t("Waiting for calls to {0}…", { 0: url }) }}</li>
          <li v-for="(h, i) in hits" :key="i" :class="{ bad: h.status >= 400 }">
            <span class="time">{{ (h.atMs / 1000).toFixed(1) }}s</span>
            <span class="status">{{ h.status }}</span>
            <span class="method">{{ h.method }}</span>
            <code>{{ h.path }}</code>
            <span class="muted">{{ h.matched || "no mock" }}</span>
          </li>
        </ol>
      </template>
    </section>
  </div>
</template>

<style scoped>
.backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
  display: flex;
  align-items: flex-start;
  justify-content: center;
  padding: 40px 16px;
  z-index: 50;
  overflow: auto;
}
.mock {
  width: min(860px, 100%);
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 18px 20px;
}
header {
  display: flex;
  justify-content: space-between;
  gap: 12px;
}
h2 {
  margin: 0 0 4px;
  font-size: 18px;
}
h3 {
  margin: 6px 0 0;
  font-size: 13px;
}
.row {
  display: flex;
  gap: 10px;
  align-items: end;
  flex-wrap: wrap;
}
.row label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--muted);
}
.row input {
  width: 90px;
}
.url {
  padding: 6px 10px;
  background: var(--surface-2);
  border-radius: 6px;
}
.routes,
.hits {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
}
.routes li,
.hits li {
  display: flex;
  gap: 8px;
  align-items: baseline;
  flex-wrap: wrap;
}
.hits {
  max-height: 260px;
  overflow: auto;
  font-family: var(--mono);
}
.hits .bad .status {
  color: var(--m-delete);
}
.method {
  font: 700 11px var(--mono);
  min-width: 52px;
}
.m-get {
  color: var(--m-get);
}
.m-post {
  color: var(--m-post);
}
.m-put,
.m-patch {
  color: var(--m-put);
}
.m-delete {
  color: var(--m-delete);
}
.answers {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
}
.chip {
  padding: 0 6px;
  border-radius: 4px;
  background: var(--surface-2);
  color: var(--m-get);
}
.chip.bad {
  color: var(--m-delete);
}
.time,
.muted {
  color: var(--muted);
}
.small {
  font-size: 12px;
}
.note {
  font-size: 12px;
  color: var(--m-put);
  margin: 0;
}
.warn {
  font-size: 12px;
  color: var(--m-delete);
  margin: 0;
}
.hint {
  font-size: 12px;
  color: var(--muted);
  margin: 0;
}
.primary,
.ghost {
  border-radius: 8px;
  padding: 6px 14px;
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
.danger {
  color: var(--m-delete);
}
button:disabled {
  opacity: 0.55;
  cursor: default;
}
</style>
