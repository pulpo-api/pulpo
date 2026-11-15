<script setup lang="ts">
import { computed, ref } from "vue";
import { HTTP_METHODS, bodyMode, isEventStream, isGrpcUrl, isMqtt, isWebSocketUrl, type HttpRequest } from "../core";
import type { InheritedAuth, InheritedSettings, Scripts } from "../workspace";
import type { Header } from "../core";
import HeadersEditor from "./HeadersEditor.vue";
import { automaticHeaders } from "../autoHeaders";
import VarInput from "./VarInput.vue";
import SettingsEditor from "./SettingsEditor.vue";
import ExamplesEditor from "./ExamplesEditor.vue";
import AuthEditor from "./AuthEditor.vue";
import CodeEditor from "./CodeEditor.vue";
import BodyEditor from "./BodyEditor.vue";
import ParamsEditor from "./ParamsEditor.vue";
import WsMessagesEditor from "./WsMessagesEditor.vue";
import GrpcEditor from "./GrpcEditor.vue";
import { pathParamNames, splitUrl } from "../params";
import { applyPasted, parseCurl } from "../paste";

const props = defineProps<{
  request: HttpRequest;
  scripts: Scripts;
  sending: boolean;
  supported: boolean;
  preview: { url: string; changed: boolean; missing: string[] } | null;
  inheritedAuth: InheritedAuth;
  inheritedSettings: InheritedSettings;
  inheritedHeaders: { header: Header; from: string }[];
  root: string;
  /** The request's id, for its saved examples. */
  requestId: string;
  /** For the automatic headers: Pulpo's version and the selected environment. */
  version: string;
  environmentName: string;
  /** For OAuth 2.0 tokens; "" = no environment. */
  environmentId: string;
}>();
const emit = defineEmits<{ send: []; save: [] }>();

/** A curl pasted into the URL bar fills the whole request (method, headers, body, auth). */
function pasteCurl(text: string): boolean {
  const pasted = parseCurl(text);
  if (!pasted) return false;
  applyPasted(props.request, pasted);
  tab.value = pasted.body !== null || pasted.form ? "body" : "headers";
  return true;
}

const tab = ref<"params" | "auth" | "body" | "headers" | "settings" | "pre" | "test" | "examples">(
  props.request.graphql || !(props.request.method === "GET" || splitUrl(props.request.url).params.length) ? "body" : "params",
);
/** `ws://`/`wss://`: no method or Send; the Body tab holds the saved messages. */
const websocket = computed(() => isWebSocketUrl(props.request.url));
/** MQTT: a broker session; the saved messages are publications (name = topic). */
const mqtt = computed(() => isMqtt(props.request));
function toggleMqtt() {
  props.request.mqtt = props.request.mqtt ? null : { subscribe: [], qos: 0 };
}
const subscribeText = computed({
  get: () => (props.request.mqtt?.subscribe ?? []).join(", "),
  set: (v: string) => {
    if (!props.request.mqtt) props.request.mqtt = {};
    props.request.mqtt.subscribe = v.split(",").map((t) => t.trim()).filter(Boolean);
  },
});
function ensureMqtt() {
  if (!props.request.mqtt) props.request.mqtt = { subscribe: [], qos: 0 };
  return props.request.mqtt;
}
/** Socket.IO: a WebSocket with its own protocol; the saved messages are events. */
const socketio = computed(() => !!props.request.socketio);
function toggleSocketIo() {
  props.request.socketio = props.request.socketio ? null : { namespace: "/", path: "/socket.io", auth: "" };
}
/** `grpc://`/`grpcs://`: the Body tab picks the method and holds the message. */
const grpc = computed(() => isGrpcUrl(props.request.url));
/** `Accept: text/event-stream`: listened to in the panel below instead of sent. */
const stream = computed(() => isEventStream(props.request));
function toggleStream() {
  const r = props.request;
  if (stream.value) {
    r.headers = r.headers.filter(
      (h) => !(h.name.trim().toLowerCase() === "accept" && h.value.toLowerCase().includes("text/event-stream")),
    );
  } else {
    r.headers = [...r.headers.filter((h) => h.name.trim().toLowerCase() !== "accept"), { name: "Accept", value: "text/event-stream", enabled: true }];
  }
}
const paramCount = computed(() => splitUrl(props.request.url).params.length + pathParamNames(props.request.url).length);
const BODY_BADGES: Partial<Record<ReturnType<typeof bodyMode>, string>> = {
  urlencoded: "Form",
  multipart: "Form",
  file: "File",
  graphql: "GraphQL",
  soap: "SOAP",
};
const examplesNew = ref(false);
const examplesEditor = ref<InstanceType<typeof ExamplesEditor> | null>(null);
defineExpose({
  /** Opens the Examples tab; with `startNew`, with a new example to fill in. */
  openExamples(startNew: boolean) {
    examplesNew.value = startNew;
    if (tab.value === "examples") {
      if (startNew) examplesEditor.value?.addNew();
      else void examplesEditor.value?.reload();
    }
    tab.value = "examples";
  },
});
const settingsCount = computed(() => Object.keys(props.request.settings ?? {}).length);
const bodyBadge = computed(() => BODY_BADGES[bodyMode(props.request)] ?? "");
const auth = computed({
  get: () => props.request.auth ?? null,
  set: (value) => {
    props.request.auth = value;
  },
});
const SHORT: Record<string, string> = { bearer: "Bearer", basic: "Basic", apiKey: "Key", oauth2: "OAuth", digest: "Digest", awsV4: "AWS", hawk: "Hawk", ntlm: "NTLM" };
const authBadge = computed(() => {
  const own = props.request.auth;
  if (own) return SHORT[own.type] ?? "";
  const inherited = props.inheritedAuth;
  return inherited.found && SHORT[inherited.auth.type] ? `${SHORT[inherited.auth.type]} ↑` : "";
});
const activeHeaders = computed(() => props.request.headers.filter((h) => h.enabled && h.name.trim()).length);
/** The settings the request really uses (its own, else inherited): token requests use them too. */
const effectiveSettings = computed(() => {
  const own = props.request.settings ?? {};
  const inherited = props.inheritedSettings;
  return {
    timeoutMs: own.timeoutMs ?? inherited.timeoutMs.value,
    verifySsl: own.verifySsl ?? inherited.verifySsl.value,
    followRedirects: own.followRedirects ?? inherited.followRedirects.value,
    proxy: own.proxy ?? inherited.proxy.value,
  };
});
const automatic = computed(() =>
  automaticHeaders(props.request, {
    version: props.version,
    auth: props.request.auth ?? props.inheritedAuth.auth,
    environment: props.environmentName,
    url: props.preview?.url ?? props.request.url,
  }),
);
const inheritedCount = computed(
  () =>
    props.inheritedHeaders.filter(
      (i) => !props.request.headers.some((h) => h.name.trim().toLowerCase() === i.header.name.trim().toLowerCase()),
    ).length,
);

</script>

<template>
  <section class="card editor">
    <div class="urlbar">
      <span v-if="mqtt" class="method mqtt" :title="$t('MQTT: connect below')">MQTT</span>
      <span v-else-if="socketio" class="method ws" :title="$t('Socket.IO: connect below')">SIO</span>
      <span v-else-if="websocket" class="method ws" :title="$t('WebSocket: connect below')">WS</span>
      <span v-else-if="grpc" class="method grpc" :title="$t('gRPC: pick the method in Message')">gRPC</span>
      <select v-else v-model="request.method" class="method" :class="`m-${request.method.toLowerCase()}`">
        <option v-for="m in HTTP_METHODS" :key="m" :value="m">{{ m }}</option>
      </select>
      <VarInput
        v-model="request.url"
        class="url"
        placeholder="https://api.example.com/resource"
        aria-label="URL"
        :intercept-paste="pasteCurl"
        @enter="emit('send')"
        @run="emit('send')"
        @save="emit('save')"
      />
      <button
        v-if="!grpc && !/^\s*mqtts?:\/\//i.test(request.url)"
        class="stream"
        :class="{ on: !!request.mqtt }"
        :title="request.mqtt ? $t('MQTT: click to go back to a plain request') : $t('Talk MQTT to this broker (ws:// or wss://); mqtt:// URLs are MQTT already')"
        @click="toggleMqtt"
      >
        MQTT
      </button>
      <button
        v-if="!grpc && !mqtt"
        class="stream"
        :class="{ on: socketio }"
        :title="socketio ? $t('Socket.IO: click to go back to a plain request') : $t('Talk Socket.IO (v4) to this server')"
        @click="toggleSocketIo"
      >
        SIO
      </button>
      <button
        v-if="!websocket && !grpc && !socketio && !mqtt"
        class="stream"
        :class="{ on: stream }"
        :title="stream ? $t('Listening to Server-Sent Events: click to send it as a normal request') : $t('Listen to it as Server-Sent Events (Accept: text/event-stream)')"
        @click="toggleStream"
      >
        SSE
      </button>
      <button v-if="!websocket && !stream && !socketio && !mqtt" class="send" :disabled="sending || !supported" @click="emit('send')">
        {{ sending ? $t("Sending…") : $t("Send") }}
      </button>
    </div>
    <p v-if="!supported" class="hint">{{ $t("This protocol is not supported yet.") }}</p>
    <p v-else-if="preview && (preview.changed || preview.missing.length)" class="resolved">
      <span v-if="preview.changed" class="url" :title="preview.url">→ {{ preview.url }}</span>
      <span v-if="preview.missing.length" class="missing">
        Missing: <code v-for="name in preview.missing" :key="name">{{ name }}</code>
      </span>
    </p>

    <div class="tabs">
      <button :class="{ on: tab === 'params' }" @click="tab = 'params'">
        {{ $t("Params") }} <span v-if="paramCount" class="count">{{ paramCount }}</span>
      </button>
      <button :class="{ on: tab === 'auth' }" @click="tab = 'auth'">
        {{ $t("Auth") }} <span v-if="authBadge" class="count" :title="request.auth ? $t('Set on this request') : $t('Inherited from a parent')">{{ authBadge }}</span>
      </button>
      <button v-if="grpc" :class="{ on: tab === 'body' }" @click="tab = 'body'">
        {{ $t("Message") }} <span v-if="request.grpc?.method" class="count">{{ request.grpc.method.split("/").pop() }}</span>
      </button>
      <button v-else-if="mqtt" :class="{ on: tab === 'body' }" @click="(ensureMqtt(), (tab = 'body'))">
        {{ $t("Topics") }} <span v-if="request.mqtt?.subscribe?.length" class="count">{{ request.mqtt.subscribe.length }}</span>
      </button>
      <button v-else-if="socketio" :class="{ on: tab === 'body' }" @click="tab = 'body'">
        {{ $t("Events") }} <span v-if="request.messages?.length" class="count">{{ request.messages.length }}</span>
      </button>
      <button v-else-if="websocket" :class="{ on: tab === 'body' }" @click="tab = 'body'">
        {{ $t("Messages") }} <span v-if="request.messages?.length" class="count">{{ request.messages.length }}</span>
      </button>
      <button v-else :class="{ on: tab === 'body' }" @click="tab = 'body'">
        {{ $t("Body") }} <span v-if="bodyBadge" class="count">{{ bodyBadge }}</span>
      </button>
      <button :class="{ on: tab === 'headers' }" @click="tab = 'headers'">
        {{ $t("Headers") }} <span v-if="activeHeaders" class="count">{{ activeHeaders }}</span
        ><span v-if="inheritedCount" class="count" :title="$t('Inherited from folders')">+{{ inheritedCount }} ↑</span>
      </button>
      <button :class="{ on: tab === 'settings' }" @click="tab = 'settings'">
        {{ $t("Settings") }} <span v-if="settingsCount" class="count" :title="$t('Settings set on this request')">{{ settingsCount }}</span>
      </button>
      <button :class="{ on: tab === 'pre' }" @click="tab = 'pre'">
        {{ $t("Pre-request") }} <span v-if="scripts.pre.trim()" class="dot"></span>
      </button>
      <button :class="{ on: tab === 'examples' }" @click="(examplesNew = false), (tab = 'examples')">{{ $t("Examples") }}</button>
      <button :class="{ on: tab === 'test' }" @click="tab = 'test'">
        {{ $t("Tests") }} <span v-if="scripts.test.trim()" class="dot"></span>
      </button>
      <span class="spacer"></span>
      <span class="kbd">{{ $t("Ctrl ↵ to send") }}</span>
    </div>

    <ParamsEditor v-if="tab === 'params'" :request="request" />

    <AuthEditor
      v-else-if="tab === 'auth'"
      v-model="auth"
      :inherited="inheritedAuth"
      :root="root"
      :environment-id="environmentId"
      :settings="effectiveSettings"
    />

    <ExamplesEditor
      v-else-if="tab === 'examples'"
      ref="examplesEditor"
      :root="root"
      :request-id="requestId"
      :start-new="examplesNew"
    />

    <SettingsEditor
      v-else-if="tab === 'settings'"
      :request="request"
      :inherited="inheritedSettings"
      :root="root"
      :resolved-url="preview?.url ?? request.url"
    />

    <GrpcEditor
      v-else-if="tab === 'body' && grpc"
      :request="request"
      :root="root"
      :request-id="requestId"
      :scripts="scripts"
      :environment-id="environmentId"
      @send="emit('send')"
      @save="emit('save')"
    />

    <div v-else-if="tab === 'body' && mqtt && request.mqtt" class="sio">
      <div class="sio-row">
        <label class="grow">{{ $t("Subscribe to") }} <input v-model="subscribeText" :placeholder="$t('orders/#, sensors/+/temp')" spellcheck="false" /></label>
        <label>QoS
          <select v-model.number="request.mqtt.qos"><option :value="0">0</option><option :value="1">1</option></select>
        </label>
        <label>{{ $t("Retain") }} <input v-model="request.mqtt.retain" type="checkbox" /></label>
      </div>
      <div class="sio-row">
        <label>{{ $t("Client id") }} <input v-model="request.mqtt.clientId" :placeholder="$t('(made up)')" spellcheck="false" /></label>
        <label>{{ $t("User") }} <input v-model="request.mqtt.username" spellcheck="false" /></label>
        <label>{{ $t("Password") }} <input v-model="request.mqtt.password" placeholder="{{mqttPassword}}" spellcheck="false" /></label>
      </div>
      <p class="hint">{{ $t("Saved publications: the name is the topic, the body the payload.") }} <code>pulpo run</code> {{ $t("publishes them and collects what arrives.") }}</p>
      <WsMessagesEditor :request="request" @save="emit('save')" />
    </div>

    <div v-else-if="tab === 'body' && socketio && request.socketio" class="sio">
      <div class="sio-row">
        <label>{{ $t("Namespace") }} <input v-model="request.socketio.namespace" placeholder="/" spellcheck="false" /></label>
        <label>{{ $t("Path") }} <input v-model="request.socketio.path" placeholder="/socket.io" spellcheck="false" /></label>
        <label class="grow">
          {{ $t("Auth (JSON)") }} <input v-model="request.socketio.auth" placeholder='{ "token": "{{token}}" }' spellcheck="false" />
        </label>
      </div>
      <p class="hint">{{ $t("Saved events: the name is the event, the body its data.") }} <code>pulpo run</code> {{ $t("emits them in order.") }}</p>
      <WsMessagesEditor :request="request" @save="emit('save')" />
    </div>

    <WsMessagesEditor v-else-if="tab === 'body' && websocket" :request="request" @save="emit('save')" />

    <BodyEditor
      v-else-if="tab === 'body'"
      :request="request"
      :root="root"
      :request-id="requestId"
      :scripts="scripts"
      :environment-id="environmentId"
      @send="emit('send')"
      @save="emit('save')"
    />

    <template v-else-if="tab === 'pre' || tab === 'test'">
      <CodeEditor
        :key="tab"
        v-model="scripts[tab]"
        class="code-area"
        language="javascript"
        @run="emit('send')"
        @save="emit('save')"
        :placeholder="
          tab === 'pre'
            ? $t('Runs before sending. e.g. pm.request.headers.upsert({ key: \'X-Request-Id\', value: pm.variables.replaceIn(\'{{$guid}}\') });')
            : $t('Runs after the response. e.g. pm.test(\'is 200\', () => pm.response.to.have.status(200));\npm.environment.set(\'orderId\', pm.response.json().id);')
        "
      />
      <p class="hint">
        {{ $t("Saved as") }} <code>{{ tab === "pre" ? ".pre.js" : ".test.js" }}</code> {{ $t("next to the request. Postman's") }} <code>pm.*</code> {{ $t("API: variables, request, response,") }} <code>pm.test</code>, <code>pm.expect</code>{{ $t(". Values set here stay local (in") }} <code>.pulpo/</code>).
      </p>
    </template>

    <HeadersEditor
      v-else
      :headers="request.headers"
      :inherited="inheritedHeaders"
      :automatic="automatic"
      @stop-cookies="request.cookies = false"
    >
      <label class="toggle" :title="$t('Uses the cookie jar of the selected environment (🍪 in the top bar)')">
        <input
          type="checkbox"
          :checked="request.cookies ?? true"
          @change="request.cookies = ($event.target as HTMLInputElement).checked"
        />
        {{ $t("Send and store cookies") }}
        <span class="hint-inline">— a <code>Cookie</code> {{ $t("header written here wins over the jar") }}</span>
      </label>
    </HeadersEditor>
  </section>
</template>

<style scoped>
.editor {
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.urlbar {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
.method {
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface-2);
  padding: 0 10px;
  font: 600 12px var(--mono);
  cursor: pointer;
}
.stream {
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface-2);
  color: var(--muted);
  padding: 0 10px;
  font: 600 12px var(--mono);
  cursor: pointer;
}
.stream.on {
  color: var(--p-sse, var(--accent));
  border-color: var(--p-sse, var(--accent));
}
.sio {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.sio-row {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.sio-row label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--muted);
}
.sio-row .grow {
  flex: 1;
  min-width: 200px;
}
.method.mqtt {
  display: inline-flex;
  align-items: center;
  color: var(--p-mqtt, #7a9a3a);
  cursor: default;
}
.method.grpc {
  display: inline-flex;
  align-items: center;
  color: var(--p-grpc, var(--accent));
  cursor: default;
}
.method.ws {
  display: inline-flex;
  align-items: center;
  color: var(--p-websocket, var(--accent));
  cursor: default;
}
.m-get { color: var(--m-get); }
.m-post { color: var(--m-post); }
.m-put { color: var(--m-put); }
.m-patch { color: var(--m-patch); }
.m-delete { color: var(--m-delete); }
.urlbar .url {
  flex: 1 1 220px;
}
.urlbar .url :deep(.cm-editor) {
  height: 100%;
  border-radius: 8px;
}
.urlbar .url :deep(.cm-content) {
  padding: 8px 12px;
}
.urlbar input {
  flex: 1 1 220px;
  min-width: 0;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface-2);
  padding: 9px 12px;
  font-family: var(--mono);
  outline: none;
}
.urlbar input:focus,
.method:focus,
textarea:focus,
.row input:focus {
  border-color: var(--accent);
  outline: none;
}
.send {
  border: 0;
  border-radius: 8px;
  padding: 0 22px;
  background: var(--accent);
  color: #fff;
  font-weight: 600;
  cursor: pointer;
}
.send:disabled {
  opacity: 0.6;
  cursor: default;
}
.hint {
  margin: 8px 2px 0;
  color: var(--muted);
  font-size: 12px;
}
.resolved {
  display: flex;
  gap: 12px;
  align-items: center;
  margin: 8px 2px 0;
  font-size: 12px;
  min-width: 0;
}
.resolved .url {
  color: var(--muted);
  font-family: var(--mono);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}
.missing {
  color: var(--m-post);
  white-space: nowrap;
}
.missing code {
  margin-left: 4px;
  padding: 1px 6px;
  border-radius: 4px;
  background: rgba(217, 130, 43, 0.14);
}
.tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  margin: 12px 0 8px;
  overflow-x: auto;
  scrollbar-width: thin;
}
.tabs > * {
  flex-shrink: 0;
  white-space: nowrap;
}
.tabs button {
  border: 0;
  background: transparent;
  padding: 6px 10px;
  border-radius: 6px;
  color: var(--muted);
  cursor: pointer;
}
.tabs button.on {
  background: var(--surface-2);
  color: var(--text);
  font-weight: 600;
}
.count {
  margin-left: 4px;
  padding: 0 6px;
  border-radius: 999px;
  background: var(--accent-soft);
  color: var(--accent);
  font-size: 11px;
}
.spacer {
  flex: 1;
}
.dot {
  display: inline-block;
  width: 6px;
  height: 6px;
  margin-left: 4px;
  border-radius: 50%;
  background: var(--accent);
  vertical-align: middle;
}
.kbd {
  color: var(--muted);
  font-size: 11px;
}
.link {
  border: 0;
  background: transparent;
  color: var(--accent) !important;
  cursor: pointer;
  padding: 6px 4px;
  align-self: flex-start;
}
.code-area {
  min-height: 160px;
}
.code {
  flex: 1;
  min-height: 140px;
  resize: vertical;
  border: 1px solid transparent;
  border-radius: 8px;
  background: var(--surface-2);
  padding: 12px;
  font: 13px/1.6 var(--mono);
}
.empty {
  color: var(--muted);
  margin: 8px 2px;
}
.headers {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.row {
  display: grid;
  grid-template-columns: 20px 1fr 2fr 28px;
  gap: 6px;
  align-items: center;
}
.row input:not([type="checkbox"]) {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 13px;
}
.icon {
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}
.toggle {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 6px;
  font-size: 12px;
  color: var(--muted);
  cursor: pointer;
}
.hint-inline {
  opacity: 0.8;
}
</style>
