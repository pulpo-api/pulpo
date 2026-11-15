<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref } from "vue";
import type { HttpRequest } from "../core";
import { backend, type Scripts, type SseEvent, type SseStream } from "../workspace";

const props = defineProps<{
  request: HttpRequest;
  scripts: Scripts;
  requestId: string;
  name: string;
  root: string;
  environmentId: string;
}>();

const events = ref<SseEvent[]>([]);
const state = ref<"idle" | "starting" | "on">("idle");
const problem = ref("");
const filter = ref("");
const open = ref<Set<number>>(new Set());
const list = ref<HTMLElement | null>(null);
let stream: SseStream | null = null;

const shown = computed(() => {
  const q = filter.value.trim().toLowerCase();
  return q ? events.value.filter((e) => `${e.event} ${e.data}`.toLowerCase().includes(q)) : events.value;
});
const count = computed(() => events.value.filter((e) => e.event !== "info" && e.event !== "error").length);
const names = computed(() => [...new Set(events.value.filter((e) => e.event !== "info").map((e) => e.event))]);

function onEvent(event: SseEvent) {
  events.value.push(event);
  if (event.event === "error" || (event.event === "info" && !event.data.startsWith("Listening"))) {
    state.value = "idle";
    stream = null;
  }
  void nextTick(() => list.value?.scrollTo({ top: list.value.scrollHeight }));
}

async function listen() {
  if (!backend.sseListen) return;
  problem.value = "";
  state.value = "starting";
  try {
    stream = await backend.sseListen(
      props.root,
      { id: props.requestId, name: props.name, protocol: "sse", request: props.request, scripts: props.scripts },
      props.environmentId,
      onEvent,
    );
    state.value = "on";
  } catch (e) {
    problem.value = String(e);
    state.value = "idle";
  }
}

async function stop() {
  await stream?.stop();
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

onUnmounted(() => void stream?.stop());
</script>

<template>
  <section class="card sse">
    <div class="top">
      <button v-if="state !== 'on'" class="primary" :disabled="state === 'starting' || !backend.sseListen" @click="listen">
        {{ state === "starting" ? "Connecting…" : "Listen" }}
      </button>
      <button v-else class="ghost danger" @click="stop">Stop</button>
      <span class="state" :class="state">● {{ state === "on" ? "Listening" : state === "starting" ? "Connecting" : "Not listening" }}</span>
      <span class="muted small">{{ count }} events<template v-if="names.length"> · {{ names.join(", ") }}</template></span>
      <span class="spacer"></span>
      <input v-model="filter" class="filter" placeholder="Filter events" spellcheck="false" />
      <button class="ghost" :disabled="!events.length" @click="(events = []), (open = new Set())">Clear</button>
    </div>
    <p v-if="problem" class="warn">{{ problem }}</p>
    <ol ref="list" class="events" aria-label="Events">
      <li v-if="!events.length" class="empty muted">
        Listen to receive the server's events as they come (the request asks for <code>Accept: text/event-stream</code>).
        <code>pulpo run</code> collects them for tests: <code>pm.response.json()</code> is the list.
      </li>
      <li v-for="(e, i) in shown" :key="i" :class="e.event === 'info' || e.event === 'error' ? e.event : 'data'" @click="toggle(i)">
        <span class="time">{{ (e.atMs / 1000).toFixed(2) }}s</span>
        <span v-if="e.event !== 'info' && e.event !== 'error'" class="name" :title="e.id ? `id ${e.id}` : ''">{{ e.event }}</span>
        <span v-else></span>
        <pre v-if="open.has(i)" class="text">{{ pretty(e.data) }}</pre>
        <span v-else class="text one">{{ e.data }}</span>
      </li>
    </ol>
  </section>
</template>

<style scoped>
.sse {
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
.state.on {
  color: var(--m-get);
}
.state.starting {
  color: var(--m-put);
}
.filter {
  width: 180px;
}
.events {
  list-style: none;
  margin: 0;
  padding: 6px;
  border: 1px solid var(--border);
  border-radius: 8px;
  min-height: 200px;
  max-height: 460px;
  overflow: auto;
  font: 12px/1.5 var(--mono);
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.events li {
  display: grid;
  grid-template-columns: 52px auto minmax(0, 1fr);
  gap: 8px;
  padding: 2px 4px;
  border-radius: 4px;
  cursor: pointer;
}
.events li:hover {
  background: var(--surface-2);
}
.events li.info {
  color: var(--muted);
}
.events li.error {
  color: var(--m-delete);
}
.time {
  color: var(--muted);
}
.name {
  color: var(--p-sse, var(--accent));
  font-weight: 600;
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
