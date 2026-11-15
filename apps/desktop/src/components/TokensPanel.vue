<script setup lang="ts">
import { t } from "../i18n";
import { computed, onMounted, onUnmounted, ref } from "vue";
import { OAUTH_DESKTOP_ONLY, OAUTH_GRANTS, oauthForgetKey, oauthRenew, oauthRevoke, oauthTokens, type ActiveToken } from "../core";
import { backend } from "../workspace";

const props = defineProps<{
  root: string;
  environmentId: string;
  environmentName: string;
  /** Request id -> name, to show who uses each token. */
  names: Record<string, string>;
}>();
const emit = defineEmits<{ close: []; open: [id: string] }>();

const tokens = ref<ActiveToken[]>([]);
const loaded = ref(false);
const problem = ref("");
const note = ref("");
const busy = ref("");
const armed = ref("");
const now = ref(Date.now() / 1000);
const timer = setInterval(() => (now.value = Date.now() / 1000), 15_000);
onUnmounted(() => clearInterval(timer));

async function load() {
  try {
    tokens.value = await oauthTokens(props.root, props.environmentId);
    problem.value = "";
  } catch (e) {
    problem.value = e instanceof Error ? e.message : String(e);
  }
  loaded.value = true;
}
onMounted(load);

function expiry(token: ActiveToken): { text: string; state: "ok" | "soon" | "expired" | "unknown" } {
  if (token.expiresAt === null) return { text: t("No expiry given"), state: "unknown" };
  const left = token.expiresAt - now.value;
  if (left <= 30) return { text: "Expired", state: "expired" };
  const minutes = Math.floor(left / 60);
  const text =
    minutes >= 120 ? `${Math.floor(minutes / 60)} h left` : minutes >= 1 ? `${minutes} min left` : `${Math.floor(left)} s left`;
  return { text, state: left < 300 ? "soon" : "ok" };
}

const sorted = computed(() => tokens.value);

async function act(token: ActiveToken, action: "renew" | "revoke" | "forget") {
  if ((action === "revoke" || action === "forget") && armed.value !== `${action}:${token.key}`) {
    armed.value = `${action}:${token.key}`;
    return;
  }
  armed.value = "";
  busy.value = `${action}:${token.key}`;
  problem.value = "";
  note.value = "";
  try {
    if (action === "renew") {
      const info = await oauthRenew(props.root, props.environmentId, token.key);
      note.value = t("Got a new access token ({0}…).", { 0: info.preview });
    } else if (action === "revoke") {
      const result = await oauthRevoke(props.root, props.environmentId, token.key);
      note.value = result ? t("Forgotten: {0}.", { 0: result }) : t("Revoked at the identity provider and forgotten.");
    } else {
      await oauthForgetKey(props.root, props.environmentId, token.key);
      note.value = t("Forgotten on this machine.");
    }
  } catch (e) {
    problem.value = e instanceof Error ? e.message : String(e);
  }
  busy.value = "";
  await load();
}

function shortUrl(url: string): string {
  return url.replace(/^https?:\/\//, "");
}
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card panel" role="dialog" :aria-label="$t('OAuth 2.0 tokens')">
      <header>
        <div>
          <h2>{{ $t("OAuth 2.0 tokens") }}</h2>
          <p class="sub">
            {{ $t("Tokens of") }} <b>{{ environmentName || $t("No environment") }}</b>{{ $t(", kept in the OS keychain on this machine, never in git.") }}
          </p>
        </div>
        <button class="ghost" :title="$t('Close')" @click="emit('close')">✕</button>
      </header>

      <p v-if="!backend.persistent" class="note">{{ OAUTH_DESKTOP_ONLY }}</p>
      <p v-if="problem" class="warn">{{ problem }}</p>
      <p v-if="note" class="note ok" role="status">{{ note }}</p>

      <div class="list">
        <p v-if="loaded && !sorted.length" class="empty">
          {{ $t("No tokens yet. Send a request with OAuth 2.0 auth, or press") }} <i>{{ $t("Get new access token") }}</i> {{ $t("in its Auth tab.") }}
        </p>
        <article v-for="token in sorted" :key="token.key" class="token">
          <div class="top">
            <code class="preview" :title="`Cache key ${token.key}`">{{ token.preview }}…</code>
            <span class="state" :class="expiry(token).state">{{ expiry(token).text }}</span>
            <span class="meta">{{ OAUTH_GRANTS[token.grant] }}</span>
            <span v-if="token.hasRefresh" class="flag" :title="$t('Pulpo renews it on its own when it expires')">{{ $t("refresh") }}</span>
            <span class="actions">
              <button
                v-if="token.canRenew"
                class="ghost"
                :disabled="!!busy"
                :title="token.hasRefresh ? $t('Use the refresh token now') : $t('Get a new token now')"
                @click="act(token, 'renew')"
              >
                {{ busy === `renew:${token.key}` ? $t("Renewing…") : $t("Renew") }}
              </button>
              <button
                v-if="token.canRevoke"
                class="ghost danger"
                :class="{ armed: armed === `revoke:${token.key}` }"
                :disabled="!!busy"
                :title="$t('Ask the identity provider to revoke it, then forget it')"
                @click="act(token, 'revoke')"
              >
                {{ armed === `revoke:${token.key}` ? $t("Click again to revoke") : $t("Revoke") }}
              </button>
              <button
                class="ghost"
                :class="{ armed: armed === `forget:${token.key}` }"
                :disabled="!!busy"
                :title="$t('Delete it from this machine only; it stays valid at the identity provider')"
                @click="act(token, 'forget')"
              >
                {{ armed === `forget:${token.key}` ? $t("Click again to forget") : $t("Forget") }}
              </button>
            </span>
          </div>
          <div class="details">
            <span>{{ $t("client") }} <code>{{ token.clientId || "—" }}</code></span>
            <span v-if="token.scope">{{ $t("scope") }} <code>{{ token.scope }}</code></span>
            <span :title="token.tokenUrl">{{ shortUrl(token.tokenUrl) }}</span>
            <span>{{ $t("got {0}", { 0: new Date(token.obtainedAt * 1000).toLocaleString() }) }}</span>
          </div>
          <div class="used">
            <template v-if="token.usedBy.length">
              {{ $t("Used by") }}
              <button v-for="id in token.usedBy" :key="id" class="link" @click="emit('open', id)">
                {{ names[id] ?? id }}
              </button>
            </template>
            <template v-else>{{ $t("No request uses these settings any more: it can only be forgotten.") }}</template>
          </div>
        </article>
      </div>
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
  width: min(820px, 94vw);
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
.note.ok {
  color: var(--m-get);
}
.list {
  overflow: auto;
  min-height: 120px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.token {
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 8px 10px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.top {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.preview {
  font-size: 13px;
}
.state {
  font-size: 12px;
  font-weight: 600;
}
.state.ok {
  color: var(--m-get);
}
.state.soon {
  color: var(--m-post);
}
.state.expired {
  color: var(--m-delete);
}
.state.unknown,
.meta {
  font-size: 12px;
  color: var(--muted);
}
.flag {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  background: var(--surface-2);
  color: var(--muted);
}
.actions {
  margin-left: auto;
  display: flex;
  gap: 6px;
}
.details,
.used {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 12px;
  font-size: 12px;
  color: var(--muted);
  overflow-wrap: anywhere;
}
.used {
  gap: 4px 8px;
  align-items: center;
}
.link {
  background: none;
  border: none;
  padding: 0;
  color: var(--accent);
  cursor: pointer;
  font-size: 12px;
}
.link:hover {
  text-decoration: underline;
}
.armed {
  border-color: var(--m-delete);
}
@container (max-width: 520px) {
  .actions {
    margin-left: 0;
    width: 100%;
  }
}
</style>
