<script setup lang="ts">
import { computed, ref } from "vue";
import { HTTP_METHODS, bodyMode, type HttpRequest } from "../core";
import type { InheritedAuth, InheritedSettings, Scripts } from "../workspace";
import SettingsEditor from "./SettingsEditor.vue";
import AuthEditor from "./AuthEditor.vue";
import CodeEditor from "./CodeEditor.vue";
import BodyEditor from "./BodyEditor.vue";
import ParamsEditor from "./ParamsEditor.vue";
import { pathParamNames, splitUrl } from "../params";

const props = defineProps<{
  request: HttpRequest;
  scripts: Scripts;
  sending: boolean;
  supported: boolean;
  preview: { url: string; changed: boolean; missing: string[] } | null;
  inheritedAuth: InheritedAuth;
  inheritedSettings: InheritedSettings;
  root: string;
}>();
const emit = defineEmits<{ send: []; save: [] }>();

const tab = ref<"params" | "auth" | "body" | "headers" | "settings" | "pre" | "test">(
  props.request.method === "GET" || splitUrl(props.request.url).params.length ? "params" : "body",
);
const paramCount = computed(() => splitUrl(props.request.url).params.length + pathParamNames(props.request.url).length);
const BODY_BADGES: Partial<Record<ReturnType<typeof bodyMode>, string>> = {
  urlencoded: "Form",
  multipart: "Form",
  file: "File",
};
const settingsCount = computed(() => Object.keys(props.request.settings ?? {}).length);
const bodyBadge = computed(() => BODY_BADGES[bodyMode(props.request)] ?? "");
const auth = computed({
  get: () => props.request.auth ?? null,
  set: (value) => {
    props.request.auth = value;
  },
});
const SHORT: Record<string, string> = { bearer: "Bearer", basic: "Basic", apiKey: "Key" };
const authBadge = computed(() => {
  const own = props.request.auth;
  if (own) return SHORT[own.type] ?? "";
  const inherited = props.inheritedAuth;
  return inherited.found && SHORT[inherited.auth.type] ? `${SHORT[inherited.auth.type]} ↑` : "";
});
const activeHeaders = computed(() => props.request.headers.filter((h) => h.enabled && h.name.trim()).length);

function addHeader() {
  props.request.headers.push({ name: "", value: "", enabled: true });
}

function removeHeader(index: number) {
  props.request.headers.splice(index, 1);
}

</script>

<template>
  <section class="card editor">
    <div class="urlbar">
      <select v-model="request.method" class="method" :class="`m-${request.method.toLowerCase()}`">
        <option v-for="m in HTTP_METHODS" :key="m" :value="m">{{ m }}</option>
      </select>
      <input v-model="request.url" spellcheck="false" placeholder="https://api.example.com/resource" />
      <button class="send" :disabled="sending || !supported" @click="emit('send')">
        {{ sending ? "Sending…" : "Send" }}
      </button>
    </div>
    <p v-if="!supported" class="hint">This protocol arrives in phase 4. HTTP works today.</p>
    <p v-else-if="preview && (preview.changed || preview.missing.length)" class="resolved">
      <span v-if="preview.changed" class="url" :title="preview.url">→ {{ preview.url }}</span>
      <span v-if="preview.missing.length" class="missing">
        Missing: <code v-for="name in preview.missing" :key="name">{{ name }}</code>
      </span>
    </p>

    <div class="tabs">
      <button :class="{ on: tab === 'params' }" @click="tab = 'params'">
        Params <span v-if="paramCount" class="count">{{ paramCount }}</span>
      </button>
      <button :class="{ on: tab === 'auth' }" @click="tab = 'auth'">
        Auth <span v-if="authBadge" class="count" :title="request.auth ? 'Set on this request' : 'Inherited from a parent'">{{ authBadge }}</span>
      </button>
      <button :class="{ on: tab === 'body' }" @click="tab = 'body'">
        Body <span v-if="bodyBadge" class="count">{{ bodyBadge }}</span>
      </button>
      <button :class="{ on: tab === 'headers' }" @click="tab = 'headers'">
        Headers <span v-if="activeHeaders" class="count">{{ activeHeaders }}</span>
      </button>
      <button :class="{ on: tab === 'settings' }" @click="tab = 'settings'">
        Settings <span v-if="settingsCount" class="count" title="Settings set on this request">{{ settingsCount }}</span>
      </button>
      <button :class="{ on: tab === 'pre' }" @click="tab = 'pre'">
        Pre-request <span v-if="scripts.pre.trim()" class="dot"></span>
      </button>
      <button :class="{ on: tab === 'test' }" @click="tab = 'test'">
        Tests <span v-if="scripts.test.trim()" class="dot"></span>
      </button>
      <span class="spacer"></span>
      <span class="kbd">Ctrl ↵ to send</span>
    </div>

    <ParamsEditor v-if="tab === 'params'" :request="request" />

    <AuthEditor v-else-if="tab === 'auth'" v-model="auth" :inherited="inheritedAuth" />

    <SettingsEditor
      v-else-if="tab === 'settings'"
      :request="request"
      :inherited="inheritedSettings"
      :root="root"
      :resolved-url="preview?.url ?? request.url"
    />

    <BodyEditor v-else-if="tab === 'body'" :request="request" :root="root" @send="emit('send')" @save="emit('save')" />

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
            ? 'Runs before sending. e.g. pm.request.headers.upsert({ key: \'X-Request-Id\', value: pm.variables.replaceIn(\'{{$guid}}\') });'
            : 'Runs after the response. e.g. pm.test(\'is 200\', () => pm.response.to.have.status(200));\npm.environment.set(\'orderId\', pm.response.json().id);'
        "
      />
      <p class="hint">
        Saved as <code>{{ tab === "pre" ? ".pre.js" : ".test.js" }}</code> next to the request. Postman's <code>pm.*</code> API:
        variables, request, response, <code>pm.test</code>, <code>pm.expect</code>. Values set here stay local (in <code>.pulpo/</code>).
      </p>
    </template>

    <div v-else class="headers">
      <div v-for="(header, i) in request.headers" :key="i" class="row">
        <input v-model="header.enabled" type="checkbox" :title="header.enabled ? 'Sent' : 'Not sent'" />
        <input v-model="header.name" placeholder="Name" spellcheck="false" />
        <input v-model="header.value" placeholder="Value" spellcheck="false" />
        <button class="icon" title="Remove" @click="removeHeader(i)">✕</button>
      </div>
      <button class="link" @click="addHeader">+ Add header</button>
      <label class="toggle" title="Uses the cookie jar of the selected environment (🍪 in the top bar)">
        <input
          type="checkbox"
          :checked="request.cookies ?? true"
          @change="request.cookies = ($event.target as HTMLInputElement).checked"
        />
        Send and store cookies
        <span class="hint-inline">— a <code>Cookie</code> header written here wins over the jar</span>
      </label>
    </div>
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
.m-get { color: var(--m-get); }
.m-post { color: var(--m-post); }
.m-put { color: var(--m-put); }
.m-patch { color: var(--m-patch); }
.m-delete { color: var(--m-delete); }
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
