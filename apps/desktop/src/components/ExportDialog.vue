<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { backend, type Environment, type ExportFormat, type Exported } from "../workspace";

const props = defineProps<{ root: string; folderId: string; environments: Environment[] }>();
const emit = defineEmits<{ close: [] }>();

type Choice = ExportFormat | `env:${string}`;
const choice = ref<Choice>("postman");
const exported = ref<Exported | null>(null);
const problem = ref("");
const note = ref("");
const busy = ref(false);
const available = computed(() => !!backend.exportCollection);

const FORMATS: { value: ExportFormat; label: string; hint: string }[] = [
  { value: "postman", label: "Postman v2.1", hint: "Folders, auth, scripts and examples. Also opens in Insomnia and Bruno." },
  { value: "openApi", label: "OpenAPI 3", hint: "Paths, parameters, bodies and security, grouped by folder." },
  { value: "http", label: ".http file", hint: "For the VS Code REST Client and JetBrains HTTP Client." },
  {
    value: "html",
    label: "HTML documentation",
    hint: "One page with every request, its tests and saved responses; ready for GitHub Pages. Variable values are left out.",
  },
];

async function load() {
  if (!available.value) return;
  busy.value = true;
  problem.value = "";
  note.value = "";
  exported.value = null;
  try {
    const value = choice.value;
    exported.value = value.startsWith("env:")
      ? await backend.exportEnvironment!(props.root, value.slice(4))
      : await backend.exportCollection!(props.root, props.folderId, value as ExportFormat);
  } catch (e) {
    problem.value = e instanceof Error ? e.message : String(e);
  }
  busy.value = false;
}
watch(choice, load, { immediate: true });

const preview = computed(() => {
  const text = exported.value?.text ?? "";
  const lines = text.split("\n");
  return lines.length > 40 ? `${lines.slice(0, 40).join("\n")}\n…` : text;
});

async function saveFile() {
  if (!exported.value || !backend.saveTextFile) return;
  try {
    if (await backend.saveTextFile(exported.value.fileName, exported.value.text)) note.value = "Saved.";
  } catch (e) {
    problem.value = e instanceof Error ? e.message : String(e);
  }
}

async function copy() {
  if (!exported.value) return;
  await navigator.clipboard.writeText(exported.value.text);
  note.value = "Copied to the clipboard.";
}
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="dialog card" role="dialog" aria-label="Export">
      <header>
        <h2>Export {{ folderId ? "folder" : "collection" }}</h2>
        <button class="icon" title="Close" @click="emit('close')">✕</button>
      </header>
      <p v-if="!available" class="muted">
        Exporting runs in the Pulpo engine, so it needs the desktop app (or <code>pulpo export</code> in a terminal).
      </p>
      <template v-else>
        <p class="muted">
          From <code>{{ folderId || "the whole workspace" }}</code>. Variables stay as <code v-pre>{{name}}</code>; secrets
          never leave the keychain.
        </p>
        <div class="formats" role="radiogroup" aria-label="Format">
          <label v-for="f in FORMATS" :key="f.value" class="format" :class="{ on: choice === f.value }">
            <input v-model="choice" type="radio" name="format" :value="f.value" />
            <span>
              <b>{{ f.label }}</b>
              <small>{{ f.hint }}</small>
            </span>
          </label>
          <label v-for="env in environments" :key="env.id" class="format" :class="{ on: choice === `env:${env.id}` }">
            <input v-model="choice" type="radio" name="format" :value="`env:${env.id}`" />
            <span>
              <b>Environment “{{ env.name }}”</b>
              <small>As a Postman environment; secret values are left empty.</small>
            </span>
          </label>
        </div>
        <p v-if="problem" class="result bad">{{ problem }}</p>
        <template v-else-if="exported">
          <p class="muted">
            <template v-if="!choice.startsWith('env:')">{{ exported.requests }} requests → </template>
            <code>{{ exported.fileName }}</code>
          </p>
          <ul v-if="exported.warnings.length" class="warnings">
            <li v-for="(w, i) in exported.warnings.slice(0, 8)" :key="i">{{ w }}</li>
            <li v-if="exported.warnings.length > 8">… and {{ exported.warnings.length - 8 }} more</li>
          </ul>
          <pre class="code">{{ preview }}</pre>
        </template>
        <footer>
          <span v-if="note" class="note" role="status">{{ note }}</span>
          <button class="ghost" :disabled="busy || !exported" @click="copy">Copy</button>
          <button class="primary" :disabled="busy || !exported" @click="saveFile">Save as…</button>
        </footer>
      </template>
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
.dialog {
  width: min(680px, 92vw);
  max-height: 86vh;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
header,
footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
footer {
  justify-content: flex-end;
}
h2 {
  margin: 0;
  font-size: 17px;
}
.icon {
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}
.muted,
.note {
  color: var(--muted);
  margin: 0;
  font-size: 13px;
}
.note {
  margin-right: auto;
  color: var(--ok);
}
.formats {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
  gap: 8px;
}
.format {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 8px 10px;
  cursor: pointer;
}
.format.on {
  border-color: var(--accent);
}
.format span {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.format small {
  color: var(--muted);
  font-size: 12px;
}
.warnings {
  margin: 0;
  padding-left: 18px;
  color: var(--m-post);
  font-size: 12px;
}
.code {
  margin: 0;
  max-height: 280px;
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface-2);
  padding: 10px;
  font: 12px/1.5 var(--mono);
  white-space: pre;
}
.primary,
.ghost {
  border-radius: 8px;
  padding: 9px 16px;
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
}
button:disabled {
  opacity: 0.55;
  cursor: default;
}
.result.bad {
  margin: 0;
  padding: 10px 12px;
  border-radius: 8px;
  background: var(--surface-2);
  border-left: 3px solid var(--m-delete);
  font-size: 13px;
}
</style>
