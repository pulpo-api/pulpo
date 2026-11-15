<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref } from "vue";
import type { HttpRequest } from "../core";
import { backend, type Scripts, type WsConnection, type WsFrame } from "../workspace";
import CodeEditor from "./CodeEditor.vue";
import { connectPacket, emitPacket, parsePacket, socketUrl } from "../socketio";

const props = defineProps<{
  request: HttpRequest;
  scripts: Scripts;
  requestId: string;
  name: string;
  root: string;
  environmentId: string;
}>();

/** A line of the log; Socket.IO events carry their name. */
type Line = WsFrame & { event?: string };
const frames = ref<Line[]>([]);
/** Socket.IO mode: the WebSocket carries Engine.IO/Socket.IO packets. */
const sio = computed(() => props.request.socketio ?? null);
const draftEvent = ref("message");
let earlyPackets: string[] = [];
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

/** Socket.IO: answers the protocol (connect, pong) and shows events, not raw packets. */
function onSocketIo(frame: WsFrame) {
  const target = sio.value!;
  const reply = (text: string) => {
    if (connection) void connection.send(text);
    else earlyPackets.push(text);
  };
  if (frame.direction === "info" || frame.direction === "error") return onFrame(frame);
  const packet = parsePacket(frame.text);
  if (frame.direction === "sent") {
    if (packet.kind === "event") {
      onFrame({ ...frame, event: packet.event, text: packet.args.length === 1 ? JSON.stringify(packet.args[0]) : JSON.stringify(packet.args) });
    }
    return;
  }
  switch (packet.kind) {
    case "open":
      reply(connectPacket(target));
      break;
    case "ping":
      reply("3");
      break;
    case "connected":
      onFrame({ direction: "info", text: `Joined namespace ${target.namespace || "/"}`, atMs: frame.atMs });
      break;
    case "connectError":
      onFrame({ direction: "error", text: `The server refused the connection: ${packet.message}`, atMs: frame.atMs });
      void connection?.close();
      break;
    case "disconnected":
      onFrame({ direction: "info", text: "The server left the namespace", atMs: frame.atMs });
      break;
    case "event":
      onFrame({
        ...frame,
        event: packet.event,
        text: packet.args.length === 1 ? JSON.stringify(packet.args[0]) : JSON.stringify(packet.args),
      });
      break;
    default:
      break;
  }
}

function onFrame(frame: Line) {
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
    earlyPackets = [];
    const request = sio.value
      ? { ...props.request, url: socketUrl(props.request.url, sio.value.path || "/socket.io"), socketio: null, messages: [] }
      : props.request;
    connection = await backend.wsConnect(
      props.root,
      { id: props.requestId, name: props.name, protocol: "websocket", request, scripts: props.scripts },
      props.environmentId,
      sio.value ? onSocketIo : onFrame,
    );
    // Packets the protocol asked for before the connection was ready.
    for (const text of earlyPackets) await connection.send(text);
    earlyPackets = [];
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
  await send(sio.value ? emitPacket(sio.value.namespace, draftEvent.value.trim() || "message", draft.value) : draft.value);
}

function sendSaved(name: string, body: string) {
  void send(sio.value ? emitPacket(sio.value.namespace, name, body) : body);
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
        @click="sendSaved(m.name, m.body)"
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
        <pre v-if="expanded.has(i)" class="text"><b v-if="f.event" class="event">{{ f.event }} </b>{{ pretty(f.text) }}</pre>
        <span v-else class="text one"><b v-if="f.event" class="event">{{ f.event }} </b>{{ f.text }}</span>
      </li>
    </ol>

    <div class="composer" :class="{ sio }">
      <input v-if="sio" v-model="draftEvent" class="event-name" placeholder="event" spellcheck="false" aria-label="Event name" />
      <CodeEditor
        v-model="draft"
        variables
        class="code-area"
        :language="/^\s*[[{]/.test(draft) ? 'json' : 'text'"
        :placeholder="sio ? 'Event data: JSON or text ({{variables}} work). Ctrl+Enter emits.' : 'Message to send ({{variables}} work). Ctrl+Enter sends.'"
        @run="sendDraft"
      />
      <button class="primary" :disabled="state !== 'open' || (!sio && !draft.trim())" @click="sendDraft">{{ sio ? "Emit" : "Send" }}</button>
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
.composer.sio {
  grid-template-columns: 140px minmax(0, 1fr) auto;
}
.event-name {
  align-self: stretch;
}
.event {
  color: var(--p-websocket, var(--accent));
  margin-right: 8px;
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
