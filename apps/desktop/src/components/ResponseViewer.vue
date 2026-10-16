<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import type { HttpResponse, SendError } from "../core";
import { asCollectionError, backend, type TestResult } from "../workspace";
import { base64ToBytes, contentTypeOf, previewKind, suggestedFileName } from "../responseFiles";
import CodeEditor, { type Language } from "./CodeEditor.vue";

export interface HistoryItem {
  at: string;
  label: string;
}

const props = defineProps<{
  response: HttpResponse | null;
  error: SendError | null;
  sending: boolean;
  tests: TestResult[];
  logs: string[];
  scriptErrors: string[];
  history: HistoryItem[];
  historyIndex: number;
  /** The request URL, to name saved files. */
  url: string;
}>();
const emit = defineEmits<{ pick: [index: number] }>();

const tab = ref<"body" | "headers" | "cookies" | "tests" | "console">("body");

/** Cookies this response sets, from its Set-Cookie headers. */
const setCookies = computed(() =>
  (props.response?.headers ?? [])
    .filter((h) => h.name.toLowerCase() === "set-cookie")
    .map((h) => {
      const [pair, ...attributes] = h.value.split(";");
      const eq = pair.indexOf("=");
      return {
        name: eq < 0 ? pair.trim() : pair.slice(0, eq).trim(),
        value: eq < 0 ? "" : pair.slice(eq + 1).trim(),
        attributes: attributes.map((a) => a.trim()).filter(Boolean).join("; "),
      };
    }),
);
const failed = computed(() => props.tests.filter((t) => !t.passed).length);

// ---------------------------------------------------------------- body views

const kind = computed(() => (props.response ? previewKind(props.response) : null));
const view = ref<"pretty" | "raw" | "preview">("pretty");
const saved = ref("");
// Images, PDFs and other binaries open in Preview; text in Pretty.
watch(
  () => props.response,
  () => {
    view.value = kind.value && kind.value !== "html" ? "preview" : "pretty";
    saved.value = "";
  },
  { immediate: true },
);

/** PDFs need a blob URL; it is released when the response changes. */
const pdfUrl = ref("");
watch(
  () => (kind.value === "pdf" ? props.response?.bodyBase64 : undefined),
  (base64) => {
    if (pdfUrl.value) URL.revokeObjectURL(pdfUrl.value);
    pdfUrl.value = base64 ? URL.createObjectURL(new Blob([base64ToBytes(base64)], { type: "application/pdf" })) : "";
  },
  { immediate: true },
);
onBeforeUnmount(() => {
  if (pdfUrl.value) URL.revokeObjectURL(pdfUrl.value);
});

const imageSrc = computed(() =>
  kind.value === "image" && props.response?.bodyBase64
    ? `data:${contentTypeOf(props.response).split(";")[0]};base64,${props.response.bodyBase64}`
    : "",
);

async function saveBody() {
  if (!props.response) return;
  try {
    const where = await backend.saveResponse(props.response, suggestedFileName(props.response, props.url));
    if (where) saved.value = `Saved to ${where}`;
  } catch (e) {
    saved.value = `Could not save: ${asCollectionError(e).message}`;
  }
}

const pretty = computed(() => {
  const body = props.response?.body ?? "";
  try {
    return JSON.stringify(JSON.parse(body), null, 2);
  } catch {
    return body;
  }
});

const bodyLanguage = computed<Language>(() => {
  const contentType = props.response?.headers.find((h) => h.name.toLowerCase() === "content-type")?.value ?? "";
  if (contentType.includes("json") || pretty.value !== (props.response?.body ?? "")) return "json";
  if (contentType.includes("xml") || contentType.includes("html")) return "xml";
  return "text";
});

const statusClass = computed(() => {
  const status = props.response?.status ?? 0;
  if (status >= 500) return "s5";
  if (status >= 400) return "s4";
  if (status >= 300) return "s3";
  return "s2";
});

function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

const errorTitle: Record<SendError["kind"], string> = {
  invalidUrl: "Invalid URL",
  invalidHeader: "Invalid header",
  timeout: "Timed out",
  connect: "Could not connect",
  unresolved: "Missing variables",
  file: "File problem",
  other: "Request failed",
};
</script>

<template>
  <section class="card viewer">
    <div v-if="sending" class="placeholder"><span class="spinner"></span> Waiting for the server…</div>

    <div v-else-if="error" class="error">
      <strong>{{ errorTitle[error.kind] }}</strong>
      <ul v-if="scriptErrors.length" class="script-errors">
        <li v-for="(e, i) in scriptErrors" :key="i">{{ e }}</li>
      </ul>
      <p>{{ error.message }}</p>
      <p v-if="error.kind === 'unresolved'" class="tip">Pick an environment that defines them, or add them with ⚙.</p>
    </div>

    <template v-else-if="response">
      <div class="status">
        <span class="pill" :class="statusClass">{{ response.status }} {{ response.statusText }}</span>
        <span class="muted">{{ response.durationMs }} ms · {{ size(response.sizeBytes) }} · {{ response.httpVersion }}</span>
        <span class="spacer"></span>
        <select
          v-if="history.length > 1"
          class="history"
          :value="historyIndex"
          title="Previous responses of this request (kept in memory only)"
          @change="emit('pick', Number(($event.target as HTMLSelectElement).value))"
        >
          <option v-for="(h, i) in history" :key="h.at + i" :value="i">{{ i === 0 ? "Latest · " : "" }}{{ h.label }}</option>
        </select>
        <button :class="{ on: tab === 'body' }" @click="tab = 'body'">Body</button>
        <button :class="{ on: tab === 'headers' }" @click="tab = 'headers'">Headers {{ response.headers.length }}</button>
        <button v-if="setCookies.length" :class="{ on: tab === 'cookies' }" @click="tab = 'cookies'">
          Cookies {{ setCookies.length }}
        </button>
        <button v-if="tests.length" :class="{ on: tab === 'tests', bad: failed }" @click="tab = 'tests'">
          Tests {{ tests.length - failed }}/{{ tests.length }}
        </button>
        <button v-if="logs.length || scriptErrors.length" :class="{ on: tab === 'console', bad: scriptErrors.length }" @click="tab = 'console'">
          Console
        </button>
      </div>
      <template v-if="tab === 'body'">
        <div class="views">
          <button :class="{ on: view === 'pretty' }" @click="view = 'pretty'">Pretty</button>
          <button :class="{ on: view === 'raw' }" @click="view = 'raw'">Raw</button>
          <button v-if="kind" :class="{ on: view === 'preview' }" @click="view = 'preview'">Preview</button>
          <span class="saved" :title="saved">{{ saved }}</span>
          <button class="save" title="Save the body to a file" @click="saveBody">Save</button>
        </div>
        <template v-if="view === 'preview' && kind">
          <!-- No scripts, no forms, no navigation: a response cannot act inside Pulpo. -->
          <iframe v-if="kind === 'html'" class="preview frame" sandbox="" :srcdoc="response.body" title="HTML preview"></iframe>
          <div v-else-if="kind === 'image'" class="preview image"><img :src="imageSrc" alt="Image in the response" /></div>
          <iframe v-else-if="kind === 'pdf'" class="preview frame" :src="pdfUrl" title="PDF preview"></iframe>
          <div v-else class="preview binary">
            <p>Binary response: {{ contentTypeOf(response) || "no Content-Type" }}, {{ size(response.sizeBytes) }}.</p>
            <p v-if="!response.bodyBase64">It is bigger than 25 MB, so Pulpo did not keep it.</p>
            <button v-else class="save" @click="saveBody">Save to a file</button>
          </div>
        </template>
        <div v-else-if="kind && kind !== 'html'" class="preview binary">
          <p>{{ contentTypeOf(response) || "Binary" }} body ({{ size(response.sizeBytes) }}): see it in Preview or save it.</p>
        </div>
        <CodeEditor
          v-else
          :key="`${historyIndex}-${view}`"
          :model-value="(view === 'raw' ? response.body : pretty) || '(empty body)'"
          :language="view === 'raw' ? 'text' : bodyLanguage"
          readonly
        />
      </template>
      <ul v-else-if="tab === 'tests'" class="code list">
        <li v-for="(t, i) in tests" :key="i" :class="t.passed ? 'pass' : 'fail'">
          <span>{{ t.passed ? "✓" : "✗" }}</span> {{ t.name }}<em v-if="t.error"> — {{ t.error }}</em>
        </li>
      </ul>
      <ul v-else-if="tab === 'console'" class="code list">
        <li v-for="(e, i) in scriptErrors" :key="`e${i}`" class="fail">! {{ e }}</li>
        <li v-for="(l, i) in logs" :key="`l${i}`">{{ l }}</li>
      </ul>
      <table v-else-if="tab === 'cookies'" class="code table">
        <tbody>
          <tr v-for="(c, i) in setCookies" :key="i">
            <td>{{ c.name }}</td>
            <td>{{ c.value }}<em v-if="c.attributes" class="attrs"> · {{ c.attributes }}</em></td>
          </tr>
        </tbody>
      </table>
      <table v-else class="code table">
        <tbody>
          <tr v-for="(h, i) in response.headers" :key="i">
            <td>{{ h.name }}</td>
            <td>{{ h.value }}</td>
          </tr>
        </tbody>
      </table>
    </template>

    <div v-else class="placeholder">
      <img src="/pulpo.svg" width="56" height="56" alt="" />
      <p>Hit <strong>Send</strong> to see the response here.</p>
    </div>
  </section>
</template>

<style scoped>
.viewer {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 0;
}
.status {
  display: flex;
  align-items: center;
  gap: 10px;
}
.status button {
  border: 0;
  background: transparent;
  padding: 6px 10px;
  border-radius: 6px;
  color: var(--muted);
  cursor: pointer;
}
.status button.on {
  background: var(--surface-2);
  color: var(--text);
  font-weight: 600;
}
.spacer {
  flex: 1;
}
.history {
  border: 1px solid var(--border);
  background: var(--surface);
  border-radius: 6px;
  padding: 4px 6px;
  font-size: 12px;
  max-width: 220px;
}
.status button.bad {
  color: var(--m-delete);
}
.list {
  list-style: none;
}
.list li {
  padding: 2px 0;
}
.list .pass span {
  color: var(--ok);
}
.list .fail,
.list .fail span {
  color: var(--m-delete);
}
.list em {
  color: var(--muted);
  font-style: normal;
}
.script-errors {
  margin: 6px 0 0;
  padding-left: 18px;
  color: var(--m-delete);
  font-family: var(--mono);
  font-size: 12px;
}
.muted {
  color: var(--muted);
  font-size: 12px;
}
.pill {
  padding: 3px 10px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
}
.s2 { background: rgba(31, 157, 104, 0.14); color: var(--ok); }
.s3 { background: rgba(47, 125, 225, 0.14); color: var(--m-put); }
.s4 { background: rgba(217, 130, 43, 0.16); color: var(--m-post); }
.s5 { background: rgba(224, 70, 79, 0.14); color: var(--m-delete); }
.code {
  flex: 1;
  margin: 0;
  padding: 12px;
  border-radius: 8px;
  background: var(--surface-2);
  font: 13px/1.6 var(--mono);
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-word;
}
.table {
  display: block;
  border-collapse: collapse;
}
.table td {
  padding: 2px 16px 2px 0;
  vertical-align: top;
}
.table td:first-child {
  color: var(--muted);
  white-space: nowrap;
}
.placeholder {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--muted);
}
.placeholder img {
  opacity: 0.8;
}
.spinner {
  width: 22px;
  height: 22px;
  border: 3px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
.error {
  border-left: 3px solid var(--m-delete);
  padding: 4px 12px;
}
.error .tip {
  font-family: var(--sans);
}
.error p {
  margin: 6px 0 0;
  font-family: var(--mono);
  font-size: 13px;
  color: var(--muted);
}
.attrs {
  color: var(--muted);
  font-style: normal;
}
.views {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-bottom: 6px;
}
.views button {
  border: 0;
  background: transparent;
  padding: 3px 9px;
  border-radius: 6px;
  color: var(--muted);
  cursor: pointer;
  font-size: 12px;
}
.views button.on {
  background: var(--surface-2);
  color: var(--text);
  font-weight: 600;
}
.views .saved {
  flex: 1;
  text-align: right;
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.save {
  border: 1px solid var(--border) !important;
  border-radius: 6px;
  background: transparent;
  color: var(--text) !important;
  padding: 3px 9px;
  cursor: pointer;
}
.preview {
  flex: 1;
  min-height: 200px;
  border-radius: 8px;
  background: var(--surface-2);
}
.preview.frame {
  width: 100%;
  border: 0;
  /* Pages are made for a white background. */
  background: #fff;
}
.preview.image {
  display: grid;
  place-items: center;
  overflow: auto;
  padding: 12px;
  background-image: linear-gradient(45deg, rgba(128, 128, 128, 0.12) 25%, transparent 25%),
    linear-gradient(-45deg, rgba(128, 128, 128, 0.12) 25%, transparent 25%);
  background-size: 16px 16px;
}
.preview.image img {
  max-width: 100%;
  max-height: 100%;
}
.preview.binary {
  display: flex;
  flex-direction: column;
  gap: 8px;
  align-items: flex-start;
  padding: 14px;
  font-size: 13px;
  color: var(--muted);
}
.preview.binary p {
  margin: 0;
}
</style>
