<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import Sidebar from "./components/Sidebar.vue";
import RequestEditor from "./components/RequestEditor.vue";
import ResponseViewer from "./components/ResponseViewer.vue";
import { sampleCollection, type RequestItem } from "./sample";
import { asSendError, coreInfo, sendHttp, type CoreInfo, type HttpResponse, type SendError } from "./core";

interface Result {
  response: HttpResponse | null;
  error: SendError | null;
  sending: boolean;
}

const items = reactive(
  sampleCollection.map((folder) => ({
    ...folder,
    items: folder.items.map((item) => ({ ...item, request: structuredClone(item.request) })),
  })),
);
const selected = ref<RequestItem>(items[0].items[0]);
const results = reactive<Record<string, Result>>({});
const info = ref<CoreInfo | null>(null);
const theme = ref<"light" | "dark">(window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");

const result = computed<Result>(() => results[selected.value.id] ?? { response: null, error: null, sending: false });
const folderName = computed(() => items.find((f) => f.items.some((i) => i.id === selected.value.id))?.name ?? "");

watch(theme, (value) => document.documentElement.setAttribute("data-theme", value), { immediate: true });

function onKey(event: KeyboardEvent) {
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
    event.preventDefault();
    void send();
  }
}

onMounted(async () => {
  window.addEventListener("keydown", onKey);
  info.value = await coreInfo();
});

onBeforeUnmount(() => window.removeEventListener("keydown", onKey));

async function send() {
  const item = selected.value;
  if (item.protocol !== "http" || results[item.id]?.sending) return;
  results[item.id] = { response: null, error: null, sending: true };
  try {
    const response = await sendHttp(item.request);
    results[item.id] = { response, error: null, sending: false };
  } catch (e) {
    results[item.id] = { response: null, error: asSendError(e), sending: false };
  }
}
</script>

<template>
  <div class="layout">
    <Sidebar :folders="items" :selected-id="selected.id" @select="selected = $event" />

    <main>
      <header class="topbar">
        <div class="crumbs">
          <span class="muted">{{ folderName }}</span>
          <span class="muted">/</span>
          <input v-model="selected.name" class="title" spellcheck="false" />
        </div>
        <div class="actions">
          <select class="env" title="Environments arrive in step 3">
            <option>No environment</option>
          </select>
          <button
            class="ghost"
            :title="`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`"
            @click="theme = theme === 'dark' ? 'light' : 'dark'"
          >
            {{ theme === "dark" ? "☀" : "☾" }}
          </button>
        </div>
      </header>

      <RequestEditor
        :key="selected.id"
        :request="selected.request"
        :sending="result.sending"
        :supported="selected.protocol === 'http'"
        @send="send"
      />

      <ResponseViewer :response="result.response" :error="result.error" :sending="result.sending" />

      <footer class="statusbar">
        <span>pulpo-core {{ info?.version ?? "…" }}</span>
        <span class="muted">{{ info?.protocols.join(" · ") }}</span>
      </footer>
    </main>
  </div>
</template>

<style scoped>
.layout {
  display: grid;
  grid-template-columns: 280px 1fr;
  height: 100%;
}
main {
  display: grid;
  grid-template-rows: auto minmax(220px, auto) 1fr auto;
  gap: 14px;
  padding: 14px 18px;
  min-width: 0;
  min-height: 0;
}
.topbar,
.actions,
.crumbs,
.statusbar {
  display: flex;
  align-items: center;
  gap: 10px;
}
.topbar {
  justify-content: space-between;
}
.muted {
  color: var(--muted);
}
.title {
  border: 1px solid transparent;
  background: transparent;
  font-weight: 700;
  padding: 2px 6px;
  border-radius: 6px;
}
.title:hover,
.title:focus {
  border-color: var(--border);
  outline: none;
}
.env,
.ghost {
  border: 1px solid var(--border);
  background: var(--surface);
  border-radius: 8px;
  padding: 6px 10px;
  cursor: pointer;
}
.statusbar {
  justify-content: space-between;
  font-size: 12px;
}
:deep(.card) {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
  padding: 14px;
  min-width: 0;
}
</style>
