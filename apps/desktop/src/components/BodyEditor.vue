<script setup lang="ts">
import { computed, ref } from "vue";
import { bodyMode, type BodyMode, type FormField, type HttpRequest } from "../core";
import { backend, toWorkspacePath } from "../workspace";
import CodeEditor, { type Language } from "./CodeEditor.vue";
import VarInput from "./VarInput.vue";

const props = defineProps<{ request: HttpRequest; root: string }>();
const emit = defineEmits<{ send: []; save: [] }>();

const MODES: { mode: BodyMode; label: string }[] = [
  { mode: "none", label: "None" },
  { mode: "raw", label: "Raw" },
  { mode: "urlencoded", label: "URL encoded" },
  { mode: "multipart", label: "Form data" },
  { mode: "file", label: "File" },
];

const mode = computed(() => bodyMode(props.request));
const hasBody = computed(() => !["GET", "HEAD"].includes(props.request.method));

// What each mode had, so switching back and forth does not lose it before saving.
const kept: { raw: string | null; fields: FormField[]; file: string } = {
  raw: props.request.body ?? null,
  fields: props.request.form?.fields ?? [],
  file: props.request.bodyFile ?? "",
};

/** Content types Pulpo (or an importer) writes by itself; a body switch replaces them. */
const AUTOMATIC_TYPES = [
  "application/json",
  "application/xml",
  "text/xml",
  "text/plain",
  "application/x-www-form-urlencoded",
  "multipart/form-data",
];

function isContentType(h: { name: string }) {
  return h.name.trim().toLowerCase() === "content-type";
}

function setMode(next: BodyMode) {
  const r = props.request;
  // Drop an automatic Content-Type: forms and files get theirs when sending, and a stale
  // "application/json" would make the server misread a form.
  r.headers = r.headers.filter(
    (h) => !(isContentType(h) && AUTOMATIC_TYPES.some((t) => h.value.trim().toLowerCase().startsWith(t))),
  );
  if (next === "raw" && kept.raw && /^\s*[[{]/.test(kept.raw)) {
    r.headers.push({ name: "Content-Type", value: "application/json", enabled: true });
  }
  if (r.body != null) kept.raw = r.body;
  if (r.form) kept.fields = r.form.fields;
  if (r.bodyFile != null) kept.file = r.bodyFile;
  r.body = next === "raw" ? (kept.raw ?? "") : null;
  r.bodyFile = next === "file" ? kept.file : null;
  if (next === "urlencoded" || next === "multipart") {
    // Urlencoded forms cannot carry files.
    const fields = next === "urlencoded" ? kept.fields.filter((f) => !f.file) : kept.fields;
    r.form = { kind: next, fields: fields.length ? fields : [{ name: "", value: "", file: false, enabled: true }] };
  } else {
    r.form = null;
  }
}

// ---------------------------------------------------------------- raw

const raw = computed({
  get: () => props.request.body ?? "",
  set: (value: string) => {
    props.request.body = value;
  },
});
type RawKind = "text" | "json" | "xml" | "html" | "javascript" | "graphql";

/** The raw types, as in Postman; choosing one sets the Content-Type header. */
const RAW_KINDS: { kind: RawKind; label: string; mime: string }[] = [
  { kind: "text", label: "Text", mime: "text/plain" },
  { kind: "json", label: "JSON", mime: "application/json" },
  { kind: "xml", label: "XML", mime: "application/xml" },
  { kind: "html", label: "HTML", mime: "text/html" },
  { kind: "javascript", label: "JavaScript", mime: "application/javascript" },
  { kind: "graphql", label: "GraphQL", mime: "application/graphql" },
];

const contentTypeHeader = computed(() =>
  props.request.headers.find((h) => h.enabled && h.name.trim().toLowerCase() === "content-type"),
);

/** The raw type, from Content-Type or else from the body. Mirrors `BodyKind::detect`
 * in pulpo-core, which also picks the body file's extension. */
const rawKind = computed<RawKind>(() => {
  const contentType = contentTypeHeader.value?.value.toLowerCase() ?? "";
  if (contentType.includes("json")) return "json";
  if (contentType.includes("html")) return "html";
  if (contentType.includes("xml")) return "xml";
  if (contentType.includes("javascript") || contentType.includes("ecmascript")) return "javascript";
  if (contentType.includes("graphql")) return "graphql";
  if (contentType) return "text";
  const start = (props.request.body ?? "").trimStart();
  const head = start.slice(0, 15).toLowerCase();
  if (start.startsWith("{") || start.startsWith("[")) return "json";
  if (head.startsWith("<!doctype html") || head.startsWith("<html")) return "html";
  if (start.startsWith("<")) return "xml";
  return "text";
});

function setRawKind(kind: RawKind) {
  const mime = RAW_KINDS.find((k) => k.kind === kind)!.mime;
  const header = contentTypeHeader.value;
  if (header) header.value = mime;
  else props.request.headers.push({ name: "Content-Type", value: mime, enabled: true });
}

const language = computed<Language>(() => {
  switch (rawKind.value) {
    case "json":
      return "json";
    case "xml":
    case "html":
      return "xml";
    case "javascript":
      return "javascript";
    default:
      return "text";
  }
});

function formatJson() {
  if (!props.request.body) return;
  try {
    props.request.body = JSON.stringify(JSON.parse(props.request.body), null, 2);
  } catch {
    /* not JSON, leave as is */
  }
}

// ---------------------------------------------------------------- form

const fields = computed(() => props.request.form?.fields ?? []);
const contentType = computed(
  () => props.request.headers.find((h) => h.enabled && isContentType(h))?.value.trim() ?? "",
);
const hasContentType = computed(() => contentType.value !== "");
/** A Content-Type header that will make the server misread a urlencoded form. */
const wrongType = computed(
  () =>
    mode.value === "urlencoded" &&
    hasContentType.value &&
    !contentType.value.toLowerCase().startsWith("application/x-www-form-urlencoded"),
);

function addField() {
  props.request.form?.fields.push({ name: "", value: "", file: false, enabled: true });
}

function removeField(index: number) {
  props.request.form?.fields.splice(index, 1);
}

function setFieldType(field: FormField, file: boolean) {
  if (field.file !== file) {
    field.file = file;
    field.value = "";
  }
}

// ---------------------------------------------------------------- files

const pickError = ref("");
const canPick = computed(() => !!backend.pickUploadFile && !!props.root);

async function pick(apply: (path: string) => void) {
  pickError.value = "";
  const picked = await backend.pickUploadFile?.();
  if (!picked) return;
  const relative = toWorkspacePath(props.root, picked);
  if (relative === null) {
    pickError.value = "That file is outside the workspace. Copy it into the workspace (e.g. files/) so it travels with the collection.";
    return;
  }
  apply(relative);
}
</script>

<template>
  <div class="body-editor">
    <div class="modes" role="radiogroup" aria-label="Body type">
      <label v-for="m in MODES" :key="m.mode" :class="{ on: mode === m.mode }">
        <input type="radio" name="body-mode" :checked="mode === m.mode" @change="setMode(m.mode)" />
        {{ m.label }}
      </label>
      <select
        v-if="mode === 'raw'"
        class="raw-kind"
        aria-label="Raw type"
        :value="rawKind"
        :title="contentTypeHeader ? `Content-Type: ${contentTypeHeader.value}` : 'No Content-Type yet: detected from the body'"
        @change="setRawKind(($event.target as HTMLSelectElement).value as RawKind)"
      >
        <option v-for="k in RAW_KINDS" :key="k.kind" :value="k.kind">{{ k.label }}</option>
      </select>
      <span v-if="mode === 'raw' && !contentTypeHeader" class="detected">detected</span>
      <span class="spacer"></span>
      <button v-if="mode === 'raw' && rawKind === 'json'" class="link" @click="formatJson">Format JSON</button>
    </div>

    <p v-if="!hasBody && mode !== 'none'" class="warn">{{ request.method }} requests usually have no body; servers may ignore it.</p>

    <p v-if="mode === 'none'" class="empty">This request sends no body.</p>

    <CodeEditor
      v-else-if="mode === 'raw'"
      variables
      v-model="raw"
      class="code-area"
      :language="language"
      placeholder='{ "hello": "pulpo" }'
      @run="emit('send')"
      @save="emit('save')"
    />

    <div v-else-if="mode === 'urlencoded' || mode === 'multipart'" class="form">
      <div v-for="(field, i) in fields" :key="i" class="row" :class="{ multipart: mode === 'multipart' }">
        <input v-model="field.enabled" type="checkbox" :title="field.enabled ? 'Sent' : 'Not sent'" />
        <input v-model="field.name" placeholder="name" spellcheck="false" />
        <select
          v-if="mode === 'multipart'"
          :value="field.file ? 'file' : 'text'"
          title="Field type"
          @change="setFieldType(field, ($event.target as HTMLSelectElement).value === 'file')"
        >
          <option value="text">Text</option>
          <option value="file">File</option>
        </select>
        <div v-if="field.file" class="file">
          <input v-model="field.value" placeholder="files/photo.png" spellcheck="false" />
          <button v-if="canPick" class="ghost" title="Choose a file in the workspace" @click="pick((p) => (field.value = p))">…</button>
        </div>
        <VarInput v-else v-model="field.value" placeholder="value" :aria-label="`Value of ${field.name || 'field'}`" />
        <button class="icon" title="Remove" @click="removeField(i)">✕</button>
      </div>
      <button class="link" @click="addField">+ Add field</button>
      <p v-if="wrongType" class="warn">
        The Content-Type header says <code>{{ contentType }}</code>, so the server will not read this form. Remove it in Headers.
      </p>
      <p class="hint">
        <template v-if="mode === 'urlencoded'">
          Sent as <code>application/x-www-form-urlencoded</code><template v-if="!hasContentType">; Pulpo sets the header</template>.
        </template>
        <template v-else>
          Sent as <code>multipart/form-data</code>. Pulpo writes that header with its boundary<template v-if="hasContentType">, so the Content-Type in Headers is not used</template>.
          Files are paths inside the workspace, so the collection works on any machine.
        </template>
      </p>
    </div>

    <div v-else-if="mode === 'file'" class="whole-file">
      <div class="file">
        <input
          :value="request.bodyFile ?? ''"
          placeholder="files/payload.bin"
          spellcheck="false"
          @input="request.bodyFile = ($event.target as HTMLInputElement).value"
        />
        <button v-if="canPick" class="ghost" @click="pick((p) => (request.bodyFile = p))">Choose…</button>
      </div>
      <p class="hint">
        The whole body is this file, relative to the workspace. Content-Type comes from its extension unless you set one in
        Headers.
      </p>
    </div>

    <p v-if="pickError" class="warn">{{ pickError }}</p>
  </div>
</template>

<style scoped>
.body-editor {
  container-type: inline-size;
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 0;
}
.modes {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px 12px;
  font-size: 12px;
}
.modes label {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--muted);
  cursor: pointer;
  white-space: nowrap;
}
.modes label.on {
  color: var(--text);
  font-weight: 600;
}
.spacer {
  flex: 1;
}
.code-area {
  min-height: 160px;
}
.form {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.row {
  display: grid;
  grid-template-columns: 20px minmax(0, 1fr) minmax(0, 2fr) 28px;
  gap: 6px;
  align-items: center;
}
.row.multipart {
  grid-template-columns: 20px minmax(0, 1fr) 70px minmax(0, 2fr) 28px;
}
/* Narrow: name and type on the first line; value and remove on the second. */
@container (max-width: 460px) {
  .row.multipart {
    grid-template-columns: 20px minmax(0, 1fr) 64px;
  }
  .row.multipart > :nth-child(4) {
    grid-column: 2;
  }
  .row.multipart > :nth-child(5) {
    justify-self: start;
  }
}
input:not([type="checkbox"]):not([type="radio"]),
select {
  min-width: 0;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 13px;
  outline: none;
}
select {
  font-family: inherit;
  cursor: pointer;
}
input:focus,
select:focus {
  border-color: var(--accent);
}
.file {
  display: flex;
  gap: 6px;
  min-width: 0;
}
.file input {
  flex: 1;
}
.whole-file .file {
  max-width: 560px;
}
.ghost {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: transparent;
  color: var(--text);
  padding: 0 10px;
  cursor: pointer;
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
  padding: 4px 2px;
}
.hint,
.empty,
.warn {
  margin: 0;
  font-size: 12px;
}
.hint,
.empty {
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.raw-kind {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  color: var(--text);
  padding: 3px 6px;
  font-size: 12px;
  cursor: pointer;
}
.detected {
  font-size: 11px;
  color: var(--muted);
}
</style>
