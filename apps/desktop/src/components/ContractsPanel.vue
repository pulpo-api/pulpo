<script setup lang="ts">
import { t } from "../i18n";
import { computed, onMounted, ref } from "vue";
import { backend, type Contract, type Drift, type TreeNode, type Workspace } from "../workspace";

const props = defineProps<{ workspace: Workspace }>();
const emit = defineEmits<{ close: []; changed: []; open: [id: string]; edit: [contract: string] }>();

const contracts = ref<Contract[]>([]);
const drifts = ref<Drift[]>([]);
const errors = ref<string[]>([]);
const loading = ref(true);
const busy = ref("");
const note = ref("");
const problem = ref("");
/** Per folder: what is ticked (operation keys and request ids). */
const picked = ref<Record<string, Set<string>>>({});
/** Contract waiting for a folder to link to. */
const linking = ref<string | null>(null);
const linkFolder = ref("");

async function load() {
  loading.value = true;
  problem.value = "";
  try {
    const [found, status] = await Promise.all([
      backend.scanContracts!(props.workspace.root),
      backend.syncStatus!(props.workspace.root),
    ]);
    contracts.value = found;
    drifts.value = status.filter((s): s is { Ok: Drift } => "Ok" in s).map((s) => s.Ok);
    errors.value = status.filter((s): s is { Err: string } => "Err" in s).map((s) => s.Err);
    const next: Record<string, Set<string>> = {};
    for (const d of drifts.value) {
      next[d.folder] = new Set([...d.added.map((i) => i.key), ...d.changed.map((i) => i.target)]);
    }
    picked.value = next;
  } catch (e) {
    problem.value = String(e);
  }
  loading.value = false;
}
onMounted(load);

const folders = computed(() => {
  const out: { id: string; label: string }[] = [{ id: "", label: t("The whole workspace") }];
  const walk = (nodes: TreeNode[], path: string[]) => {
    for (const n of nodes) {
      if (n.type === "folder") {
        const label = [...path, n.name];
        out.push({ id: n.id, label: label.join(" / ") });
        walk(n.children, label);
      }
    }
  };
  walk(props.workspace.tree, []);
  return out;
});
const folderName = (id: string) => folders.value.find((f) => f.id === id)?.label ?? id;
const unsupported = computed(() => contracts.value.filter((c) => !c.supported));
const openapi = computed(() => contracts.value.filter((c) => c.supported));
const outOfStep = computed(() => drifts.value.filter((d) => d.added.length || d.removed.length || d.changed.length));

function toggle(folder: string, key: string) {
  const set = new Set(picked.value[folder]);
  if (set.has(key)) set.delete(key);
  else set.add(key);
  picked.value = { ...picked.value, [folder]: set };
}

async function apply(d: Drift) {
  const set = picked.value[d.folder] ?? new Set();
  busy.value = d.folder;
  note.value = "";
  try {
    const done = await backend.syncApply!(props.workspace.root, d.folder, {
      add: d.added.filter((i) => set.has(i.key)).map((i) => i.key),
      update: d.changed.filter((i) => set.has(i.target)).map((i) => i.target),
      remove: d.removed.filter((i) => set.has(i.target)).map((i) => i.target),
    });
    note.value = `${folderName(d.folder)}: ${done.added.length} added, ${done.updated.length} completed, ${done.removed.length} removed.`;
    emit("changed");
    await load();
  } catch (e) {
    problem.value = String(e);
  }
  busy.value = "";
}

async function importContract(c: Contract) {
  busy.value = c.path;
  try {
    const summary = await backend.syncImport!(props.workspace.root, c.path);
    note.value = t("Imported {0}: {1} requests into a folder linked to {2}.", { 0: c.title, 1: summary.requests, 2: c.path });
    emit("changed");
    await load();
  } catch (e) {
    problem.value = String(e);
  }
  busy.value = "";
}

async function link(c: Contract) {
  busy.value = c.path;
  try {
    await backend.syncLink!(props.workspace.root, linkFolder.value, c.path);
    linking.value = null;
    emit("changed");
    await load();
  } catch (e) {
    problem.value = String(e);
  }
  busy.value = "";
}
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card panel" role="dialog" :aria-label="$t('API contracts')">
      <header>
        <div>
          <h2>{{ $t("API contracts") }}</h2>
          <p class="sub">
            {{ $t("The collection follows the contracts of this repository: new operations, gone ones and requests that miss something the contract asks for. Nothing you wrote in a request is changed.") }}
          </p>
        </div>
        <button class="ghost" :title="$t('Close')" @click="emit('close')">✕</button>
      </header>

      <p v-if="!backend.scanContracts" class="note">{{ $t("This runs in the desktop app, or with") }} <code>pulpo sync</code>.</p>
      <p v-if="problem" class="warn">{{ problem }}</p>
      <p v-for="(e, i) in errors" :key="i" class="warn">{{ e }}</p>
      <p v-if="note" class="ok" role="status">{{ note }}</p>
      <p v-if="loading" class="note">{{ $t("Reading the contracts…") }}</p>

      <div v-else class="list">
        <section v-for="d in outOfStep" :key="d.folder" class="drift">
          <h3>
            {{ folderName(d.folder) }}
            <span class="muted">← {{ d.source }}</span>
          </h3>
          <p class="muted small">
            {{ $t("{0} new · {1} gone · {2} to complete · {3} in step", { 0: d.added.length, 1: d.removed.length, 2: d.changed.length, 3: d.unchanged }) }}<template v-if="d.repeated"> {{ $t("(+{0} repeated calls)", { 0: d.repeated }) }}</template>
          </p>
          <label v-for="i in d.added" :key="`a${i.key}`" class="row">
            <input type="checkbox" :checked="picked[d.folder]?.has(i.key)" @change="toggle(d.folder, i.key)" />
            <span class="tag add">{{ $t("new") }}</span>
            <code class="method">{{ i.method }}</code>
            <span>{{ i.name }}</span>
            <span class="muted small">{{ i.key }}</span>
          </label>
          <label v-for="i in d.changed" :key="`c${i.target}`" class="row">
            <input type="checkbox" :checked="picked[d.folder]?.has(i.target)" @change="toggle(d.folder, i.target)" />
            <span class="tag mod">{{ $t("complete") }}</span>
            <code class="method">{{ i.method }}</code>
            <button class="link" @click.prevent="emit('open', i.target)">{{ i.name }}</button>
            <span class="muted small">{{ $t("adds {0}", { 0: i.changes.join(", ") }) }}</span>
          </label>
          <label v-for="i in d.removed" :key="`r${i.target}`" class="row">
            <input type="checkbox" :checked="picked[d.folder]?.has(i.target)" @change="toggle(d.folder, i.target)" />
            <span class="tag del">{{ $t("gone") }}</span>
            <code class="method">{{ i.method }}</code>
            <button class="link" @click.prevent="emit('open', i.target)">{{ i.name }}</button>
            <span class="muted small">{{ i.changes[0] ?? "not in the contract any more" }}</span>
          </label>
          <footer>
            <span class="muted small">{{ $t("Ticked “gone” requests are deleted.") }}</span>
            <button class="primary" :disabled="busy === d.folder || !picked[d.folder]?.size" @click="apply(d)">
              {{ busy === d.folder ? $t("Applying…") : $t("Apply selected") }}
            </button>
          </footer>
        </section>
        <p v-if="!outOfStep.length && drifts.length" class="ok">{{ $t("Every linked folder is in step with its contract.") }}</p>

        <h3 class="section">{{ $t("Contracts in this repository") }}</h3>
        <p v-if="!contracts.length" class="note">
          {{ $t("No OpenAPI, .proto, WSDL or GraphQL schema files, nor Spring, NestJS, Express, FastAPI, Next.js or Nuxt routes, found in the repository of this workspace.") }}
        </p>
        <div v-for="c in openapi" :key="c.path" class="contract">
          <div>
            <b>{{ c.title }}</b> <span class="muted small">{{ c.operations }} {{ c.note ? "endpoints" : "operations" }}</span>
            <div class="muted small"><code>{{ c.path }}</code><template v-if="c.note"> {{ $t("· {0} code", { 0: c.kind }) }}</template></div>
            <div v-if="c.folder !== null && c.folder !== undefined" class="small">→ {{ folderName(c.folder) }}</div>
          </div>
          <div class="actions">
            <button v-if="c.kind === 'openapi'" class="ghost" :title="$t('Edit with checks against the OpenAPI schema')" @click="emit('edit', c.path)">
              {{ $t("Edit") }}
            </button>
            <template v-if="c.folder !== null && c.folder !== undefined" />
            <template v-else-if="linking === c.path">
              <select v-model="linkFolder" :aria-label="$t('Folder to link')">
                <option v-for="f in folders" :key="f.id" :value="f.id">{{ f.label }}</option>
              </select>
              <button class="ghost" :disabled="!!busy" @click="link(c)">{{ $t("Link") }}</button>
              <button class="ghost" @click="linking = null">{{ $t("Cancel") }}</button>
            </template>
            <template v-else>
              <button class="ghost" :disabled="!!busy" @click="importContract(c)">{{ $t("Import") }}</button>
              <button class="ghost" :disabled="!!busy" :title="$t('For a collection made by hand or imported from Postman')" @click="linking = c.path; linkFolder = ''">
                {{ $t("Link to a folder…") }}
              </button>
            </template>
          </div>
        </div>
        <div v-for="c in unsupported" :key="c.path" class="contract off">
          <div>
            <b>{{ c.title }}</b> <span class="muted small">{{ c.kind }} · {{ c.operations }}</span>
            <div class="muted small"><code>{{ c.path }}</code> · {{ c.note }}</div>
          </div>
        </div>
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
  container-type: inline-size;
  width: min(860px, 94vw);
  max-height: 88vh;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
header {
  display: flex;
  justify-content: space-between;
  gap: 12px;
}
h2 {
  margin: 0;
  font-size: 17px;
}
h3 {
  margin: 0;
  font-size: 14px;
}
.section {
  margin-top: 8px;
}
.sub,
.note,
.muted {
  color: var(--muted);
  margin: 4px 0 0;
}
.sub,
.note,
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
  margin: 0;
  font-size: 13px;
}
.list {
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.drift {
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.row {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 4px 8px;
  font-size: 13px;
  cursor: pointer;
}
.tag {
  width: 64px;
  font-size: 11px;
  text-transform: uppercase;
  font-weight: 600;
}
.tag.add {
  color: var(--m-get);
}
.tag.mod {
  color: var(--m-post);
}
.tag.del {
  color: var(--m-delete);
}
.method {
  font-size: 11px;
  color: var(--muted);
  width: 54px;
}
.drift footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 6px;
}
.contract {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  border-bottom: 1px solid var(--border);
  padding: 6px 0;
  font-size: 13px;
  flex-wrap: wrap;
}
.contract.off {
  opacity: 0.65;
}
.actions {
  display: flex;
  gap: 6px;
  align-items: center;
}
select {
  max-width: 260px;
}
.link {
  background: none;
  border: none;
  padding: 0;
  color: var(--accent);
  cursor: pointer;
  font: inherit;
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
button:disabled {
  opacity: 0.55;
  cursor: default;
}
</style>
