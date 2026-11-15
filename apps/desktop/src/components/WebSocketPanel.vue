<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref } from "vue";
import type { HttpRequest } from "../core";
import { backend, type Scripts, type WsConnection, type WsFrame } from "../workspace";
import CodeEditor from "./CodeEditor.vue";

const props = defineProps<{
  request: HttpRequest;
  scripts: Scripts;
  requestId: string;
  name: string;
  root: string;
  environmentId: string;
}>();

const frames = ref<WsFrame[]>([]);
const state = ref<"closed" | "connecting" | "open" | "closing">("closed");
const problem = ref("");
const draft = ref("");
const filter = ref("");
const log = ref<HTMLElement | null>(null);
let connection: WsConnection | null = null;

const saved = computed(() => props.request.messages ?? []);
const shown = computed(() => {
  const q = filter.value.trim().toLowerCase();
  return q ? frames.value.filter((f) => f.text.toLowerCase().includes(q)) : frames.value;
});
const counts = computed(() => ({
  sent: frames.value.filter((f) => f.direction === "sent").length,
  received: frames.value.filter((f) => f.direction === "received").length,
}));

function onFrame(frame: WsFrame) {
  frames.value.push(frame);
  if (frame.direction === "error" || (frame.direction === "info" && frame.text.startsWith("Closed"))) {
    state.value = "closed";
    connection = null;
  }
  void nextTick(() => log.value?.scrollTo({ top: log.value.scrollHeight }));
}

async function connect() {
  if (!backend.wsConnect) return;
  problem.value = "";
  state.value = "connecting";
  try {
    connection = await backend.wsConnect(
      props.root,
      { id: props.requestId, name: props.name, protocol: "websocket", request: props.request, scripts: props.scripts },
      props.environmentId,
      onFrame,
    );
    state.value = "open";
  } catch (e) {
    problem.value = String(e);
    state.value = "closed";
  }
}

async function disconnect() {
  // The server confirms the close; until then the line is closing.
  state.value = "closing";
  await connection?.close();
}

async function send(text: string) {
  if (!connection || !text.trim()) return;
  problem.value = "";
  try {
    await connection.send(text);
  } catch (e) {
    problem.value = String(e);
  }
}

async function sendDraft() {
  await send(draft.value);
}

function pretty(text: string) {
  if (!/^\s*[[{]/.test(text)) return text;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

const expanded = ref<Set<number>>(new Set());
function toggle(i: number) {
  const next = new Set(expanded.value);
  if (next.has(i)) next.delete(i);
  else next.add(i);
  expanded.value = next;
}

onUnmounted(() => void connection?.close());
</script>

<template>
  <section class="card ws">
    <div class="top">
      <button v-if="state === 'closed' || state === 'connecting'" class="primary" :disabled="state === 'connecting' || !backend.wsConnect" @click="connect">
        {{ state === "connecting" ? "Connecting…" : "Connect" }}
      </button>
      <button v-else class="ghost danger" :disabled="state === 'closing'" @click="disconnect">
        {{ state === "closing" ? "Closing…" : "Disconnect" }}
      </button>
      <span class="state" :class="state">● {{ { open: "Connected", connecting: "Connecting", closing: "Closing", closed: "Not connected" }[state] }}</span>
      <span class="muted small">↑ {{ counts.sent }} · ↓ {{ counts.received }}</span>
      <span class="spacer"></span>
      <input v-model="filter" class="filter" placeholder="Filter messages" spellcheck="false" />
      <button class="ghost" :disabled="!frames.length" @click="(frames = []), (expanded = new Set())">Clear</button>
    </div>
    <p v-if="problem" class="warn">{{ problem }}</p>

    <div v-if="saved.length" class="saved">
      <span class="muted small">Saved:</span>
      <button
        v-for="(m, i) in saved"
        :key="i"
        class="chip"
        :disabled="state !== 'open'"
        :title="m.body"
        @click="send(m.body)"
      >
        ↑ {{ m.name }}
      </button>
    </div>

    <ol ref="log" class="log" aria-label="Messages">
      <li v-if="!frames.length" class="empty muted">
        Connect to start. Messages you send and receive show up here; JSON is formatted when you open it.
      </li>
      <li v-for="(f, i) in shown" :key="i" class="frame" :class="f.direction" @click="toggle(i)">
        <span class="arrow">{{ f.direction === "sent" ? "↑" : f.direction === "received" ? "↓" : f.direction === "error" ? "!" : "·" }}</span>
        <span class="time">{{ (f.atMs / 1000).toFixed(2) }}s</span>
        <pre v-if="expanded.has(i)" class="text">{{ pretty(f.text) }}</pre>
        <span v-else class="text one">{{ f.text }}</span>
      </li>
    </ol>

    <div class="composer">
      <CodeEditor
        v-model="draft"
        variables
        class="code-area"
        :language="/^\s*[[{]/.test(draft) ? 'json' : 'text'"
        placeholder="Message to send ({{variables}} work). Ctrl+Enter sends."
        @run="sendDraft"
      />
      <button class="primary" :disabled="state !== 'open' || !draft.trim()" @click="sendDraft">Send</button>
    </div>
  </section>
</template>

<style scoped>
.ws {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 0;
  container-type: inline-size;
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
.state.connecting,
.state.closing {
  color: var(--m-put);
}
.filter {
  width: 180px;
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
  max-height: 360px;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
  font: 12px/1.5 var(--mono);
}
.frame {
  display: grid;
  grid-template-columns: 14px 52px minmax(0, 1fr);
  gap: 6px;
  padding: 2px 4px;
  border-radius: 4px;
  cursor: pointer;
}
.frame:hover {
  background: var(--surface-2);
}
.frame.sent .arrow {
  color: var(--m-post);
}
.frame.received .arrow {
  color: var(--m-get);
}
.frame.info {
  color: var(--muted);
}
.frame.error {
  color: var(--m-delete);
}
.time {
  color: var(--muted);
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
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 8px;
  align-items: end;
}
.composer .code-area {
  min-height: 70px;
  max-height: 180px;
}
.empty {
  padding: 8px;
  font-family: inherit;
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
