<script setup lang="ts">
import { computed } from "vue";
import type { Header } from "../core";
import VarInput from "./VarInput.vue";

/** Edits a list of headers. Inherited ones (from folders) are shown above, read-only. */
const props = defineProps<{ headers: Header[]; inherited?: { header: Header; from: string }[] }>();

const same = (a: Header, b: Header) => a.name.trim().toLowerCase() === b.name.trim().toLowerCase();

/** Inherited headers and whether an own header (enabled or not) replaces them. */
const shownInherited = computed(() =>
  (props.inherited ?? []).map((i) => ({ ...i, replaced: props.headers.find((h) => same(h, i.header)) })),
);

function add(header: Header = { name: "", value: "", enabled: true }) {
  props.headers.push({ ...header });
}

function remove(index: number) {
  props.headers.splice(index, 1);
}
</script>

<template>
  <div class="headers">
    <template v-if="shownInherited.length">
      <p class="head">Inherited</p>
      <div v-for="i in shownInherited" :key="i.header.name" class="row inherited" :class="{ replaced: i.replaced }">
        <span class="lock" :title="`Set by ${i.from}`">↑</span>
        <code class="name">{{ i.header.name }}</code>
        <code class="value">{{ i.header.value }}</code>
        <span class="from">
          <template v-if="!i.replaced">
            {{ i.from }} ·
            <button class="link" title="Add your own value for this header" @click="add(i.header)">override</button>
            ·
            <button class="link" title="Do not send it from this request" @click="add({ ...i.header, enabled: false })">
              stop
            </button>
          </template>
          <template v-else-if="i.replaced.enabled">replaced below</template>
          <template v-else>stopped below</template>
        </span>
      </div>
      <p class="head">Own</p>
    </template>

    <div v-for="(header, i) in headers" :key="i" class="row">
      <input v-model="header.enabled" type="checkbox" :title="header.enabled ? 'Sent' : 'Not sent'" />
      <input v-model="header.name" placeholder="Name" spellcheck="false" />
      <VarInput v-model="header.value" placeholder="Value" :aria-label="`Value of ${header.name || 'header'}`" />
      <button class="icon" title="Remove" @click="remove(i)">✕</button>
    </div>
    <button class="link add" @click="add()">+ Add header</button>
    <slot />
  </div>
</template>

<style scoped>
.headers {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.head {
  margin: 4px 0 0;
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}
.row {
  display: grid;
  grid-template-columns: 20px 1fr 2fr 28px;
  gap: 6px;
  align-items: center;
}
.row.inherited {
  grid-template-columns: 20px minmax(0, 1fr) minmax(0, 1.4fr) minmax(0, 1fr);
  font-size: 13px;
  color: var(--muted);
}
.row.inherited.replaced .name,
.row.inherited.replaced .value {
  text-decoration: line-through;
  opacity: 0.6;
}
.lock {
  text-align: center;
}
.name,
.value {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.from {
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row input:not([type="checkbox"]) {
  min-width: 0;
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
.icon {
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}
.link {
  border: 0;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  padding: 0;
  font-size: inherit;
}
.link.add {
  align-self: flex-start;
  padding: 6px 4px;
}
</style>
