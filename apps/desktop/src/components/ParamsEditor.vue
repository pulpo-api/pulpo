<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { HttpRequest } from "../core";
import { joinUrl, pathParamNames, splitUrl } from "../params";

const props = defineProps<{ request: HttpRequest }>();

interface Row {
  name: string;
  value: string;
  enabled: boolean;
}

const rows = ref<Row[]>([]);
let written = "";

function stateKey() {
  return JSON.stringify([props.request.url, props.request.disabledParams]);
}

/** Rebuilds the table when the URL or the disabled params change from outside. */
function load() {
  const { params } = splitUrl(props.request.url);
  rows.value = [
    ...params.map((p) => ({ ...p, enabled: true })),
    ...props.request.disabledParams.map((p) => ({ ...p, enabled: false })),
  ];
}

watch(stateKey, (key) => {
  if (key !== written) load();
}, { immediate: true });

/** Writes the table back: enabled rows into the URL, disabled ones aside. */
function write() {
  const { base, hash } = splitUrl(props.request.url);
  props.request.url = joinUrl(
    base,
    rows.value.filter((r) => r.enabled),
    hash,
  );
  props.request.disabledParams = rows.value
    .filter((r) => !r.enabled && (r.name || r.value))
    .map(({ name, value }) => ({ name, value }));
  written = stateKey();
}

function add() {
  rows.value.push({ name: "", value: "", enabled: true });
}

function remove(index: number) {
  rows.value.splice(index, 1);
  write();
}

const duplicates = computed(() => {
  const seen = new Set<string>();
  const dup = new Set<string>();
  for (const r of rows.value.filter((r) => r.enabled && r.name)) {
    if (seen.has(r.name)) dup.add(r.name);
    seen.add(r.name);
  }
  return dup;
});

// ------------------------------------------------------------------ path params

const pathNames = computed(() => pathParamNames(props.request.url));

function pathValue(name: string): string {
  return props.request.pathParams.find((p) => p.name === name)?.value ?? "";
}

function setPathValue(name: string, value: string) {
  const existing = props.request.pathParams.find((p) => p.name === name);
  if (existing) existing.value = value;
  else props.request.pathParams.push({ name, value });
}

// Drop values of segments the user removed from the URL (not on first load).
watch(pathNames, (names) => {
  const kept = props.request.pathParams.filter((p) => names.includes(p.name));
  if (kept.length !== props.request.pathParams.length) props.request.pathParams = kept;
});
</script>

<template>
  <div class="params">
    <div class="head">
      <span>Query params</span>
      <span class="hint">Enabled params live in the URL; disabled ones are kept aside and not sent.</span>
    </div>
    <div v-for="(row, i) in rows" :key="i" class="row">
      <input v-model="row.enabled" type="checkbox" :title="row.enabled ? 'Sent' : 'Not sent'" @change="write" />
      <input
        v-model="row.name"
        :class="{ bad: row.enabled && duplicates.has(row.name) }"
        placeholder="name"
        spellcheck="false"
        @input="write"
      />
      <input v-model="row.value" placeholder="value" spellcheck="false" @input="write" />
      <button class="icon" title="Remove" @click="remove(i)">✕</button>
    </div>
    <button class="link" @click="add">+ Add param</button>
    <p v-if="duplicates.size" class="warn">
      Repeated: <code v-for="name in duplicates" :key="name">{{ name }}</code>. Valid in HTTP (e.g. <code>tag=a&amp;tag=b</code>), but often a mistake.
    </p>

    <template v-if="pathNames.length">
      <div class="head path">
        <span>Path params</span>
        <span class="hint">Values for the <code>:name</code> segments of the URL. They can use <code v-pre>{{variables}}</code>.</span>
      </div>
      <div v-for="name in pathNames" :key="name" class="row path-row">
        <code class="name">:{{ name }}</code>
        <input
          :value="pathValue(name)"
          placeholder="value"
          spellcheck="false"
          @input="setPathValue(name, ($event.target as HTMLInputElement).value)"
        />
      </div>
    </template>
  </div>
</template>

<style scoped>
.params {
  container-type: inline-size;
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-height: 160px;
}
.head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}
.head.path {
  margin-top: 10px;
}
@container (max-width: 520px) {
  .hint {
    display: none;
  }
}
.hint {
  text-transform: none;
  letter-spacing: 0;
  font-weight: 400;
}
.row {
  display: grid;
  grid-template-columns: 20px 1fr 2fr 28px;
  gap: 6px;
  align-items: center;
}
.path-row {
  grid-template-columns: minmax(80px, 1fr) 2fr;
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
  border-color: var(--m-post);
}
.name {
  font-size: 13px;
  color: var(--m-patch);
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
.warn {
  margin: 0;
  font-size: 12px;
  color: var(--m-post);
}
.warn code {
  margin: 0 3px;
}
</style>
