<script setup lang="ts">
import { computed, ref } from "vue";
import type { HttpRequest } from "../core";
import { backend, type GrpcMethod, type GrpcSchema, type Scripts } from "../workspace";
import CodeEditor from "./CodeEditor.vue";
import WsMessagesEditor from "./WsMessagesEditor.vue";

const props = defineProps<{
  request: HttpRequest;
  root: string;
  requestId: string;
  scripts: Scripts;
  environmentId: string;
}>();
const emit = defineEmits<{ send: []; save: [] }>();

const target = computed(() => {
  if (!props.request.grpc) props.request.grpc = { method: "", protoFiles: [] };
  return props.request.grpc;
});
const body = computed({
  get: () => props.request.body ?? "",
  set: (v: string) => {
    props.request.body = v;
  },
});
const protoFiles = computed({
  get: () => (target.value.protoFiles ?? []).join(", "),
  set: (v: string) => {
    target.value.protoFiles = v
      .split(",")
      .map((f) => f.trim())
      .filter(Boolean);
  },
});

const schema = ref<GrpcSchema | null>(null);
const loading = ref(false);
const problem = ref("");
const canLoad = computed(() => !!backend.grpcSchema && !!props.root);

const services = computed(() => {
  const groups = new Map<string, GrpcMethod[]>();
  for (const m of schema.value?.methods ?? []) {
    groups.set(m.service, [...(groups.get(m.service) ?? []), m]);
  }
  return [...groups.entries()];
});
const current = computed(() => schema.value?.methods.find((m) => m.path === target.value.method) ?? null);

function kind(m: GrpcMethod) {
  if (m.clientStreaming && m.serverStreaming) return "bidi streaming";
  if (m.clientStreaming) return "client streaming";
  if (m.serverStreaming) return "server streaming";
  return "unary";
}

async function load() {
  if (!backend.grpcSchema) return;
  loading.value = true;
  problem.value = "";
  try {
    schema.value = await backend.grpcSchema(
      props.root,
      { id: props.requestId, name: "", protocol: "grpc", request: props.request, scripts: props.scripts },
      props.environmentId,
    );
    if (!target.value.method && schema.value.methods.length === 1) pick(schema.value.methods[0].path);
  } catch (e) {
    problem.value = String(e);
  }
  loading.value = false;
}

function pick(path: string) {
  target.value.method = path;
  const m = schema.value?.methods.find((x) => x.path === path);
  // A new method starts from its example unless a message is already written.
  if (m && !body.value.trim()) body.value = m.example;
}

function useExample() {
  if (current.value) body.value = current.value.example;
}
</script>

<template>
  <div class="grpc">
    <p v-if="!canLoad" class="note">
      gRPC calls are made by the desktop app: a browser can only speak gRPC-Web. You can still write the method and the
      message here.
    </p>
    <div class="row">
      <label class="field">
        <span>Method</span>
        <select v-if="schema" :value="target.method" aria-label="Method" @change="pick(($event.target as HTMLSelectElement).value)">
          <option value="" disabled>Pick a method</option>
          <optgroup v-for="[service, list] in services" :key="service" :label="service">
            <option v-for="m in list" :key="m.path" :value="m.path">{{ m.name }} · {{ kind(m) }}</option>
          </optgroup>
        </select>
        <input v-else v-model="target.method" placeholder="package.Service/Method" spellcheck="false" />
      </label>
      <button v-if="canLoad" class="ghost" :disabled="loading" @click="load">
        {{ loading ? "Reading…" : schema ? "↻ Methods" : "Load methods" }}
      </button>
    </div>
    <label class="field">
      <span>.proto files</span>
      <input
        v-model="protoFiles"
        placeholder="empty: ask the server (reflection) · or protos/orders.proto, protos/common.proto"
        spellcheck="false"
      />
    </label>
    <p v-if="problem" class="warn">{{ problem }}</p>
    <p v-if="schema" class="muted small">
      {{ schema.methods.length }} methods from {{ schema.source }}<template v-if="current">
        · {{ current.input }} → {{ current.output }} · {{ kind(current) }}</template
      >
    </p>

    <div class="label-row">
      <span class="label">Message (JSON)</span>
      <button v-if="current" class="link" @click="useExample">Use example</button>
    </div>
    <CodeEditor
      v-model="body"
      variables
      class="code-area"
      language="json"
      placeholder='{ "id": "{{orderId}}" }'
      @run="emit('send')"
      @save="emit('save')"
    />
    <template v-if="current?.clientStreaming">
      <p class="muted small">
        Client streaming: the saved messages below are sent in order (if there are none, the message above).
      </p>
      <WsMessagesEditor :request="request" @save="emit('save')" />
    </template>
    <p class="hint">
      Headers go as metadata. The reply comes back as JSON (a list when the server streams); errors show their gRPC code.
    </p>
  </div>
</template>

<style scoped>
.grpc {
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
.field select,
.field input {
  width: 100%;
  box-sizing: border-box;
}
.label-row {
  display: flex;
  gap: 8px;
  align-items: baseline;
}
.label {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}
.code-area {
  min-height: 180px;
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
