<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  asCollectionError,
  backend,
  CODE_LANGUAGES,
  type CodeLanguage,
  type CodeOptions,
  type RequestEntry,
} from "../workspace";
import CodeEditor, { type Language } from "./CodeEditor.vue";

/** The current request as code in another language, ready to copy. */
const props = defineProps<{ root: string; entry: RequestEntry; environmentId: string; environmentName: string }>();
const emit = defineEmits<{ close: [] }>();

const LANGUAGE_KEY = "pulpo.codeLanguage";
function remembered(): CodeLanguage {
  try {
    const saved = localStorage.getItem(LANGUAGE_KEY);
    if (saved && saved in CODE_LANGUAGES) return saved as CodeLanguage;
  } catch {
    /* storage may be unavailable */
  }
  return "curl";
}

const language = ref<CodeLanguage>(remembered());
const options = ref<CodeOptions>({ resolve: false, includeSecrets: false });
const code = ref("");
const problem = ref("");
const copied = ref(false);

const highlight = computed<Language>(() =>
  language.value === "fetch" || language.value === "axios" ? "javascript" : "text",
);

async function generate() {
  try {
    code.value = await backend.generateCode(props.root, props.entry, props.environmentId, language.value, options.value);
    problem.value = "";
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}

watch(
  [language, options],
  () => {
    try {
      localStorage.setItem(LANGUAGE_KEY, language.value);
    } catch {
      /* ignore */
    }
    // Secrets only make sense when variables are resolved.
    if (!options.value.resolve && options.value.includeSecrets) options.value.includeSecrets = false;
    void generate();
  },
  { deep: true, immediate: true },
);

async function copy() {
  try {
    await navigator.clipboard.writeText(code.value);
    copied.value = true;
    setTimeout(() => (copied.value = false), 1500);
  } catch {
    problem.value = "Could not copy: select the code and copy it by hand.";
  }
}
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card panel" role="dialog" aria-label="Code">
      <header>
        <div>
          <h2>Code</h2>
          <p class="sub">{{ entry.name }}, with what it inherits from its folders.</p>
        </div>
        <button class="ghost" title="Close" @click="emit('close')">✕</button>
      </header>

      <div class="tools">
        <select v-model="language" aria-label="Language">
          <option v-for="(label, id) in CODE_LANGUAGES" :key="id" :value="id">{{ label }}</option>
        </select>
        <label :title="environmentId ? `Use the values of ${environmentName}` : 'Pick an environment in the top bar first'">
          <input v-model="options.resolve" type="checkbox" :disabled="!environmentId" />
          Resolve variables{{ environmentName ? ` (${environmentName})` : "" }}
        </label>
        <label>
          <input v-model="options.includeSecrets" type="checkbox" :disabled="!options.resolve" />
          Include secrets
        </label>
        <span class="spacer"></span>
        <button class="primary" :disabled="!code" @click="copy">{{ copied ? "Copied" : "Copy" }}</button>
      </div>

      <p v-if="options.includeSecrets" class="warn">
        The code now holds secret values (and tokens set by scripts). Do not paste it in tickets, chats or repos.
      </p>
      <p v-else-if="options.resolve" class="sub">
        Secrets, values set by scripts and dynamic variables stay as <code v-pre>{{name}}</code>.
      </p>
      <p v-if="problem" class="warn">{{ problem }}</p>

      <CodeEditor :model-value="code" :language="highlight" readonly class="code" />
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
.panel {
  width: min(860px, 94vw);
  height: min(640px, 86vh);
  display: flex;
  flex-direction: column;
  gap: 10px;
}
header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}
h2 {
  margin: 0;
  font-size: 17px;
}
.sub,
.warn {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.tools {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 14px;
  font-size: 13px;
}
.tools label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
}
select {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  cursor: pointer;
}
.spacer {
  flex: 1;
}
.code {
  flex: 1;
  min-height: 0;
}
.ghost,
.primary {
  border-radius: 8px;
  padding: 6px 14px;
  cursor: pointer;
}
.ghost {
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text);
}
.primary {
  border: 0;
  background: var(--accent);
  color: #fff;
  font-weight: 600;
}
button:disabled {
  opacity: 0.5;
  cursor: default;
}
</style>
