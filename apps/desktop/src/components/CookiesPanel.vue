<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { asCollectionError, backend, clone, type Cookie } from "../workspace";

const props = defineProps<{ root: string; environmentId: string; environmentName: string }>();
const emit = defineEmits<{ close: [] }>();

const saved = ref<Cookie[]>([]);
const draft = ref<Cookie[]>([]);
const problem = ref("");
const filter = ref("");
const confirmClear = ref(false);
const adding = ref({ name: "", value: "", domain: "", path: "/" });

async function load() {
  try {
    saved.value = await backend.listCookies(props.root, props.environmentId);
    draft.value = clone(saved.value);
    problem.value = "";
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}
onMounted(load);

const dirty = computed(() => JSON.stringify(draft.value) !== JSON.stringify(saved.value));

/** Cookies grouped by domain, filtered by name, value or domain. */
const groups = computed(() => {
  const needle = filter.value.trim().toLowerCase();
  const map = new Map<string, { cookie: Cookie; index: number }[]>();
  draft.value.forEach((cookie, index) => {
    const text = `${cookie.domain} ${cookie.name} ${cookie.value}`.toLowerCase();
    if (needle && !text.includes(needle)) return;
    const list = map.get(cookie.domain) ?? [];
    list.push({ cookie, index });
    map.set(cookie.domain, list);
  });
  return [...map.entries()].sort(([a], [b]) => a.localeCompare(b));
});

function expiresText(cookie: Cookie): string {
  if (cookie.expires === null) return "Session";
  return new Date(cookie.expires * 1000).toLocaleString();
}

function remove(index: number) {
  draft.value.splice(index, 1);
}

function clearAll() {
  if (!confirmClear.value) {
    confirmClear.value = true;
    return;
  }
  draft.value = [];
  confirmClear.value = false;
}

function add() {
  const { name, value, domain, path } = adding.value;
  const host = domain.trim().replace(/^\./, "").toLowerCase();
  if (!name.trim() || !host) return;
  draft.value = draft.value.filter((c) => !(c.name === name.trim() && c.domain === host && c.path === (path || "/")));
  draft.value.push({
    name: name.trim(),
    value,
    domain: host,
    hostOnly: false,
    path: path.trim() || "/",
    expires: null,
    secure: false,
    httpOnly: false,
    sameSite: null,
  });
  adding.value = { name: "", value: "", domain: host, path: "/" };
}

async function save() {
  try {
    await backend.saveCookies(props.root, props.environmentId, draft.value);
    await load();
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card panel" role="dialog" aria-label="Cookies">
      <header>
        <div>
          <h2>Cookies</h2>
          <p class="sub">
            Jar of <b>{{ environmentName || "No environment" }}</b>. Kept in <code>.pulpo/cookies.json</code> on this machine,
            never in git.
          </p>
        </div>
        <button class="ghost" title="Close" @click="emit('close')">✕</button>
      </header>

      <p v-if="!backend.persistent" class="note">
        In the browser preview the browser keeps real cookies itself; this list is only to try the panel.
      </p>
      <p v-if="problem" class="warn">{{ problem }}</p>

      <div class="tools">
        <input v-model="filter" placeholder="Filter by domain, name or value" spellcheck="false" />
        <button class="ghost danger" :class="{ armed: confirmClear }" :disabled="!draft.length" @click="clearAll">
          {{ confirmClear ? "Click again to clear all" : "Clear all" }}
        </button>
      </div>

      <div class="list">
        <p v-if="!draft.length" class="empty">
          No cookies yet. Responses with <code>Set-Cookie</code> fill this jar, and later requests to the same domain send them.
        </p>
        <p v-else-if="!groups.length" class="empty">Nothing matches the filter.</p>
        <div v-for="[domain, items] in groups" :key="domain" class="group">
          <h3>{{ domain }}</h3>
          <div v-for="{ cookie, index } in items" :key="index" class="row">
            <code class="name" :title="cookie.name">{{ cookie.name }}</code>
            <input v-model="cookie.value" spellcheck="false" :aria-label="`Value of ${cookie.name}`" />
            <span class="meta" :title="`Path ${cookie.path}`">{{ cookie.path }}</span>
            <span class="meta">{{ expiresText(cookie) }}</span>
            <span class="flags">
              <span v-if="cookie.secure" title="Only sent over https">Secure</span>
              <span v-if="cookie.httpOnly" title="Browsers hide it from JavaScript">HttpOnly</span>
              <span v-if="cookie.hostOnly" title="Only this exact host, not its subdomains">Host</span>
            </span>
            <button class="icon" title="Delete" @click="remove(index)">✕</button>
          </div>
        </div>
      </div>

      <form class="add" @submit.prevent="add">
        <input v-model="adding.name" placeholder="name" spellcheck="false" />
        <input v-model="adding.value" placeholder="value" spellcheck="false" />
        <input v-model="adding.domain" placeholder="domain (api.example.com)" spellcheck="false" />
        <input v-model="adding.path" placeholder="/" spellcheck="false" />
        <button class="ghost" type="submit" :disabled="!adding.name.trim() || !adding.domain.trim()">Add</button>
      </form>

      <footer>
        <button class="ghost" :disabled="!dirty" @click="draft = clone(saved)">Discard</button>
        <button class="primary" :disabled="!dirty" @click="save">Save</button>
      </footer>
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
  container-type: inline-size;
  width: min(900px, 94vw);
  max-height: 86vh;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}
h2 {
  margin: 0;
  font-size: 17px;
}
.sub,
.note,
.empty,
.warn {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.tools {
  display: flex;
  gap: 8px;
}
.tools input {
  flex: 1;
}
input {
  min-width: 0;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 13px;
  outline: none;
}
input:focus {
  border-color: var(--accent);
}
.list {
  overflow: auto;
  min-height: 120px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
h3 {
  margin: 0 0 6px;
  font-size: 12px;
  color: var(--muted);
  font-family: var(--mono);
}
.row {
  display: grid;
  grid-template-columns: minmax(80px, 1fr) minmax(0, 2fr) 70px 150px auto 28px;
  gap: 8px;
  align-items: center;
  margin-bottom: 4px;
}
@container (max-width: 620px) {
  .row {
    grid-template-columns: minmax(70px, 1fr) minmax(0, 2fr) 28px;
  }
  .row .meta,
  .row .flags {
    display: none;
  }
}
.name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 13px;
}
.meta {
  font-size: 12px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.flags {
  display: flex;
  gap: 4px;
}
.flags span {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  background: var(--surface-2);
  color: var(--muted);
}
.add {
  display: grid;
  grid-template-columns: 1fr 1.5fr 1.5fr 70px auto;
  gap: 6px;
  border-top: 1px solid var(--border);
  padding-top: 10px;
}
@container (max-width: 620px) {
  .add {
    grid-template-columns: 1fr 1fr;
  }
}
footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
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
.ghost.danger.armed {
  border-color: var(--m-delete);
  color: var(--m-delete);
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
.icon {
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}
</style>
