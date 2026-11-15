<script setup lang="ts">
import { computed, ref } from "vue";
import { checkEnvelope, type HttpRequest } from "../core";
import { backend, type SoapOperation } from "../workspace";
import CodeEditor from "./CodeEditor.vue";

const props = defineProps<{ request: HttpRequest; root: string }>();
const emit = defineEmits<{ send: []; save: [] }>();

const target = computed(() => {
  if (!props.request.soap) props.request.soap = { wsdl: "", operation: "" };
  return props.request.soap;
});
const envelope = computed({
  get: () => props.request.body ?? "",
  set: (v: string) => {
    props.request.body = v;
  },
});

const operations = ref<SoapOperation[] | null>(null);
const loading = ref(false);
const problem = ref("");
const canLoad = computed(() => !!backend.soapOperations && !!props.root);

const groups = computed(() => {
  const out = new Map<string, SoapOperation[]>();
  for (const op of operations.value ?? []) {
    const key = `${op.service} · ${op.port} (SOAP ${op.version})`;
    out.set(key, [...(out.get(key) ?? []), op]);
  }
  return [...out.entries()];
});
const current = computed(() => operations.value?.find((o) => o.path === target.value.operation) ?? null);
const issues = computed(() => checkEnvelope(envelope.value, current.value?.version, current.value?.bodyElement));

async function load() {
  if (!backend.soapOperations || !target.value.wsdl.trim()) return;
  loading.value = true;
  problem.value = "";
  try {
    operations.value = await backend.soapOperations(props.root, target.value.wsdl.trim());
  } catch (e) {
    problem.value = String(e);
  }
  loading.value = false;
}

/** Picks an operation: its headers, its address (when the URL is still empty) and, if the
 * envelope is empty, its sample envelope. */
function pick(path: string) {
  const op = operations.value?.find((o) => o.path === path);
  target.value.operation = path;
  if (!op) return;
  const r = props.request;
  const names = op.headers.map(([n]) => n.toLowerCase()).concat(["soapaction"]);
  r.headers = [
    ...r.headers.filter((h) => !names.includes(h.name.trim().toLowerCase())),
    ...op.headers.map(([name, value]) => ({ name, value, enabled: true })),
  ];
  if (!r.url.trim() || r.url.trim() === "https://") r.url = op.address;
  if (!envelope.value.trim()) envelope.value = op.envelope;
}

function useExample() {
  if (current.value) envelope.value = current.value.envelope;
}

function format() {
  const doc = new DOMParser().parseFromString(envelope.value, "application/xml");
  if (doc.getElementsByTagName("parsererror").length) return;
  const lines: string[] = [];
  const walk = (node: Element, depth: number) => {
    const pad = "   ".repeat(depth);
    const attrs = [...node.attributes].map((a) => ` ${a.name}="${a.value}"`).join("");
    const kids = [...node.childNodes].filter((n) => n.nodeType === 1 || (n.nodeType === 3 && n.textContent?.trim()) || n.nodeType === 8);
    if (!kids.length) lines.push(`${pad}<${node.tagName}${attrs}/>`);
    else if (kids.length === 1 && kids[0].nodeType === 3) lines.push(`${pad}<${node.tagName}${attrs}>${kids[0].textContent?.trim()}</${node.tagName}>`);
    else {
      lines.push(`${pad}<${node.tagName}${attrs}>`);
      for (const k of kids) {
        if (k.nodeType === 1) walk(k as Element, depth + 1);
        else if (k.nodeType === 8) lines.push(`${"   ".repeat(depth + 1)}<!--${k.textContent}-->`);
        else lines.push(`${"   ".repeat(depth + 1)}${k.textContent?.trim()}`);
      }
      lines.push(`${pad}</${node.tagName}>`);
    }
  };
  walk(doc.documentElement, 0);
  envelope.value = `${lines.join("\n")}\n`;
}
</script>

<template>
  <div class="soap">
    <div class="row">
      <label class="field">
        <span>WSDL</span>
        <input v-model="target.wsdl" :placeholder="$t('contracts/calculator.wsdl or https://host/service?wsdl')" spellcheck="false" @keydown.enter="load" />
      </label>
      <button v-if="canLoad" class="ghost" :disabled="loading || !target.wsdl.trim()" @click="load">
        {{ loading ? $t("Reading…") : operations ? "↻ Operations" : $t("Load operations") }}
      </button>
    </div>
    <p v-if="!canLoad" class="note">
      {{ $t("Reading the WSDL is done by the desktop app. Here you can still write the envelope and send it.") }}
    </p>
    <p v-if="problem" class="warn">{{ problem }}</p>
    <label v-if="operations" class="field">
      <span>{{ $t("Operation") }}</span>
      <select :value="target.operation" :aria-label="$t('Operation')" @change="pick(($event.target as HTMLSelectElement).value)">
        <option value="" disabled>{{ $t("Pick an operation") }}</option>
        <optgroup v-for="[group, list] in groups" :key="group" :label="group">
          <option v-for="op in list" :key="op.path" :value="op.path">{{ op.name }}</option>
        </optgroup>
      </select>
    </label>
    <p v-else-if="target.operation" class="muted small">{{ $t("Operation") }} <code>{{ target.operation }}</code></p>
    <p v-if="current" class="muted small">
      {{ $t("SOAP {0} · {1} · action", { 0: current.version, 1: current.style }) }} <code>{{ current.soapAction || "(none)" }}</code>
      <template v-if="current.documentation"> · {{ current.documentation }}</template>
    </p>

    <div class="label-row">
      <span class="label">{{ $t("Envelope") }}</span>
      <button v-if="current" class="link" @click="useExample">{{ $t("Use example") }}</button>
      <button class="link" @click="format">{{ $t("Format XML") }}</button>
    </div>
    <CodeEditor
      v-model="envelope"
      variables
      class="code-area"
      language="xml"
      :placeholder="$t('<soapenv:Envelope …>')"
      @run="emit('send')"
      @save="emit('save')"
    />
    <ul v-if="issues.length && envelope.trim()" class="issues">
      <li v-for="(i, n) in issues" :key="n">{{ i }}</li>
    </ul>
    <p class="hint">
      {{ $t("Picking an operation sets") }} <code>Content-Type</code> {{ $t("and") }} <code>SOAPAction</code> {{ $t("in Headers; the envelope is sent as the body, with") }} <code>{{ "{{" }}variables}}</code> {{ $t("filled in.") }}
    </p>
  </div>
</template>

<style scoped>
.soap {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 0;
}
.row {
  display: flex;
  gap: 8px;
  align-items: end;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--muted);
  flex: 1;
  min-width: 0;
}
.field input,
.field select {
  width: 100%;
  box-sizing: border-box;
}
.label-row {
  display: flex;
  gap: 12px;
  align-items: baseline;
}
.label {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}
.code-area {
  min-height: 220px;
}
.link {
  background: none;
  border: none;
  color: var(--accent);
  cursor: pointer;
  font-size: 12px;
  padding: 0;
}
.ghost {
  border: 1px solid var(--border);
  background: var(--surface);
  color: inherit;
  border-radius: 8px;
  padding: 5px 12px;
  cursor: pointer;
  white-space: nowrap;
}
.issues {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  color: var(--m-put);
}
.muted {
  color: var(--muted);
  margin: 0;
}
.small {
  font-size: 12px;
}
.note {
  font-size: 12px;
  margin: 0;
  color: var(--m-put);
}
.warn {
  color: var(--m-delete);
  font-size: 12px;
  margin: 0;
}
.hint {
  font-size: 12px;
  color: var(--muted);
  margin: 0;
}
</style>
