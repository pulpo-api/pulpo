<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import type { Auth } from "../core";
import {
  asCollectionError,
  backend,
  clone,
  inheritedAuth,
  inheritedHeaders,
  inheritedSettings,
  type FolderMeta,
  type Scripts,
  type Workspace,
} from "../workspace";
import AuthEditor from "./AuthEditor.vue";
import CodeEditor from "./CodeEditor.vue";
import HeadersEditor from "./HeadersEditor.vue";
import SettingsEditor from "./SettingsEditor.vue";

/** Edits what a folder ("" = the whole collection) sets for the requests inside. */
const props = defineProps<{ workspace: Workspace; folderId: string }>();
const emit = defineEmits<{ close: []; saved: [folderId: string, meta: FolderMeta] }>();

const tab = ref<"headers" | "auth" | "settings" | "pre" | "test">("headers");
const savedMeta = ref<FolderMeta | null>(null);
const savedScripts = ref<Scripts>({ pre: "", test: "" });
const meta = ref<FolderMeta | null>(null);
const scripts = ref<Scripts>({ pre: "", test: "" });
const problem = ref("");

const isCollection = computed(() => props.folderId === "");

async function load() {
  try {
    const [m, s] = await Promise.all([
      backend.folderMeta(props.workspace.root, props.folderId),
      backend.folderScripts(props.workspace.root, props.folderId),
    ]);
    savedMeta.value = m;
    savedScripts.value = s;
    meta.value = clone(m);
    scripts.value = clone(s);
    problem.value = "";
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}
onMounted(load);
watch(() => props.folderId, load);

const dirty = computed(
  () =>
    JSON.stringify([meta.value, scripts.value]) !== JSON.stringify([savedMeta.value, savedScripts.value]),
);

// What the parents give (for a folder; the collection is the top).
const parentAuth = computed(() => (isCollection.value ? undefined : inheritedAuth(props.workspace, props.folderId)));
// The collection is the top level: what it does not set falls back to the defaults.
const parentSettings = computed(() =>
  isCollection.value
    ? inheritedSettings({ root: "", name: "", tree: [] }, "")
    : inheritedSettings(props.workspace, props.folderId),
);
const parentHeaders = computed(() => (isCollection.value ? [] : inheritedHeaders(props.workspace, props.folderId)));

const auth = computed({
  get: () => meta.value?.auth ?? null,
  set: (value: Auth | null) => {
    if (meta.value) meta.value.auth = value;
  },
});

const headerCount = computed(() => meta.value?.headers.filter((h) => h.enabled && h.name.trim()).length ?? 0);
const settingsCount = computed(() => Object.keys(meta.value?.settings ?? {}).length);

async function save() {
  if (!meta.value) return;
  try {
    const clean: FolderMeta = {
      ...clone(meta.value),
      headers: meta.value.headers.filter((h) => h.name.trim()),
    };
    await backend.saveFolderMeta(props.workspace.root, props.folderId, clean);
    await backend.saveFolderScripts(props.workspace.root, props.folderId, scripts.value);
    emit("saved", props.folderId, clean);
    await load();
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}

defineExpose({ save, dirty });
</script>

<template>
  <section class="card folder-editor">
    <header>
      <div>
        <p class="kind">{{ isCollection ? "Collection" : "Folder" }}</p>
        <h2>{{ meta?.name ?? "…" }}</h2>
        <p class="sub">
          Every request inside inherits these, unless it sets its own.
          <template v-if="!isCollection">Rename it from the tree (F2).</template>
        </p>
      </div>
      <div class="actions">
        <button class="ghost" :disabled="!dirty" @click="load">Discard</button>
        <button class="primary" :disabled="!dirty" title="Ctrl+S" @click="save">{{ dirty ? "Save" : "Saved" }}</button>
        <button class="ghost" title="Back to the request" @click="emit('close')">✕</button>
      </div>
    </header>

    <p v-if="problem" class="warn">{{ problem }}</p>

    <template v-if="meta">
      <div class="tabs">
        <button :class="{ on: tab === 'headers' }" @click="tab = 'headers'">
          Headers <span v-if="headerCount" class="count">{{ headerCount }}</span>
        </button>
        <button :class="{ on: tab === 'auth' }" @click="tab = 'auth'">
          Auth <span v-if="meta.auth && meta.auth.type !== 'none'" class="count">set</span>
        </button>
        <button :class="{ on: tab === 'settings' }" @click="tab = 'settings'">
          Settings <span v-if="settingsCount" class="count">{{ settingsCount }}</span>
        </button>
        <button :class="{ on: tab === 'pre' }" @click="tab = 'pre'">
          Pre-request <span v-if="scripts.pre.trim()" class="dot"></span>
        </button>
        <button :class="{ on: tab === 'test' }" @click="tab = 'test'">
          Tests <span v-if="scripts.test.trim()" class="dot"></span>
        </button>
      </div>

      <HeadersEditor v-if="tab === 'headers'" :headers="meta.headers" :inherited="parentHeaders">
        <p class="hint">
          Sent by every request inside (Postman makes you repeat them). A request can override one by setting the same
          header, or stop it with a disabled one.
        </p>
      </HeadersEditor>

      <AuthEditor v-else-if="tab === 'auth'" v-model="auth" :inherited="parentAuth" />

      <SettingsEditor
        v-else-if="tab === 'settings'"
        :request="meta"
        :inherited="parentSettings"
        :root="workspace.root"
        resolved-url=""
        :certificates="false"
      />

      <template v-else>
        <CodeEditor :key="tab" v-model="scripts[tab]" class="code-area" language="javascript" @save="save" />
        <p class="hint">
          Saved as <code>_folder.{{ tab === "pre" ? "pre" : "test" }}.js</code>. Runs
          {{ tab === "pre" ? "before" : "after" }} every request inside, before the request's own script.
        </p>
      </template>
    </template>
  </section>
</template>

<style scoped>
.folder-editor {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 0;
}
header {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}
.kind {
  margin: 0;
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--accent);
}
h2 {
  margin: 2px 0;
  font-size: 18px;
}
.sub,
.hint,
.warn {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.actions {
  display: flex;
  gap: 8px;
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
.tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  margin: 6px 0 4px;
  overflow-x: auto;
}
.tabs button {
  flex-shrink: 0;
  white-space: nowrap;
  border: 0;
  background: transparent;
  padding: 6px 10px;
  border-radius: 6px;
  color: var(--muted);
  cursor: pointer;
}
.tabs button.on {
  background: var(--surface-2);
  color: var(--text);
  font-weight: 600;
}
.count {
  margin-left: 4px;
  padding: 0 6px;
  border-radius: 999px;
  background: var(--accent-soft);
  color: var(--accent);
  font-size: 11px;
}
.dot {
  display: inline-block;
  width: 6px;
  height: 6px;
  margin-left: 4px;
  border-radius: 50%;
  background: var(--accent);
  vertical-align: middle;
}
.code-area {
  min-height: 200px;
}
</style>
