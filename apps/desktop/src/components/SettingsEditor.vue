<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { DEFAULT_TIMEOUT_MS, type HttpRequest, type Settings } from "../core";
import { backend, certificateFor, type ClientCertificate, type InheritedSettings } from "../workspace";
import CertificatesPanel from "./CertificatesPanel.vue";

const props = defineProps<{ request: HttpRequest; inherited: InheritedSettings; root: string; resolvedUrl: string }>();

/** The request's own settings; writing undefined means "inherit". */
function own(): Settings {
  if (!props.request.settings) props.request.settings = {};
  return props.request.settings;
}

function set<K extends keyof Settings>(key: K, value: Settings[K] | undefined) {
  const settings = own();
  if (value === undefined) delete settings[key];
  else settings[key] = value;
}

function source(from: string) {
  return from ? `from ${from}` : "default";
}

function onOff(value: boolean | undefined, fallback: boolean) {
  return (value ?? fallback) ? "On" : "Off";
}

// ---------------------------------------------------------------- timeout

const timeoutText = computed({
  get: () => (props.request.settings?.timeoutMs ?? "").toString(),
  set: (text: string) => {
    const n = Number(text);
    set("timeoutMs", text.trim() && Number.isInteger(n) && n > 0 ? n : undefined);
  },
});
const timeoutPlaceholder = computed(() => {
  const i = props.inherited.timeoutMs;
  return `${i.value ?? DEFAULT_TIMEOUT_MS} (${source(i.from)})`;
});

// ---------------------------------------------------------------- booleans

function choice(key: "verifySsl" | "followRedirects") {
  return computed({
    get: () => {
      const v = props.request.settings?.[key];
      return v === undefined ? "inherit" : v ? "on" : "off";
    },
    set: (v: string) => set(key, v === "inherit" ? undefined : v === "on"),
  });
}
const verify = choice("verifySsl");
const follow = choice("followRedirects");
const verifyEffective = computed(() => props.request.settings?.verifySsl ?? props.inherited.verifySsl.value ?? true);

// ---------------------------------------------------------------- proxy

const proxyMode = computed({
  get: () => {
    const p = props.request.settings?.proxy;
    if (p === undefined) return "inherit";
    return p === "system" || p === "none" ? p : "custom";
  },
  set: (mode: string) => {
    if (mode === "inherit") set("proxy", undefined);
    else if (mode === "custom") set("proxy", props.request.settings?.proxy?.includes("://") ? props.request.settings.proxy : "http://");
    else set("proxy", mode);
  },
});
const inheritedProxy = computed(() => {
  const i = props.inherited.proxy;
  const value = i.value ?? "system";
  return `${value === "system" ? "System" : value === "none" ? "None" : value} (${source(i.from)})`;
});

// ---------------------------------------------------------------- certificates

const certificates = ref<ClientCertificate[]>([]);
const showCertificates = ref(false);
async function loadCertificates() {
  try {
    certificates.value = props.root ? await backend.listCertificates(props.root) : [];
  } catch {
    certificates.value = [];
  }
}
onMounted(loadCertificates);
const matched = computed(() => certificateFor(certificates.value, props.resolvedUrl));

</script>

<template>
  <div class="settings">
    <p v-if="!backend.persistent" class="note">
      The browser preview only applies the timeout; SSL, redirects, proxy and certificates are applied by the desktop app.
    </p>

    <div class="row">
      <label for="set-timeout">Timeout (ms)</label>
      <input id="set-timeout" v-model="timeoutText" inputmode="numeric" :placeholder="timeoutPlaceholder" spellcheck="false" />
    </div>

    <div class="row">
      <label for="set-verify">Verify SSL certificates</label>
      <select id="set-verify" v-model="verify">
        <option value="inherit">Inherit: {{ onOff(inherited.verifySsl.value, true) }} ({{ source(inherited.verifySsl.from) }})</option>
        <option value="on">On</option>
        <option value="off">Off</option>
      </select>
    </div>
    <p v-if="!verifyEffective" class="warn">
      Any server certificate is accepted, so traffic could be intercepted. Use it only against your own test servers.
    </p>

    <div class="row">
      <label for="set-follow">Follow redirects</label>
      <select id="set-follow" v-model="follow">
        <option value="inherit">Inherit: {{ onOff(inherited.followRedirects.value, true) }} ({{ source(inherited.followRedirects.from) }})</option>
        <option value="on">On (up to 10)</option>
        <option value="off">Off</option>
      </select>
    </div>

    <div class="row">
      <label for="set-proxy">Proxy</label>
      <div class="proxy">
        <select id="set-proxy" v-model="proxyMode">
          <option value="inherit">Inherit: {{ inheritedProxy }}</option>
          <option value="system">System (HTTP_PROXY…)</option>
          <option value="none">None</option>
          <option value="custom">Custom</option>
        </select>
        <input
          v-if="proxyMode === 'custom'"
          :value="request.settings?.proxy ?? ''"
          placeholder="http://user:{{proxyPassword}}@proxy.corp:3128"
          spellcheck="false"
          @input="set('proxy', ($event.target as HTMLInputElement).value)"
        />
      </div>
    </div>

    <section class="certs">
      <h4>Client certificate (mTLS)</h4>
      <p v-if="matched" class="hint">
        Uses the certificate for <code>{{ matched.host }}</code> ({{ matched.pfx ? "PFX" : "PEM" }}) set up on this machine.
      </p>
      <p v-else class="hint">No certificate set up for this host on this machine.</p>
      <button class="link" @click="showCertificates = true">Manage certificates…</button>
    </section>

    <p class="hint">
      Empty fields inherit from the folders and the workspace. Saved in the request file, so the team shares them;
      certificates stay on each machine.
    </p>

    <CertificatesPanel v-if="showCertificates" :root="root" @close="showCertificates = false" @saved="loadCertificates" />
  </div>
</template>

<style scoped>
.settings {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 160px;
}
.row {
  display: grid;
  grid-template-columns: 170px minmax(0, 1fr);
  gap: 8px;
  align-items: center;
  max-width: 640px;
}
@container (max-width: 460px) {
  .row {
    grid-template-columns: minmax(0, 1fr);
    gap: 4px;
  }
}
label {
  color: var(--muted);
  font-size: 12px;
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
  cursor: pointer;
}
input:focus,
select:focus {
  border-color: var(--accent);
}
.proxy {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.proxy input {
  flex: 1 1 220px;
}
.certs {
  margin-top: 6px;
  border-top: 1px solid var(--border);
  padding-top: 8px;
}
h4 {
  margin: 0 0 4px;
  font-size: 12px;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}
.note,
.hint,
.warn {
  margin: 0;
  font-size: 12px;
  max-width: 640px;
}
.note,
.hint {
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.link {
  border: 0;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  padding: 4px 0;
}
</style>
