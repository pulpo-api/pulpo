<script setup lang="ts">
import { computed, ref } from "vue";
import type { Header } from "../core";
import type { AutoHeader } from "../autoHeaders";
import VarInput from "./VarInput.vue";

/** Edits a list of headers. Inherited ones (from folders) are shown above, read-only. */
const props = defineProps<{
  headers: Header[];
  inherited?: { header: Header; from: string }[];
  /** What Pulpo adds by itself (requests only). */
  automatic?: AutoHeader[];
}>();
const emit = defineEmits<{ stopCookies: [] }>();

const same = (a: Header, b: Header) => a.name.trim().toLowerCase() === b.name.trim().toLowerCase();

/** Inherited headers and whether an own header (enabled or not) replaces them. */
const shownInherited = computed(() =>
  (props.inherited ?? []).map((i) => ({ ...i, replaced: props.headers.find((h) => same(h, i.header)) })),
);

/** Automatic headers are hidden until asked for, as in Postman: they are noise most days. */
const showAutomatic = ref(false);

const shownAutomatic = computed(() =>
  (props.automatic ?? []).map((a) => {
    const own = props.headers.find((h) => same(h, { name: a.name, value: "", enabled: true }));
    const state =
      own?.enabled && a.override ? "replaced" : own && !own.enabled && a.stop === "header" ? "stopped" : "sent";
    return { ...a, state };
  }),
);
const sentAutomatic = computed(() => shownAutomatic.value.filter((a) => a.state === "sent").length);

function overrideAuto(a: AutoHeader) {
  // Descriptions like "calculated when sending" are not values to start from.
  const real = !/ when |\(|…/.test(a.value);
  add({ name: a.name, value: real ? a.value : "", enabled: true });
}

function stopAuto(a: AutoHeader) {
  if (a.stop === "cookies") emit("stopCookies");
  else add({ name: a.name, value: "", enabled: false });
}

function add(header: Header = { name: "", value: "", enabled: true }) {
  props.headers.push({ ...header });
}

function remove(index: number) {
  props.headers.splice(index, 1);
}
</script>

<template>
  <div class="headers">
    <template v-if="shownAutomatic.length">
      <button class="head toggle-auto" :aria-expanded="showAutomatic" @click="showAutomatic = !showAutomatic">
        {{ showAutomatic ? "▾" : "▸" }} Automatic ({{ sentAutomatic }})
        <span class="hint">added when sending</span>
      </button>
      <template v-if="showAutomatic">
        <div v-for="a in shownAutomatic" :key="a.name" class="row inherited" :class="{ replaced: a.state !== 'sent' }">
          <span class="lock" :title="`Added by ${a.from}`">⚙</span>
          <code class="name">{{ a.name }}</code>
          <code class="value" :title="a.value">{{ a.value }}</code>
          <span class="from">
            <template v-if="a.state === 'replaced'">replaced below</template>
            <template v-else-if="a.state === 'stopped'">stopped below</template>
            <template v-else>
              {{ a.from }}
              <template v-if="a.override">
                · <button class="link" title="Send your own value instead" @click="overrideAuto(a)">override</button>
              </template>
              <template v-if="a.stop">
                · <button class="link" :title="a.stop === 'cookies' ? 'Turn the cookie jar off for this request' : 'Do not send it'" @click="stopAuto(a)">stop</button>
              </template>
            </template>
          </span>
        </div>
      </template>
    </template>

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
.toggle-auto {
  display: flex;
  align-items: baseline;
  gap: 8px;
  border: 0;
  background: transparent;
  padding: 0;
  cursor: pointer;
  text-align: left;
}
.toggle-auto .hint {
  text-transform: none;
  letter-spacing: 0;
  font-weight: 400;
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
