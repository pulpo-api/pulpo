<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  AUTH_LABELS,
  OAUTH_DESKTOP_ONLY,
  OAUTH_GRANTS,
  insideTauri,
  newOAuth2,
  oauthDiscover,
  oauthForget,
  oauthLogin,
  oauthStatus,
  type Auth,
  type OAuthAuth,
  type Settings,
  type TokenInfo,
} from "../core";
import type { InheritedAuth } from "../workspace";
import VarInput from "./VarInput.vue";

/** Edits an auth. `null` = inherit (only offered when `inherited` is given). */
const auth = defineModel<Auth | null>({ required: true });
const props = defineProps<{
  inherited?: InheritedAuth;
  /** Workspace and environment, for OAuth 2.0 tokens. */
  root?: string;
  environmentId?: string;
  /** Settings for token requests (SSL, proxy, timeout). */
  settings?: Settings;
}>();

type Choice = Auth["type"] | "inherit";

const choice = computed<Choice>({
  get: () => auth.value?.type ?? (props.inherited ? "inherit" : "none"),
  set: (type) => {
    // Keep what the user typed when switching between kinds that share fields.
    const old = auth.value;
    const token = old?.type === "bearer" ? old.token : old?.type === "apiKey" ? old.value : "";
    switch (type) {
      case "inherit":
        auth.value = null;
        break;
      case "none":
        auth.value = { type: "none" };
        break;
      case "bearer":
        auth.value = { type: "bearer", token: token || "{{token}}" };
        break;
      case "basic":
        auth.value = { type: "basic", username: "", password: "{{password}}" };
        break;
      case "apiKey":
        auth.value = { type: "apiKey", key: "X-API-Key", value: token || "{{apiKey}}", in: "header" };
        break;
      case "oauth2":
        auth.value = newOAuth2();
        break;
      case "digest":
        auth.value = { type: "digest", username: "{{username}}", password: "{{password}}" };
        break;
      case "awsV4":
        auth.value = {
          type: "awsV4",
          accessKey: "{{awsAccessKey}}",
          secretKey: "{{awsSecretKey}}",
          sessionToken: "",
          region: "us-east-1",
          service: "execute-api",
        };
        break;
      case "hawk":
        auth.value = { type: "hawk", id: "{{hawkId}}", key: "{{hawkKey}}", algorithm: "sha256", ext: "", payload: false };
        break;
      case "ntlm":
        auth.value = { type: "ntlm", username: "{{username}}", password: "{{password}}", domain: "", workstation: "" };
        break;
    }
  },
});

const isLiteral = (value: string) => value.trim() !== "" && !value.includes("{{");

/** Secret-looking values typed literally end up in git. */
const literalSecret = computed(() => {
  const a = auth.value;
  switch (a?.type) {
    case "bearer":
      return isLiteral(a.token);
    case "basic":
      return isLiteral(a.password);
    case "apiKey":
      return isLiteral(a.value);
    case "oauth2":
      return isLiteral(a.clientSecret) || isLiteral(a.password);
    case "digest":
    case "ntlm":
      return isLiteral(a.password);
    case "awsV4":
      return isLiteral(a.secretKey) || isLiteral(a.sessionToken);
    case "hawk":
      return isLiteral(a.key);
    default:
      return false;
  }
});

const shown = computed(() => (auth.value === null && props.inherited ? props.inherited.auth : auth.value));
const inheritedFrom = computed(() => {
  const i = props.inherited;
  if (!i) return "";
  return i.sourceId ? `folder “${i.sourceName}”` : `the workspace (pulpo.yaml)`;
});

// --- OAuth 2.0 -------------------------------------------------------------------------

/** The OAuth 2.0 auth in use (own or inherited), for the token status. */
const oauth = computed<OAuthAuth | null>(() => (shown.value?.type === "oauth2" ? shown.value : null));
const status = ref<TokenInfo | null>(null);
const busy = ref(false);
const problem = ref("");
const issuer = ref("");
const now = ref(Date.now() / 1000);
const clock = setInterval(() => (now.value = Date.now() / 1000), 15_000);
onBeforeUnmount(() => clearInterval(clock));

let pending: ReturnType<typeof setTimeout> | undefined;
async function refreshStatus() {
  const config = oauth.value;
  if (!config || !props.root) {
    status.value = null;
    return;
  }
  try {
    status.value = await oauthStatus(props.root, props.environmentId ?? "", config);
  } catch {
    // Missing variables: there cannot be a token for it yet.
    status.value = null;
  }
}
watch(
  [oauth, () => props.root, () => props.environmentId],
  () => {
    clearTimeout(pending);
    pending = setTimeout(refreshStatus, 300);
  },
  { deep: true, immediate: true },
);
onBeforeUnmount(() => clearTimeout(pending));

const expiry = computed(() => {
  const s = status.value;
  if (!s) return "";
  if (!s.valid) return s.hasRefresh ? "expired; refreshed on the next send" : "expired";
  if (s.expiresAt == null) return "valid, no expiry given";
  const minutes = Math.round((s.expiresAt - now.value) / 60);
  return minutes >= 1 ? `valid for ${minutes} min` : "valid for less than a minute";
});

async function getToken() {
  const config = oauth.value;
  if (!config || !props.root) return;
  busy.value = true;
  problem.value = "";
  try {
    status.value = await oauthLogin(props.root, props.environmentId ?? "", config, props.settings);
  } catch (e) {
    problem.value = e instanceof Error ? e.message : String(e);
  } finally {
    busy.value = false;
  }
}

async function forget() {
  const config = oauth.value;
  if (!config || !props.root) return;
  await oauthForget(props.root, props.environmentId ?? "", config);
  status.value = null;
}

async function discover() {
  const a = auth.value;
  if (a?.type !== "oauth2" || !issuer.value.trim()) return;
  problem.value = "";
  try {
    const found = await oauthDiscover(issuer.value.trim());
    if (found.authorizationEndpoint) a.authUrl = found.authorizationEndpoint;
    a.tokenUrl = found.tokenEndpoint;
    if (!a.scope && found.scopesSupported.includes("openid")) a.scope = "openid";
  } catch (e) {
    problem.value = `OpenID discovery: ${e instanceof Error ? e.message : String(e)}`;
  }
}

const getLabel = computed(() =>
  busy.value
    ? oauth.value?.grant === "authorizationCode"
      ? "Waiting for the browser…"
      : "Getting a token…"
    : "Get new access token",
);
</script>

<template>
  <div class="auth">
    <div class="row">
      <label for="auth-type">Type</label>
      <select id="auth-type" v-model="choice">
        <option v-if="inherited" value="inherit">Inherit from parent</option>
        <option v-for="(label, type) in AUTH_LABELS" :key="type" :value="type">{{ label }}</option>
      </select>
    </div>

    <p v-if="auth === null && inherited" class="note">
      <template v-if="inherited.found">
        Uses <b>{{ AUTH_LABELS[inherited.auth.type] }}</b> from {{ inheritedFrom }}.
      </template>
      <template v-else>Nothing up the tree sets an auth, so none is sent.</template>
    </p>

    <template v-if="auth?.type === 'bearer'">
      <div class="row">
        <label for="auth-token">Token</label>
        <VarInput id="auth-token" v-model="auth.token" placeholder="{{token}}" aria-label="Token" />
      </div>
    </template>

    <template v-else-if="auth?.type === 'basic'">
      <div class="row">
        <label for="auth-user">Username</label>
        <VarInput id="auth-user" v-model="auth.username" aria-label="Username" />
      </div>
      <div class="row">
        <label for="auth-pass">Password</label>
        <VarInput id="auth-pass" v-model="auth.password" placeholder="{{password}}" aria-label="Password" />
      </div>
    </template>

    <template v-else-if="auth?.type === 'apiKey'">
      <div class="row">
        <label for="auth-key">Key</label>
        <input id="auth-key" v-model="auth.key" spellcheck="false" placeholder="X-API-Key" />
      </div>
      <div class="row">
        <label for="auth-value">Value</label>
        <VarInput id="auth-value" v-model="auth.value" placeholder="{{apiKey}}" aria-label="API key value" />
      </div>
      <div class="row">
        <label for="auth-in">Add to</label>
        <select id="auth-in" v-model="auth.in">
          <option value="header">Header</option>
          <option value="query">Query params</option>
        </select>
      </div>
    </template>

    <template v-else-if="auth?.type === 'digest' || auth?.type === 'ntlm'">
      <div class="row">
        <label for="auth-user">Username</label>
        <VarInput id="auth-user" v-model="auth.username" placeholder="{{username}}" aria-label="Username" />
      </div>
      <div class="row">
        <label for="auth-pass">Password</label>
        <VarInput id="auth-pass" v-model="auth.password" placeholder="{{password}}" aria-label="Password" />
      </div>
      <template v-if="auth.type === 'ntlm'">
        <div class="row">
          <label for="auth-domain">Domain</label>
          <VarInput id="auth-domain" v-model="auth.domain" placeholder="optional" aria-label="Domain" />
        </div>
        <div class="row">
          <label for="auth-workstation">Workstation</label>
          <VarInput id="auth-workstation" v-model="auth.workstation" placeholder="optional" aria-label="Workstation" />
        </div>
      </template>
    </template>

    <template v-else-if="auth?.type === 'awsV4'">
      <div class="row">
        <label for="aws-access">Access key</label>
        <VarInput id="aws-access" v-model="auth.accessKey" placeholder="{{awsAccessKey}}" aria-label="Access key" />
      </div>
      <div class="row">
        <label for="aws-secret">Secret key</label>
        <VarInput id="aws-secret" v-model="auth.secretKey" placeholder="{{awsSecretKey}}" aria-label="Secret key" />
      </div>
      <div class="row">
        <label for="aws-region">Region</label>
        <VarInput id="aws-region" v-model="auth.region" placeholder="us-east-1" aria-label="Region" />
      </div>
      <div class="row">
        <label for="aws-service">Service</label>
        <VarInput id="aws-service" v-model="auth.service" placeholder="execute-api, s3, lambda…" aria-label="Service" />
      </div>
      <div class="row">
        <label for="aws-session">Session token</label>
        <VarInput
          id="aws-session"
          v-model="auth.sessionToken"
          placeholder="only for temporary credentials"
          aria-label="Session token"
        />
      </div>
    </template>

    <template v-else-if="auth?.type === 'hawk'">
      <div class="row">
        <label for="hawk-id">Hawk ID</label>
        <VarInput id="hawk-id" v-model="auth.id" placeholder="{{hawkId}}" aria-label="Hawk ID" />
      </div>
      <div class="row">
        <label for="hawk-key">Hawk key</label>
        <VarInput id="hawk-key" v-model="auth.key" placeholder="{{hawkKey}}" aria-label="Hawk key" />
      </div>
      <div class="row">
        <label for="hawk-alg">Algorithm</label>
        <select id="hawk-alg" v-model="auth.algorithm">
          <option value="sha256">SHA-256</option>
          <option value="sha1">SHA-1</option>
        </select>
      </div>
      <div class="row">
        <label for="hawk-ext">Ext</label>
        <VarInput id="hawk-ext" v-model="auth.ext" placeholder="optional app data" aria-label="Ext" />
      </div>
      <div class="row">
        <label for="hawk-payload">Body</label>
        <label class="check"><input id="hawk-payload" v-model="auth.payload" type="checkbox" /> Sign the body too</label>
      </div>
    </template>

    <template v-else-if="auth?.type === 'oauth2'">
      <div class="row">
        <label for="oauth-grant">Grant</label>
        <select id="oauth-grant" v-model="auth.grant">
          <option v-for="(label, grant) in OAUTH_GRANTS" :key="grant" :value="grant">{{ label }}</option>
        </select>
      </div>
      <div class="row">
        <label for="oauth-issuer">Issuer</label>
        <div class="inline">
          <input
            id="oauth-issuer"
            v-model="issuer"
            spellcheck="false"
            placeholder="https://login.example.com (fills the URLs)"
            @keydown.enter.prevent="discover"
          />
          <button type="button" :disabled="!issuer.trim()" @click="discover">Discover</button>
        </div>
      </div>
      <div v-if="auth.grant === 'authorizationCode'" class="row">
        <label for="oauth-auth-url">Auth URL</label>
        <VarInput id="oauth-auth-url" v-model="auth.authUrl" placeholder="https://…/authorize" aria-label="Authorization URL" />
      </div>
      <div class="row">
        <label for="oauth-token-url">Token URL</label>
        <VarInput id="oauth-token-url" v-model="auth.tokenUrl" placeholder="https://…/token" aria-label="Token URL" />
      </div>
      <div class="row">
        <label for="oauth-client-id">Client ID</label>
        <VarInput id="oauth-client-id" v-model="auth.clientId" placeholder="{{clientId}}" aria-label="Client ID" />
      </div>
      <div class="row">
        <label for="oauth-client-secret">Client secret</label>
        <VarInput
          id="oauth-client-secret"
          v-model="auth.clientSecret"
          placeholder="{{clientSecret}} (empty for public clients)"
          aria-label="Client secret"
        />
      </div>
      <div class="row">
        <label for="oauth-scope">Scope</label>
        <VarInput id="oauth-scope" v-model="auth.scope" placeholder="openid profile api" aria-label="Scope" />
      </div>
      <template v-if="auth.grant === 'password'">
        <div class="row">
          <label for="oauth-user">Username</label>
          <VarInput id="oauth-user" v-model="auth.username" aria-label="Username" />
        </div>
        <div class="row">
          <label for="oauth-pass">Password</label>
          <VarInput id="oauth-pass" v-model="auth.password" placeholder="{{password}}" aria-label="Password" />
        </div>
      </template>
      <details class="advanced">
        <summary>Advanced</summary>
        <div v-if="auth.grant === 'authorizationCode'" class="row">
          <label for="oauth-redirect">Redirect URI</label>
          <VarInput
            id="oauth-redirect"
            v-model="auth.redirectUri"
            placeholder="automatic: http://127.0.0.1:<free port>/callback"
            aria-label="Redirect URI"
          />
        </div>
        <div v-if="auth.grant === 'authorizationCode'" class="row">
          <label for="oauth-pkce">PKCE</label>
          <label class="check"><input id="oauth-pkce" v-model="auth.pkce" type="checkbox" /> Send a code challenge (S256)</label>
        </div>
        <div class="row">
          <label for="oauth-client-auth">Client auth</label>
          <select id="oauth-client-auth" v-model="auth.clientAuth">
            <option value="header">Basic auth header</option>
            <option value="body">In the request body</option>
          </select>
        </div>
        <div class="row">
          <label for="oauth-prefix">Header prefix</label>
          <input id="oauth-prefix" v-model="auth.headerPrefix" spellcheck="false" placeholder="Bearer" />
        </div>
      </details>
    </template>

    <div v-if="oauth" class="token">
      <span v-if="!insideTauri" class="muted">{{ OAUTH_DESKTOP_ONLY }}</span>
      <template v-else>
        <span v-if="status" :class="status.valid ? 'ok' : 'muted'">
          Token <code>{{ status.preview }}…</code> {{ expiry }}<template v-if="environmentId === ''"> (no environment)</template>
        </span>
        <span v-else class="muted">
          <template v-if="oauth.grant === 'authorizationCode'">No token yet: log in to get one.</template>
          <template v-else>No token yet: Pulpo gets one when you send.</template>
        </span>
        <span class="actions">
          <button type="button" class="primary" :disabled="busy || !root" @click="getToken">{{ getLabel }}</button>
          <button v-if="status" type="button" :disabled="busy" @click="forget">Forget</button>
        </span>
      </template>
    </div>
    <p v-if="problem" class="warn">{{ problem }}</p>

    <p v-if="literalSecret" class="warn">
      This value is saved as plain text in the request file, so it ends up in git. Use a variable such as
      <code v-pre>{{token}}</code> and mark it secret in the environment: its value then lives in the OS keychain.
    </p>
    <p v-else-if="shown && shown.type === 'oauth2'" class="hint">
      Sent as <code>Authorization: {{ shown.headerPrefix || "Bearer" }} …</code>. Tokens are kept per environment in
      the OS keychain (never in git) and refreshed when they expire; a header with the same name in the
      Headers tab wins.
    </p>
    <p v-else-if="shown && (shown.type === 'digest' || shown.type === 'ntlm')" class="hint">
      Pulpo sends the request, reads the server's <code>WWW-Authenticate</code> challenge and sends it again with the
      answer<template v-if="shown.type === 'ntlm'"> on the same connection (HTTP/1.1)</template>. A header named
      Authorization in the Headers tab wins.
    </p>
    <p v-else-if="shown && (shown.type === 'awsV4' || shown.type === 'hawk')" class="hint">
      Signed when sent, over the method, URL, headers<template v-if="shown.type === 'awsV4' || shown.payload">
      and body</template>. A header named Authorization in the Headers tab wins.
    </p>
    <p v-else-if="shown && shown.type !== 'none'" class="hint">
      Sent as
      <code v-if="shown.type === 'bearer'">Authorization: Bearer …</code>
      <code v-else-if="shown.type === 'basic'">Authorization: Basic …</code>
      <code v-else-if="shown.type === 'apiKey' && shown.in === 'query'">?{{ shown.key }}=…</code>
      <code v-else-if="shown.type === 'apiKey'">{{ shown.key }}: …</code><template v-if="shown.type !== 'apiKey' || shown.in === 'header'">;
      a header with the same name in the Headers tab wins</template>.
    </p>
  </div>
</template>

<style scoped>
.auth {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 160px;
}
.row {
  display: grid;
  grid-template-columns: 90px minmax(0, 1fr);
  gap: 8px;
  align-items: center;
  max-width: 560px;
}
label {
  color: var(--muted);
  font-size: 12px;
}
input,
select {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 13px;
  outline: none;
  min-width: 0;
}
select {
  font-family: inherit;
  cursor: pointer;
}
input:focus,
select:focus {
  border-color: var(--accent);
}
input[type="checkbox"] {
  padding: 0;
}
.check {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--text);
}
.inline {
  display: flex;
  gap: 6px;
}
.inline input {
  flex: 1;
}
button {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  color: var(--text);
  padding: 5px 10px;
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}
button.primary {
  background: var(--accent);
  border-color: var(--accent);
  color: #fff;
}
button:disabled {
  opacity: 0.6;
  cursor: default;
}
.advanced {
  max-width: 560px;
}
.advanced summary {
  cursor: pointer;
  color: var(--muted);
  font-size: 12px;
  margin-bottom: 8px;
}
.advanced .row + .row {
  margin-top: 8px;
}
.token {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 12px;
  max-width: 640px;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  font-size: 12px;
}
.token .actions {
  display: flex;
  gap: 6px;
  margin-left: auto;
}
.ok {
  color: var(--text);
}
.muted {
  color: var(--muted);
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
</style>
