<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { clone, type Environment } from "../workspace";

const props = defineProps<{ environments: Environment[]; activeId: string; persistent: boolean }>();
const emit = defineEmits<{
  close: [];
  save: [env: Environment];
  create: [name: string];
  remove: [id: string];
}>();

const selectedId = ref(props.activeId || props.environments[0]?.id || "");
const draft = ref<Environment | null>(null);
const newName = ref("");
const confirmDelete = ref(false);

const original = computed(() => props.environments.find((e) => e.id === selectedId.value) ?? null);

watch(
  [selectedId, () => props.environments],
  () => {
    draft.value = original.value ? clone(original.value) : null;
    confirmDelete.value = false;
  },
  { immediate: true },
);

const dirty = computed(() => {
  if (!draft.value || !original.value) return false;
  return JSON.stringify(draft.value) !== JSON.stringify(original.value);
});

const duplicates = computed(() => {
  const seen = new Set<string>();
  const dup = new Set<string>();
  for (const v of draft.value?.variables ?? []) {
    const name = v.name.trim();
    if (seen.has(name)) dup.add(name);
    seen.add(name);
  }
  return dup;
});

function addVariable() {
  draft.value?.variables.push({ name: "", value: "", secret: false, hasValue: false });
}

function save() {
  if (!draft.value || duplicates.value.size) return;
  const env = clone(draft.value);
  env.variables = env.variables.filter((v) => v.name.trim()).map((v) => ({ ...v, name: v.name.trim() }));
  emit("save", env);
}

function create() {
  const name = newName.value.trim();
  if (!name) return;
  emit("create", name);
  newName.value = "";
}

function remove() {
  if (!draft.value) return;
  if (!confirmDelete.value) {
    confirmDelete.value = true;
    return;
  }
  emit("remove", draft.value.id);
}

watch(
  () => props.environments.length,
  (now, before) => {
    if (now > before) selectedId.value = props.environments[now - 1].id;
    else if (!props.environments.some((e) => e.id === selectedId.value)) selectedId.value = props.environments[0]?.id ?? "";
  },
);
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="panel card" role="dialog" aria-label="Environments">
      <header>
        <h2>Environments</h2>
        <button class="icon" title="Close" @click="emit('close')">✕</button>
      </header>

      <div class="body">
        <aside>
          <button
            v-for="env in environments"
            :key="env.id"
            class="env"
            :class="{ on: env.id === selectedId }"
            @click="selectedId = env.id"
          >
            {{ env.name }}
          </button>
          <input v-model="newName" class="new" placeholder="+ New environment" @keydown.enter="create" />
        </aside>

        <div v-if="draft" class="editor">
          <div class="row head">
            <span>Name</span><span>Value</span><span title="Stored in the OS keychain, never in the repo">Secret</span><span></span>
          </div>
          <div v-for="(v, i) in draft.variables" :key="i" class="row">
            <input v-model="v.name" :class="{ bad: duplicates.has(v.name.trim()) }" placeholder="name" spellcheck="false" />
            <input
              v-model="v.value"
              :type="v.secret ? 'password' : 'text'"
              :placeholder="v.secret ? (v.hasValue ? '•••••• stored in keychain' : 'not set') : 'value'"
              spellcheck="false"
              autocomplete="off"
            />
            <input v-model="v.secret" type="checkbox" />
            <button class="icon" title="Remove" @click="draft.variables.splice(i, 1)">✕</button>
          </div>
          <button class="link" @click="addVariable">+ Add variable</button>

          <p class="note">
            Plain values are saved in <code>{{ draft.id }}</code> and go to git. Secret values stay in
            {{ persistent ? "your OS keychain" : "memory (browser preview)" }} and never touch the repo. Leave a secret empty to keep
            the stored value.
          </p>
          <p v-if="duplicates.size" class="bad-note">Duplicated names: {{ [...duplicates].join(", ") }}</p>

          <footer>
            <button class="ghost danger" :class="{ armed: confirmDelete }" @click="remove">
              {{ confirmDelete ? "Click again to delete" : "Delete environment" }}
            </button>
            <span class="spacer"></span>
            <button class="primary" :disabled="!dirty || duplicates.size > 0" @click="save">
              {{ dirty ? "Save" : "Saved" }}
            </button>
          </footer>
        </div>
        <p v-else class="empty">No environments yet. Type a name on the left and press Enter.</p>
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
  width: min(900px, 92vw);
  max-height: 86vh;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
h2 {
  margin: 0;
  font-size: 17px;
}
.body {
  display: grid;
  grid-template-columns: 200px 1fr;
  gap: 16px;
  min-height: 0;
  overflow: auto;
}
aside {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.env {
  border: 0;
  background: transparent;
  text-align: left;
  padding: 7px 10px;
  border-radius: 8px;
  cursor: pointer;
}
.env:hover {
  background: var(--surface-2);
}
.env.on {
  background: var(--accent-soft);
  font-weight: 600;
}
.new {
  margin-top: 6px;
  border: 1px dashed var(--border);
  border-radius: 8px;
  background: transparent;
  padding: 7px 10px;
  outline: none;
}
.new:focus {
  border-color: var(--accent);
}
.editor {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.row {
  display: grid;
  grid-template-columns: 1fr 2fr 56px 28px;
  gap: 6px;
  align-items: center;
}
.row.head {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}
.row input:not([type="checkbox"]) {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 13px;
  outline: none;
}
.row input:focus {
  border-color: var(--accent);
}
.row input.bad {
  border-color: var(--m-delete);
}
.row input[type="checkbox"] {
  justify-self: center;
}
.icon {
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}
.link {
  align-self: flex-start;
  border: 0;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  padding: 6px 2px;
}
.note {
  color: var(--muted);
  font-size: 12px;
  margin: 6px 0 0;
}
.bad-note {
  color: var(--m-delete);
  font-size: 12px;
  margin: 0;
}
footer {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
}
.spacer {
  flex: 1;
}
.ghost {
  border: 1px solid var(--border);
  background: var(--surface);
  border-radius: 8px;
  padding: 7px 12px;
  cursor: pointer;
}
.danger.armed {
  border-color: var(--m-delete);
  color: var(--m-delete);
}
.primary {
  border: 0;
  border-radius: 8px;
  padding: 8px 18px;
  background: var(--accent);
  color: #fff;
  font-weight: 600;
  cursor: pointer;
}
.primary:disabled {
  opacity: 0.55;
  cursor: default;
}
.empty {
  color: var(--muted);
}
</style>
