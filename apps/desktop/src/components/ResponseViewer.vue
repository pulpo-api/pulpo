<script setup lang="ts">
import { computed, ref } from "vue";
import type { HttpResponse, SendError } from "../core";

const props = defineProps<{ response: HttpResponse | null; error: SendError | null; sending: boolean }>();

const tab = ref<"body" | "headers">("body");

const pretty = computed(() => {
  const body = props.response?.body ?? "";
  try {
    return JSON.stringify(JSON.parse(body), null, 2);
  } catch {
    return body;
  }
});

const statusClass = computed(() => {
  const status = props.response?.status ?? 0;
  if (status >= 500) return "s5";
  if (status >= 400) return "s4";
  if (status >= 300) return "s3";
  return "s2";
});

function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

const errorTitle: Record<SendError["kind"], string> = {
  invalidUrl: "Invalid URL",
  invalidHeader: "Invalid header",
  timeout: "Timed out",
  connect: "Could not connect",
  unresolved: "Missing variables",
  other: "Request failed",
};
</script>

<template>
  <section class="card viewer">
    <div v-if="sending" class="placeholder"><span class="spinner"></span> Waiting for the server…</div>

    <div v-else-if="error" class="error">
      <strong>{{ errorTitle[error.kind] }}</strong>
      <p>{{ error.message }}</p>
      <p v-if="error.kind === 'unresolved'" class="tip">Pick an environment that defines them, or add them with ⚙.</p>
    </div>

    <template v-else-if="response">
      <div class="status">
        <span class="pill" :class="statusClass">{{ response.status }} {{ response.statusText }}</span>
        <span class="muted">{{ response.durationMs }} ms · {{ size(response.sizeBytes) }} · {{ response.httpVersion }}</span>
        <span class="spacer"></span>
        <button :class="{ on: tab === 'body' }" @click="tab = 'body'">Body</button>
        <button :class="{ on: tab === 'headers' }" @click="tab = 'headers'">Headers {{ response.headers.length }}</button>
      </div>
      <pre v-if="tab === 'body'" class="code">{{ pretty || "(empty body)" }}</pre>
      <table v-else class="code table">
        <tbody>
          <tr v-for="(h, i) in response.headers" :key="i">
            <td>{{ h.name }}</td>
            <td>{{ h.value }}</td>
          </tr>
        </tbody>
      </table>
    </template>

    <div v-else class="placeholder">
      <img src="/pulpo.svg" width="56" height="56" alt="" />
      <p>Hit <strong>Send</strong> to see the response here.</p>
    </div>
  </section>
</template>

<style scoped>
.viewer {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 0;
}
.status {
  display: flex;
  align-items: center;
  gap: 10px;
}
.status button {
  border: 0;
  background: transparent;
  padding: 6px 10px;
  border-radius: 6px;
  color: var(--muted);
  cursor: pointer;
}
.status button.on {
  background: var(--surface-2);
  color: var(--text);
  font-weight: 600;
}
.spacer {
  flex: 1;
}
.muted {
  color: var(--muted);
  font-size: 12px;
}
.pill {
  padding: 3px 10px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
}
.s2 { background: rgba(31, 157, 104, 0.14); color: var(--ok); }
.s3 { background: rgba(47, 125, 225, 0.14); color: var(--m-put); }
.s4 { background: rgba(217, 130, 43, 0.16); color: var(--m-post); }
.s5 { background: rgba(224, 70, 79, 0.14); color: var(--m-delete); }
.code {
  flex: 1;
  margin: 0;
  padding: 12px;
  border-radius: 8px;
  background: var(--surface-2);
  font: 13px/1.6 var(--mono);
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-word;
}
.table {
  display: block;
  border-collapse: collapse;
}
.table td {
  padding: 2px 16px 2px 0;
  vertical-align: top;
}
.table td:first-child {
  color: var(--muted);
  white-space: nowrap;
}
.placeholder {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--muted);
}
.placeholder img {
  opacity: 0.8;
}
.spinner {
  width: 22px;
  height: 22px;
  border: 3px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
.error {
  border-left: 3px solid var(--m-delete);
  padding: 4px 12px;
}
.error .tip {
  font-family: var(--sans);
}
.error p {
  margin: 6px 0 0;
  font-family: var(--mono);
  font-size: 13px;
  color: var(--muted);
}
</style>
