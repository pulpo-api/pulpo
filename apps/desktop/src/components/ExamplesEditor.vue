<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { asCollectionError, backend, clone, type Example } from "../workspace";
import CodeEditor, { type Language } from "./CodeEditor.vue";
import HeadersEditor from "./HeadersEditor.vue";

/** Saved responses of a request: what the endpoint answers, for the team and the mock. */
const props = defineProps<{ root: string; requestId: string; startNew?: boolean }>();

const examples = ref<Example[]>([]);
const selectedId = ref<string | null>(null);
const draft = ref<Example | null>(null);
const problem = ref("");
const confirmDelete = ref(false);

async function load(select?: string) {
  try {
    examples.value = await backend.listExamples(props.root, props.requestId);
    const keep = select ?? selectedId.value;
    pick(examples.value.find((e) => e.id === keep) ?? examples.value[0] ?? null);
    problem.value = "";
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}

function pick(example: Example | null) {
  selectedId.value = example?.id ?? null;
  draft.value = example ? clone(example) : null;
  confirmDelete.value = false;
}

function addNew() {
  selectedId.value = "";
  draft.value = {
    id: "",
    name: "New example",
    status: 200,
    headers: [{ name: "Content-Type", value: "application/json", enabled: true }],
    body: "{\n  \n}\n",
  };
}

onMounted(async () => {
  await load();
  if (props.startNew) addNew();
});
watch(() => props.requestId, () => load());

const saved = computed(() => examples.value.find((e) => e.id === selectedId.value) ?? null);
const dirty = computed(() => !!draft.value && JSON.stringify(draft.value) !== JSON.stringify(saved.value));

const language = computed<Language>(() => {
  const type = draft.value?.headers.find((h) => h.name.toLowerCase() === "content-type")?.value ?? "";
  if (type.includes("json") || /^\s*[[{]/.test(draft.value?.body ?? "")) return "json";
  if (type.includes("xml") || type.includes("html")) return "xml";
  return "text";
});

async function save() {
  if (!draft.value) return;
  try {
    const result = await backend.saveExample(props.root, props.requestId, {
      ...clone(draft.value),
      headers: draft.value.headers.filter((h) => h.name.trim()),
    });
    await load(result.id);
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}

async function remove() {
  if (!draft.value?.id) {
    pick(examples.value[0] ?? null);
    return;
  }
  if (!confirmDelete.value) {
    confirmDelete.value = true;
    return;
  }
  try {
    await backend.deleteExample(props.root, props.requestId, draft.value.id);
    selectedId.value = null;
    await load();
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}

defineExpose({ addNew, reload: load });
</script>

<template>
  <div class="examples">
    <aside>
      <button
        v-for="e in examples"
        :key="e.id"
        class="item"
        :class="{ on: e.id === selectedId }"
        @click="pick(e)"
      >
        <span class="code" :class="`s${Math.floor(e.status / 100)}`">{{ e.status }}</span>
        <span class="name">{{ e.name }}</span>
      </button>
      <button v-if="selectedId === ''" class="item on"><span class="code">new</span><span class="name">{{ draft?.name }}</span></button>
      <button class="link" @click="addNew">+ Add example</button>
      <p class="hint">
        Saved next to the request in <code>.examples/</code>. Make one from a response with "Save as example".
      </p>
    </aside>

    <section v-if="draft" class="editor">
      <div class="row">
        <input v-model="draft.name" class="name-input" aria-label="Example name" spellcheck="false" />
        <input v-model.number="draft.status" class="status" type="number" min="100" max="599" aria-label="Status code" />
        <button class="ghost danger" :class="{ armed: confirmDelete }" @click="remove">
          {{ confirmDelete ? "Click again to delete" : draft.id ? "Delete" : "Discard" }}
        </button>
        <button class="primary" :disabled="!dirty" @click="save">{{ dirty ? "Save" : "Saved" }}</button>
      </div>
      <p v-if="problem" class="warn">{{ problem }}</p>
      <HeadersEditor :headers="draft.headers" />
      <CodeEditor v-model="draft.body" class="body" :language="language" @save="save" />
    </section>
    <p v-else class="empty">No examples yet.</p>
  </div>
</template>

<style scoped>
.examples {
  display: grid;
  grid-template-columns: minmax(150px, 220px) minmax(0, 1fr);
  gap: 12px;
  min-height: 220px;
}
@container (max-width: 520px) {
  .examples {
    grid-template-columns: minmax(0, 1fr);
  }
}
aside {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.item {
  display: flex;
  align-items: center;
  gap: 8px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--text);
  padding: 6px 8px;
  text-align: left;
  cursor: pointer;
}
.item.on {
  background: var(--accent-soft);
}
.item .code {
  font: 600 11px var(--mono);
}
.s2 { color: var(--m-get); }
.s3 { color: var(--m-patch); }
.s4 { color: var(--m-post); }
.s5 { color: var(--m-delete); }
.item .name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.link {
  align-self: flex-start;
  border: 0;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  padding: 6px 8px;
}
.hint,
.empty,
.warn {
  margin: 4px 8px;
  font-size: 12px;
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.editor {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-width: 0;
}
.row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
.name-input,
.status {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  color: var(--text);
  padding: 6px 8px;
  outline: none;
}
.name-input {
  flex: 1 1 160px;
}
.status {
  width: 80px;
  font-family: var(--mono);
}
.body {
  min-height: 160px;
}
.ghost,
.primary {
  border-radius: 8px;
  padding: 6px 14px;
  cursor: pointer;
}
.ghost {
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text);
}
.ghost.danger.armed {
  border-color: var(--m-delete);
  color: var(--m-delete);
}
.primary {
  border: 0;
  background: var(--accent);
  color: #fff;
  font-weight: 600;
}
button:disabled {
  opacity: 0.5;
  cursor: default;
}
</style>
