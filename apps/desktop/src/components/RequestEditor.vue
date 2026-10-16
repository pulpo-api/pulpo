<script setup lang="ts">
import { computed, ref } from "vue";
import { HTTP_METHODS, type HttpRequest } from "../core";

const props = defineProps<{
  request: HttpRequest;
  sending: boolean;
  supported: boolean;
  preview: { url: string; changed: boolean; missing: string[] } | null;
}>();
const emit = defineEmits<{ send: [] }>();

const tab = ref<"body" | "headers">("body");
const hasBody = computed(() => !["GET", "HEAD"].includes(props.request.method));
const activeHeaders = computed(() => props.request.headers.filter((h) => h.enabled && h.name.trim()).length);

function addHeader() {
  props.request.headers.push({ name: "", value: "", enabled: true });
}

function removeHeader(index: number) {
  props.request.headers.splice(index, 1);
}

function formatBody() {
  if (!props.request.body) return;
  try {
    props.request.body = JSON.stringify(JSON.parse(props.request.body), null, 2);
  } catch {
    /* not JSON, leave as is */
  }
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
      <button :class="{ on: tab === 'body' }" @click="tab = 'body'">Body</button>
      <button :class="{ on: tab === 'headers' }" @click="tab = 'headers'">
        Headers <span v-if="activeHeaders" class="count">{{ activeHeaders }}</span>
      </button>
      <span class="spacer"></span>
      <button v-if="tab === 'body' && hasBody" class="link" @click="formatBody">Format JSON</button>
      <span class="kbd">Ctrl ↵ to send</span>
    </div>

    <template v-if="tab === 'body'">
      <textarea
        v-if="hasBody"
        class="code"
        spellcheck="false"
        placeholder='{ "hello": "pulpo" }'
        :value="request.body ?? ''"
        @input="request.body = ($event.target as HTMLTextAreaElement).value || null"
      ></textarea>
      <p v-else class="empty">{{ request.method }} requests have no body.</p>
    </template>

    <div v-else class="headers">
      <div v-for="(header, i) in request.headers" :key="i" class="row">
        <input v-model="header.enabled" type="checkbox" :title="header.enabled ? 'Sent' : 'Not sent'" />
        <input v-model="header.name" placeholder="Name" spellcheck="false" />
        <input v-model="header.value" placeholder="Value" spellcheck="false" />
        <button class="icon" title="Remove" @click="removeHeader(i)">✕</button>
      </div>
      <button class="link" @click="addHeader">+ Add header</button>
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
  flex: 1;
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
</style>
