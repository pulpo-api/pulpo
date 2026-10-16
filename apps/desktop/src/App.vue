<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import Sidebar from "./components/Sidebar.vue";
import { sampleCollection, type RequestItem } from "./sample";
import { coreInfo, type CoreInfo } from "./core";

const selected = ref<RequestItem>(sampleCollection[1].items[0]);
const tab = ref<"body" | "headers" | "auth" | "tests">("body");
const info = ref<CoreInfo | null>(null);
const theme = ref<"light" | "dark">(
  window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
);

watch(theme, (value) => document.documentElement.setAttribute("data-theme", value), { immediate: true });

onMounted(async () => {
  info.value = await coreInfo();
});

const body = `{
  "idempotencyKey": "{{$guid}}",
  "groups": [
    { "tenantId": "{{tenantId}}", "storeId": "{{storeId}}", "deliveryMode": "PICKUP" }
  ]
}`;

const response = `{
  "checkoutId": "5b0c3c8e-6f4e-4a1d-9f62-1d2b7c0e9a41",
  "orders": [
    { "orderId": "a91f…", "code": "P-7K2Q9M", "status": "PENDING_PAYMENT" }
  ]
}`;
</script>

<template>
  <div class="layout">
    <Sidebar :folders="sampleCollection" :selected-id="selected.id" @select="selected = $event" />

    <main>
      <header class="topbar">
        <div class="crumbs">
          <span class="muted">Marketplace</span>
          <span class="muted">/</span>
          <strong>{{ selected.name }}</strong>
        </div>
        <div class="actions">
          <select class="env">
            <option>Local</option>
            <option>Dev</option>
          </select>
          <button class="ghost" :title="`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`" @click="theme = theme === 'dark' ? 'light' : 'dark'">
            {{ theme === "dark" ? "☀" : "☾" }}
          </button>
        </div>
      </header>

      <section class="request card">
        <div class="urlbar">
          <span class="protocol">{{ selected.protocol === "http" ? selected.method : selected.protocol.toUpperCase() }}</span>
          <input :value="selected.url" spellcheck="false" />
          <button class="send">Send</button>
        </div>
        <div class="tabs">
          <button v-for="t in ['body', 'headers', 'auth', 'tests'] as const" :key="t" :class="{ on: tab === t }" @click="tab = t">
            {{ t }}
          </button>
        </div>
        <pre class="code">{{ tab === "body" ? body : `// ${tab} editor coming in phase 1` }}</pre>
      </section>

      <section class="response card">
        <div class="status">
          <span class="pill ok">201 Created</span>
          <span class="muted">184 ms · 412 B</span>
        </div>
        <pre class="code">{{ response }}</pre>
      </section>

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
  grid-template-rows: auto auto 1fr auto;
  gap: 14px;
  padding: 14px 18px;
  min-width: 0;
}
.topbar,
.actions,
.crumbs,
.status,
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
.card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow);
  padding: 14px;
  min-width: 0;
}
.urlbar {
  display: flex;
  gap: 8px;
  align-items: stretch;
}
.protocol {
  display: grid;
  place-items: center;
  min-width: 70px;
  padding: 0 10px;
  border-radius: 8px;
  background: var(--surface-2);
  color: var(--m-post);
  font: 600 12px var(--mono);
}
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
.urlbar input:focus {
  border-color: var(--accent);
}
.send {
  border: 0;
  border-radius: 8px;
  padding: 0 20px;
  background: var(--accent);
  color: #fff;
  font-weight: 600;
  cursor: pointer;
}
.send:hover {
  filter: brightness(1.05);
}
.tabs {
  display: flex;
  gap: 4px;
  margin: 12px 0 8px;
}
.tabs button {
  border: 0;
  background: transparent;
  padding: 6px 10px;
  border-radius: 6px;
  text-transform: capitalize;
  color: var(--muted);
  cursor: pointer;
}
.tabs button.on {
  background: var(--surface-2);
  color: var(--text);
  font-weight: 600;
}
.code {
  margin: 0;
  padding: 12px;
  border-radius: 8px;
  background: var(--surface-2);
  font: 13px/1.6 var(--mono);
  overflow: auto;
}
.response {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 0;
}
.response .code {
  flex: 1;
}
.pill {
  padding: 3px 10px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
}
.pill.ok {
  background: rgba(31, 157, 104, 0.14);
  color: var(--ok);
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
</style>
