<script setup lang="ts">
import { computed, ref } from "vue";
import type { HttpRequest } from "../core";
import CodeEditor from "./CodeEditor.vue";

const props = defineProps<{ request: HttpRequest }>();
const emit = defineEmits<{ save: [] }>();

const messages = computed(() => {
  if (!props.request.messages) props.request.messages = [];
  return props.request.messages;
});
const open = ref(0);

function add() {
  messages.value.push({ name: `Message ${messages.value.length + 1}`, body: "" });
  open.value = messages.value.length - 1;
}

function remove(i: number) {
  messages.value.splice(i, 1);
  open.value = Math.max(0, Math.min(open.value, messages.value.length - 1));
}

function move(i: number, by: number) {
  const j = i + by;
  if (j < 0 || j >= messages.value.length) return;
  const [m] = messages.value.splice(i, 1);
  messages.value.splice(j, 0, m);
  open.value = j;
}

function looksJson(text: string) {
  return /^\s*[[{]/.test(text);
}
</script>

<template>
  <div class="messages">
    <p v-if="!messages.length" class="empty">
      No saved messages. Save the ones you send often (subscribe, ping…): they are sent in order by
      <code>pulpo run</code> and flows, and one click away when connected.
    </p>
    <div v-for="(m, i) in messages" :key="i" class="message" :class="{ open: open === i }">
      <div class="head">
        <button class="toggle" :aria-expanded="open === i" @click="open = open === i ? -1 : i">{{ open === i ? "▾" : "▸" }}</button>
        <input v-model="m.name" class="name" spellcheck="false" aria-label="Message name" />
        <button class="icon" title="Move up" :disabled="i === 0" @click="move(i, -1)">↑</button>
        <button class="icon" title="Move down" :disabled="i === messages.length - 1" @click="move(i, 1)">↓</button>
        <button class="icon" title="Remove" @click="remove(i)">✕</button>
      </div>
      <CodeEditor
        v-if="open === i"
        v-model="m.body"
        variables
        class="code-area"
        :language="looksJson(m.body) ? 'json' : 'text'"
        placeholder='{ "type": "subscribe", "channel": "{{channel}}" }'
        @save="emit('save')"
      />
    </div>
    <button class="link" @click="add">+ Add message</button>
  </div>
</template>

<style scoped>
.messages {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.message {
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 6px 8px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.message.open .code-area {
  min-height: 120px;
}
.head {
  display: flex;
  gap: 6px;
  align-items: center;
}
.name {
  flex: 1;
  min-width: 0;
}
.toggle,
.icon {
  background: none;
  border: none;
  color: var(--muted);
  cursor: pointer;
}
.icon:disabled {
  opacity: 0.4;
  cursor: default;
}
.link {
  align-self: flex-start;
  background: none;
  border: none;
  color: var(--accent);
  cursor: pointer;
  padding: 0;
}
.empty {
  color: var(--muted);
  font-size: 13px;
  margin: 0;
}
</style>
