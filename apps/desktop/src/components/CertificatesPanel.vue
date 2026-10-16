<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { asCollectionError, backend, clone, type ClientCertificate } from "../workspace";

const props = defineProps<{ root: string }>();
const emit = defineEmits<{ close: []; saved: [] }>();

type Row = ClientCertificate & { kind: "pem" | "pfx" };

const saved = ref<Row[]>([]);
const draft = ref<Row[]>([]);
const problem = ref("");

function toRows(list: ClientCertificate[]): Row[] {
  return list.map((c) => ({ ...c, kind: c.pfx ? "pfx" : "pem" }));
}

function fromRows(rows: Row[]): ClientCertificate[] {
  return rows
    .filter((r) => r.host.trim())
    .map(({ kind, host, cert, key, pfx, passphrase }) =>
      kind === "pfx"
        ? { host: host.trim(), pfx: pfx ?? "", ...(passphrase ? { passphrase } : {}) }
        : { host: host.trim(), cert: cert ?? "", ...(key ? { key } : {}) },
    );
}

async function load() {
  try {
    saved.value = toRows(await backend.listCertificates(props.root));
    draft.value = clone(saved.value);
    problem.value = "";
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}
onMounted(load);

const dirty = computed(() => JSON.stringify(fromRows(draft.value)) !== JSON.stringify(fromRows(saved.value)));

function add() {
  draft.value.push({ host: "", kind: "pfx", pfx: "", passphrase: "{{certPassword}}" });
}

/** A password typed literally would sit in a plain file; a secret variable lives in the keychain. */
function literalPassword(row: Row) {
  return row.kind === "pfx" && !!row.passphrase && !row.passphrase.includes("{{");
}

async function save() {
  try {
    await backend.saveCertificates(props.root, fromRows(draft.value));
    await load();
    emit("saved");
  } catch (e) {
    problem.value = asCollectionError(e).message;
  }
}
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card panel" role="dialog" aria-label="Client certificates">
      <header>
        <div>
          <h2>Client certificates</h2>
          <p class="sub">
            For APIs that ask for a certificate (mTLS). Kept in <code>.pulpo/certificates.json</code> on this machine, never
            in git: each teammate points to their own files.
          </p>
        </div>
        <button class="ghost" title="Close" @click="emit('close')">✕</button>
      </header>

      <p v-if="!backend.persistent" class="sub">In the browser preview certificates are not used; this list is only to try the panel.</p>
      <p v-if="problem" class="warn">{{ problem }}</p>

      <div class="list">
        <p v-if="!draft.length" class="sub">No certificates yet.</p>
        <div v-for="(row, i) in draft" :key="i" class="cert">
          <div class="line">
            <input v-model="row.host" class="host" placeholder="api.bank.test, api.bank.test:8443 or *.bank.test" spellcheck="false" />
            <select v-model="row.kind" title="Certificate format">
              <option value="pfx">PFX / P12</option>
              <option value="pem">PEM (cert + key)</option>
            </select>
            <button class="icon" title="Remove" @click="draft.splice(i, 1)">✕</button>
          </div>
          <div v-if="row.kind === 'pfx'" class="line">
            <input v-model="row.pfx" placeholder="~/certs/me.pfx" spellcheck="false" />
            <input v-model="row.passphrase" placeholder="{{certPassword}}" spellcheck="false" autocomplete="off" />
          </div>
          <div v-else class="line">
            <input v-model="row.cert" placeholder="~/certs/me.crt" spellcheck="false" />
            <input v-model="row.key" placeholder="~/certs/me.key (not encrypted)" spellcheck="false" />
          </div>
          <p v-if="literalPassword(row)" class="warn">
            The password would be stored as plain text. Write <code v-pre>{{certPassword}}</code> and add it as a secret of the
            environment (⚙): it then lives in the OS keychain.
          </p>
        </div>
        <button class="link" @click="add">+ Add certificate</button>
      </div>

      <p class="sub">Paths: absolute, <code>~/</code> for your home folder, or relative to the workspace.</p>

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
  z-index: 20;
}
.panel {
  width: min(760px, 94vw);
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
.warn {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.list {
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.cert {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding-bottom: 10px;
  border-bottom: 1px solid var(--border);
}
.line {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.line input {
  flex: 1 1 200px;
}
input,
select {
  min-width: 0;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 13px;
  outline: none;
}
select {
  font-family: inherit;
}
input:focus,
select:focus {
  border-color: var(--accent);
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
.link {
  align-self: flex-start;
  border: 0;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  padding: 4px 0;
}
</style>
