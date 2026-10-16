<script setup lang="ts">
import { computed } from "vue";
import { AUTH_LABELS, type Auth } from "../core";
import type { InheritedAuth } from "../workspace";
import VarInput from "./VarInput.vue";

/** Edits an auth. `null` = inherit (only offered when `inherited` is given). */
const auth = defineModel<Auth | null>({ required: true });
const props = defineProps<{ inherited?: InheritedAuth }>();

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
    }
  },
});

/** Secret-looking values typed literally end up in git. */
const literalSecret = computed(() => {
  const a = auth.value;
  const secret = a?.type === "bearer" ? a.token : a?.type === "basic" ? a.password : a?.type === "apiKey" ? a.value : "";
  return secret.trim() !== "" && !secret.includes("{{");
});

const shown = computed(() => (auth.value === null && props.inherited ? props.inherited.auth : auth.value));
const inheritedFrom = computed(() => {
  const i = props.inherited;
  if (!i) return "";
  return i.sourceId ? `folder “${i.sourceName}”` : `the workspace (pulpo.yaml)`;
});
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

    <p v-if="literalSecret" class="warn">
      This value is saved as plain text in the request file, so it ends up in git. Use a variable such as
      <code v-pre>{{token}}</code> and mark it secret in the environment: its value then lives in the OS keychain.
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
}
select {
  font-family: inherit;
  cursor: pointer;
}
input:focus,
select:focus {
  border-color: var(--accent);
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
