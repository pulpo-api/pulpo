<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref } from "vue";
import type { HttpRequest } from "../core";
import { backend, type MqttMessage, type MqttSession, type Scripts } from "../workspace";
import CodeEditor from "./CodeEditor.vue";

const props = defineProps<{
  request: HttpRequest;
  scripts: Scripts;
  requestId: string;
  name: string;
  root: string;
  environmentId: string;
}>();

const lines = ref<MqttMessage[]>([]);
const state = ref<"closed" | "connecting" | "open">("closed");
const problem = ref("");
const filter = ref("");
const topic = ref("");
const payload = ref("");
const qos = ref(props.request.mqtt?.qos ?? 0);
const retain = ref(props.request.mqtt?.retain ?? false);
const open = ref<Set<number>>(new Set());
const log = ref<HTMLElement | null>(null);
let session: MqttSession | null = null;

const saved = computed(() => props.request.messages ?? []);
const shown = computed(() => {
  const q = filter.value.trim().toLowerCase();
  return q ? lines.value.filter((l) => `${l.topic} ${l.payload}`.toLowerCase().includes(q)) : lines.value;
});
const counts = computed(() => ({
  sent: lines.value.filter((l) => l.direction === "sent").length,
  received: lines.value.filter((l) => l.direction === "received").length,
}));

function onMessage(m: MqttMessage) {
  lines.value.push(m);
  if (m.direction === "error" || (m.direction === "info" && /^(Disconnected|Closed)/.test(m.payload))) {
    state.value = "closed";
    session = null;
  }
  void nextTick(() => log.value?.scrollTo({ top: log.value.scrollHeight }));
}

async function connect() {
  if (!backend.mqttConnect) return;
  problem.value = "";
  state.value = "connecting";
  try {
    session = await backend.mqttConnect(
      props.root,
      { id: props.requestId, name: props.name, protocol: "mqtt", request: props.request, scripts: props.scripts },
      props.environmentId,
      onMessage,
    );
    state.value = "open";
  } catch (e) {
    problem.value = String(e);
    state.value = "closed";
  }
}

async function disconnect() {
  await session?.close();
}

async function publish(t: string, p: string) {
  if (!session || !t.trim()) return;
  problem.value = "";
  try {
    await session.publish(t.trim(), p, qos.value, retain.value);
  } catch (e) {
    problem.value = String(e);
  }
}

function toggle(i: number) {
  const next = new Set(open.value);
  if (next.has(i)) next.delete(i);
  else next.add(i);
  open.value = next;
}

function pretty(text: string) {
  if (!/^\s*[[{]/.test(text)) return text;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

onUnmounted(() => void session?.close());
</script>

<template>
  <section class="card mqtt">
    <div class="top">
      <button v-if="state !== 'open'" class="primary" :disabled="state === 'connecting' || !backend.mqttConnect" @click="connect">
        {{ state === "connecting" ? "Connecting…" : "Connect" }}
      </button>
      <button v-else class="ghost danger" @click="disconnect">Disconnect</button>
      <span class="state" :class="state">● {{ { open: "Connected", connecting: "Connecting", closed: "Not connected" }[state] }}</span>
      <span class="muted small">↑ {{ counts.sent }} · ↓ {{ counts.received }}</span>
      <span class="spacer"></span>
      <input v-model="filter" class="filter" placeholder="Filter by topic or payload" spellcheck="false" />
      <button class="ghost" :disabled="!lines.length" @click="(lines = []), (open = new Set())">Clear</button>
    </div>
    <p v-if="problem" class="warn">{{ problem }}</p>

    <div v-if="saved.length" class="saved">
      <span class="muted small">Saved:</span>
      <button v-for="(m, i) in saved" :key="i" class="chip" :disabled="state !== 'open'" :title="m.body" @click="publish(m.name, m.body)">
        ↑ {{ m.name }}
      </button>
    </div>

    <ol ref="log" class="log" aria-label="Messages">
      <li v-if="!lines.length" class="empty muted">
        Connect to subscribe to the topics in <b>Topics</b>; messages and what you publish show up here.
      </li>
      <li v-for="(l, i) in shown" :key="i" class="line" :class="l.direction" @click="toggle(i)">
        <span class="arrow">{{ l.direction === "sent" ? "↑" : l.direction === "received" ? "↓" : l.direction === "error" ? "!" : "·" }}</span>
        <span class="time">{{ (l.atMs / 1000).toFixed(2) }}s</span>
        <span class="topic">{{ l.topic }}<span v-if="l.retain" class="flag" title="Retained">R</span></span>
        <pre v-if="open.has(i)" class="text">{{ pretty(l.payload) }}</pre>
        <span v-else class="text one">{{ l.payload }}</span>
      </li>
    </ol>

    <div class="composer">
      <input v-model="topic" class="topic-input" placeholder="topic (orders/{{orderId}})" spellcheck="false" aria-label="Topic" />
      <CodeEditor v-model="payload" variables class="code-area" :language="/^\s*[[{]/.test(payload) ? 'json' : 'text'" placeholder="Payload. Ctrl+Enter publishes." @run="publish(topic, payload)" />
      <div class="opts">
        <label>QoS <select v-model.number="qos"><option :value="0">0</option><option :value="1">1</option></select></label>
        <label><input v-model="retain" type="checkbox" /> Retain</label>
        <button class="primary" :disabled="state !== 'open' || !topic.trim()" @click="publish(topic, payload)">Publish</button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.mqtt {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 0;
}
.top {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.spacer {
  flex: 1;
}
.state {
  font-size: 12px;
  color: var(--muted);
}
.state.open {
  color: var(--m-get);
}
.state.connecting {
  color: var(--m-put);
}
.filter {
  width: 200px;
}
.saved {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: center;
}
.chip {
  border: 1px solid var(--border);
  background: var(--surface-2);
  color: inherit;
  border-radius: 999px;
  padding: 2px 10px;
  font-size: 12px;
  cursor: pointer;
}
.chip:disabled {
  opacity: 0.5;
  cursor: default;
}
.log {
  list-style: none;
  margin: 0;
  padding: 6px;
  border: 1px solid var(--border);
  border-radius: 8px;
  min-height: 160px;
  max-height: 380px;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
  font: 12px/1.5 var(--mono);
}
.line {
  display: grid;
  grid-template-columns: 14px 52px minmax(80px, auto) minmax(0, 1fr);
  gap: 8px;
  padding: 2px 4px;
  border-radius: 4px;
  cursor: pointer;
}
.line:hover {
  background: var(--surface-2);
}
.line.sent .arrow {
  color: var(--m-post);
}
.line.received .arrow {
  color: var(--m-get);
}
.line.info {
  color: var(--muted);
}
.line.error {
  color: var(--m-delete);
}
.time {
  color: var(--muted);
}
.topic {
  color: var(--p-mqtt, #7a9a3a);
  font-weight: 600;
}
.flag {
  margin-left: 4px;
  font-size: 10px;
  padding: 0 3px;
  border: 1px solid currentColor;
  border-radius: 3px;
}
.text {
  margin: 0;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.text.one {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.composer {
  display: grid;
  grid-template-columns: 220px minmax(0, 1fr) auto;
  gap: 8px;
  align-items: end;
}
.composer .code-area {
  min-height: 60px;
  max-height: 160px;
}
.opts {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 12px;
  color: var(--muted);
}
.empty {
  padding: 8px;
  font-family: inherit;
  display: block;
}
.muted {
  color: var(--muted);
}
.small {
  font-size: 12px;
}
.warn {
  color: var(--m-delete);
  font-size: 12px;
  margin: 0;
}
.primary,
.ghost {
  border-radius: 8px;
  padding: 6px 14px;
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
  color: inherit;
}
.danger {
  color: var(--m-delete);
}
button:disabled {
  opacity: 0.55;
  cursor: default;
}
</style>
