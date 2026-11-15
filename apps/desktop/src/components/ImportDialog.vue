<script setup lang="ts">
import { ref } from "vue";
import type { ImportSummary } from "../workspace";

defineProps<{ target: string; available: boolean; blocked: string | null }>();
const emit = defineEmits<{
  close: [];
  file: [];
  text: [text: string];
}>();
defineExpose({ show });

const pasted = ref("");
const busy = ref(false);
const result = ref<ImportSummary | null>(null);
const error = ref<string | null>(null);

/** Called by the parent with the outcome of an import. */
function show(outcome: { summary?: ImportSummary; error?: string }) {
  busy.value = false;
  result.value = outcome.summary ?? null;
  error.value = outcome.error ?? null;
  if (outcome.summary) pasted.value = "";
}

function pickFile() {
  busy.value = true;
  error.value = null;
  result.value = null;
  emit("file");
}

function importText() {
  if (!pasted.value.trim()) return;
  busy.value = true;
  error.value = null;
  result.value = null;
  emit("text", pasted.value);
}

const formats: Record<string, string> = {
  postmanCollection: "Postman collection",
  postmanEnvironment: "Postman environment",
  openApi: "OpenAPI",
  curl: "curl command",
  insomnia: "Insomnia export",
  bruno: "Bruno collection",
  hoppscotch: "Hoppscotch collection",
  har: "HAR file",
  httpFile: ".http file",
};
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="dialog card" role="dialog" :aria-label="$t('Import')">
      <header>
        <h2>{{ $t("Import") }}</h2>
        <button class="icon" :title="$t('Close')" @click="emit('close')">✕</button>
      </header>

      <p v-if="!available" class="muted">
        {{ $t("Importing runs in the Pulpo engine, so it needs the desktop app. The browser preview can't import.") }}
      </p>
      <p v-else-if="blocked" class="warn">{{ blocked }}</p>
      <template v-else>
        <p class="muted">
          {{ $t("Into") }} <code>{{ target || "workspace root" }}</code>{{ $t(". Recognised automatically: Postman (v2.1) collections and environments, Insomnia, Hoppscotch, Bruno (pick its") }} <code>bruno.json</code>{{ $t("), OpenAPI 3, HAR files from the browser,") }}
          <code>.http</code> {{ $t("files and curl commands.") }}
        </p>

        <button class="primary" :disabled="busy" @click="pickFile">{{ $t("Choose a file…") }}</button>

        <div class="or"><span>{{ $t("or paste") }}</span></div>
        <textarea
          v-model="pasted"
          class="code"
          spellcheck="false"
          :placeholder="$t('curl \'https://api.example.com/orders\' -H \'Accept: application/json\'')"
        ></textarea>
        <button class="ghost" :disabled="busy || !pasted.trim()" @click="importText">
          {{ busy ? $t("Importing…") : $t("Import pasted text") }}
        </button>
      </template>

      <div v-if="result" class="result ok">
        <strong>
          {{ $t("Imported {0}: {1} requests, {2} folders", { 0: result.format ? $t(formats[result.format]) : "", 1: result.requests, 2: result.folders }) }}<template v-if="result.environments.length">{{ $t(", {0} environment(s)", { 0: result.environments.length }) }}</template>.
        </strong>
        <ul v-if="result.warnings.length">
          <li v-for="(w, i) in result.warnings.slice(0, 12)" :key="i">{{ w }}</li>
          <li v-if="result.warnings.length > 12">{{ $t("… and {0} more", { 0: result.warnings.length - 12 }) }}</li>
        </ul>
      </div>
      <p v-if="error" class="result bad">{{ error }}</p>
    </section>
  </div>
</template>

<style scoped>
.backdrop {
  position: fixed;
  inset: 0;
  background: rgba(10, 12, 20, 0.45);
  display: grid;
  place-items: center;
  z-index: 10;
}
.dialog {
  width: min(620px, 92vw);
  max-height: 86vh;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
h2 {
  margin: 0;
  font-size: 17px;
}
.icon {
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}
.muted {
  color: var(--muted);
  margin: 0;
}
.warn {
  margin: 0;
  color: var(--m-post);
}
.or {
  display: flex;
  align-items: center;
  gap: 10px;
  color: var(--muted);
  font-size: 12px;
}
.or::before,
.or::after {
  content: "";
  flex: 1;
  height: 1px;
  background: var(--border);
}
.code {
  min-height: 120px;
  resize: vertical;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface-2);
  padding: 10px;
  font: 13px/1.5 var(--mono);
  outline: none;
}
.code:focus {
  border-color: var(--accent);
}
.primary,
.ghost {
  border-radius: 8px;
  padding: 9px 16px;
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
}
button:disabled {
  opacity: 0.55;
  cursor: default;
}
.result {
  margin: 0;
  padding: 10px 12px;
  border-radius: 8px;
  background: var(--surface-2);
  font-size: 13px;
}
.result.ok {
  border-left: 3px solid var(--ok);
}
.result.bad {
  border-left: 3px solid var(--m-delete);
  font-family: var(--mono);
}
.result ul {
  margin: 8px 0 0;
  padding-left: 18px;
  color: var(--muted);
}
</style>
