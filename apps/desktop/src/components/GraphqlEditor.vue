<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import type { HttpRequest } from "../core";
import { backend, type GraphqlSchema, type Scripts } from "../workspace";
import CodeEditor from "./CodeEditor.vue";
import { operationType, subscribe, toWsUrl, type SubEvent, type Subscription } from "../graphqlSubscription";

const props = defineProps<{
  request: HttpRequest;
  root: string;
  requestId: string;
  scripts: Scripts;
  environmentId: string;
}>();
const emit = defineEmits<{ send: []; save: [] }>();

const graphql = computed(() => props.request.graphql!);
const query = computed({
  get: () => graphql.value.query,
  set: (v: string) => (graphql.value.query = v),
});
const variables = computed({
  get: () => graphql.value.variables,
  set: (v: string) => (graphql.value.variables = v),
});
const operationName = computed({
  get: () => graphql.value.operationName ?? "",
  set: (v: string) => (graphql.value.operationName = v.trim() ? v : null),
});

// ---------------------------------------------------------------- schema

const fetched = ref<GraphqlSchema | null>(null);
/** graphql-js schema for completion and checks (kept out of Vue's reactivity). */
const schema = shallowRef<unknown>(null);
const loading = ref(false);
const problem = ref("");
const showDocs = ref(false);
const search = ref("");
const canFetch = computed(() => !!backend.graphqlSchema && !!props.root);

const variablesProblem = computed(() => {
  const text = variables.value.trim();
  if (!text) return "";
  // `{{name}}` is filled in before sending; check the rest.
  try {
    JSON.parse(text.replace(/"?\{\{[^}]+\}\}"?/g, '"x"'));
    return "";
  } catch (e) {
    return `Variables are not valid JSON: ${(e as Error).message}`;
  }
});

/** Operations in the query, so a document with several can pick one. */
const operations = computed(() =>
  [...query.value.matchAll(/\b(query|mutation|subscription)\s+([_A-Za-z][_0-9A-Za-z]*)/g)].map((m) => m[2]),
);

// ---------------------------------------------------------------- subscriptions

const isSubscription = computed(() => operationType(query.value, operationName.value) === "subscription");
const subState = ref<"idle" | "starting" | "on">("idle");
const events = ref<SubEvent[]>([]);
const openEvents = ref<Set<number>>(new Set());
let subscription: Subscription | null = null;

async function startSubscription() {
  if (!backend.wsConnect) return;
  problem.value = "";
  subState.value = "starting";
  events.value = [];
  openEvents.value = new Set();
  try {
    subscription = await subscribe(
      props.root,
      { id: props.requestId, name: "", protocol: "graphql", request: props.request, scripts: props.scripts },
      props.environmentId,
      (event) => {
        events.value.push(event);
        if (event.kind === "done" || event.kind === "error" || (event.kind === "info" && event.text.startsWith("Closed"))) {
          if (event.kind !== "error" || event.text.startsWith("could not")) subState.value = "idle";
        }
      },
    );
    subState.value = "on";
  } catch (e) {
    problem.value = String(e);
    subState.value = "idle";
  }
}

async function stopSubscription() {
  await subscription?.stop();
  subscription = null;
  subState.value = "idle";
}

function toggleEvent(i: number) {
  const next = new Set(openEvents.value);
  if (next.has(i)) next.delete(i);
  else next.add(i);
  openEvents.value = next;
}

function prettyJson(text: string) {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

onUnmounted(() => void subscription?.stop());

async function useSchema(result: GraphqlSchema | null) {
  fetched.value = result;
  if (!result) return;
  const { buildClientSchema } = await import("graphql");
  try {
    schema.value = buildClientSchema(JSON.parse(result.introspection));
  } catch (e) {
    problem.value = `The schema could not be read: ${(e as Error).message}`;
  }
}

function entry() {
  return { id: props.requestId, name: "", protocol: "graphql" as const, request: props.request, scripts: props.scripts };
}

async function load(refresh: boolean) {
  if (!backend.graphqlSchema) return;
  loading.value = refresh;
  problem.value = "";
  try {
    await useSchema(await backend.graphqlSchema(props.root, entry(), props.environmentId, refresh));
    if (refresh) showDocs.value = true;
  } catch (e) {
    problem.value = String(e);
  }
  loading.value = false;
}

onMounted(() => void load(false));
watch(
  () => props.request.url,
  () => {
    schema.value = null;
    fetched.value = null;
    void load(false);
  },
);

const fetchedWhen = computed(() => {
  if (!fetched.value) return "";
  const at = new Date(fetched.value.fetchedAt * 1000);
  return at.toLocaleString();
});

// ---------------------------------------------------------------- docs

interface TypeRef {
  kind: string;
  name: string | null;
  ofType: TypeRef | null;
}
interface Field {
  name: string;
  description?: string | null;
  args?: { name: string; type: TypeRef; description?: string | null }[];
  type?: TypeRef;
  isDeprecated?: boolean;
}
interface FullType {
  kind: string;
  name: string;
  description?: string | null;
  fields?: Field[] | null;
  inputFields?: Field[] | null;
  enumValues?: Field[] | null;
}

const types = computed<FullType[]>(() => {
  if (!fetched.value) return [];
  try {
    return (JSON.parse(fetched.value.introspection).__schema.types as FullType[]).filter((t) => !t.name.startsWith("__"));
  } catch {
    return [];
  }
});
const roots = computed(() => {
  if (!fetched.value) return [] as string[];
  try {
    const s = JSON.parse(fetched.value.introspection).__schema;
    return [s.queryType?.name, s.mutationType?.name, s.subscriptionType?.name].filter(Boolean) as string[];
  } catch {
    return [];
  }
});
const current = ref("");

function typeText(t: TypeRef | null | undefined): string {
  if (!t) return "";
  if (t.kind === "NON_NULL") return `${typeText(t.ofType)}!`;
  if (t.kind === "LIST") return `[${typeText(t.ofType)}]`;
  return t.name ?? "";
}
function namedType(t: TypeRef | null | undefined): string {
  while (t && !t.name) t = t.ofType;
  return t?.name ?? "";
}
function membersOf(t: FullType): Field[] {
  return t.fields ?? t.inputFields ?? t.enumValues ?? [];
}

/** What the docs list: a search over every type and field, else one type, else the roots. */
const docs = computed(() => {
  const q = search.value.trim().toLowerCase();
  if (q) {
    const hits: { type: FullType; field: Field | null }[] = [];
    for (const t of types.value) {
      if (t.name.toLowerCase().includes(q)) hits.push({ type: t, field: null });
      for (const f of membersOf(t)) {
        if (f.name.toLowerCase().includes(q) || (f.description ?? "").toLowerCase().includes(q)) {
          hits.push({ type: t, field: f });
        }
      }
      if (hits.length > 80) break;
    }
    return { title: `Matches for “${search.value.trim()}”`, hits };
  }
  const name = current.value || roots.value[0] || "";
  const t = types.value.find((x) => x.name === name);
  return { title: name, type: t, hits: [] };
});

function open(name: string) {
  if (types.value.some((t) => t.name === name)) {
    current.value = name;
    search.value = "";
  }
}

function format() {
  try {
    if (variables.value.trim()) variables.value = JSON.stringify(JSON.parse(variables.value), null, 2);
  } catch {
    /* not JSON yet */
  }
}
</script>

<template>
  <div class="gql">
    <div class="bar">
      <label v-if="operations.length > 1" class="op">
        Operation
        <select v-model="operationName" aria-label="Operation to run">
          <option value="">(the only one)</option>
          <option v-for="o in operations" :key="o" :value="o">{{ o }}</option>
        </select>
      </label>
      <span class="spacer"></span>
      <span v-if="fetched" class="muted small" :title="`Read ${fetchedWhen}`">
        Schema {{ fetched.cached ? "from cache" : "read" }} · {{ types.length }} types
      </span>
      <button v-if="canFetch" class="ghost" :disabled="loading" @click="load(true)">
        {{ loading ? "Reading schema…" : fetched ? "↻ Schema" : "Load schema" }}
      </button>
      <button v-if="fetched" class="ghost" :class="{ on: showDocs }" @click="showDocs = !showDocs">Docs</button>
    </div>

    <p v-if="problem" class="warn">{{ problem }}</p>
    <p v-if="fetched && fetched.compared && (fetched.added.length || fetched.removed.length)" class="changes">
      The schema changed since last time:
      <span v-if="fetched.added.length" class="add">+{{ fetched.added.length }}</span>
      <span v-if="fetched.removed.length" class="del">−{{ fetched.removed.length }}</span>
      <span class="list">
        <code v-for="n in fetched.added.slice(0, 12)" :key="`a${n}`" class="add">+{{ n }}</code>
        <code v-for="n in fetched.removed.slice(0, 12)" :key="`r${n}`" class="del">−{{ n }}</code>
      </span>
    </p>
    <p v-else-if="fetched && fetched.compared && !fetched.cached" class="muted small">The schema has not changed since last time.</p>

    <div class="panes" :class="{ docs: showDocs && fetched }">
      <div class="pane query">
        <span class="label">Query</span>
        <CodeEditor
          v-model="query"
          class="code-area"
          language="graphql"
          :graphql-schema="schema"
          placeholder="query { me { id name } }   (Ctrl+Space completes from the schema)"
          @run="emit('send')"
          @save="emit('save')"
        />
      </div>
      <div class="pane vars">
        <span class="label">
          Variables <button class="link" @click="format">Format</button>
        </span>
        <CodeEditor
          v-model="variables"
          variables
          class="code-area"
          language="json"
          placeholder='{ "id": "{{userId}}" }'
          @run="emit('send')"
          @save="emit('save')"
        />
        <p v-if="variablesProblem" class="warn small">{{ variablesProblem }}</p>
      </div>
      <aside v-if="showDocs && fetched" class="pane explorer" aria-label="Schema docs">
        <input v-model="search" class="search" placeholder="Search types and fields" spellcheck="false" />
        <div class="crumbs">
          <button v-for="r in roots" :key="r" class="link" :class="{ on: docs.title === r }" @click="open(r)">{{ r }}</button>
        </div>
        <h4>{{ docs.title }}</h4>
        <template v-if="'type' in docs && docs.type">
          <p v-if="docs.type.description" class="muted small">{{ docs.type.description }}</p>
          <ul class="fields">
            <li v-for="f in membersOf(docs.type)" :key="f.name" :class="{ old: f.isDeprecated }">
              <code>{{ f.name }}</code
              ><template v-if="f.args?.length"
                >(<template v-for="(a, i) in f.args" :key="a.name"
                  ><template v-if="i">, </template>{{ a.name }}:
                  <button class="type" @click="open(namedType(a.type))">{{ typeText(a.type) }}</button></template
                >)</template
              ><template v-if="f.type">: <button class="type" @click="open(namedType(f.type))">{{ typeText(f.type) }}</button></template>
              <div v-if="f.description" class="muted small">{{ f.description }}</div>
            </li>
          </ul>
        </template>
        <ul v-else class="fields">
          <li v-for="(h, i) in docs.hits" :key="i">
            <button class="type" @click="open(h.type.name)">{{ h.type.name }}</button
            ><template v-if="h.field">.<code>{{ h.field.name }}</code
              ><template v-if="h.field.type">: {{ typeText(h.field.type) }}</template></template
            >
            <div v-if="h.field?.description" class="muted small">{{ h.field.description }}</div>
          </li>
          <li v-if="!docs.hits.length" class="muted small">Nothing matches.</li>
        </ul>
      </aside>
    </div>
    <section v-if="isSubscription" class="sub" aria-label="Subscription">
      <div class="sub-bar">
        <button
          v-if="subState !== 'on'"
          class="primary"
          :disabled="subState === 'starting' || !backend.wsConnect"
          @click="startSubscription"
        >
          {{ subState === "starting" ? "Connecting…" : "Subscribe" }}
        </button>
        <button v-else class="ghost" @click="stopSubscription">Stop</button>
        <span class="muted small">
          Over WebSocket at <code>{{ toWsUrl(request.url) }}</code> (graphql-transport-ws or the older graphql-ws).
          {{ events.filter((e) => e.kind === "data").length }} events.
        </span>
      </div>
      <ol v-if="events.length" class="events">
        <li v-for="(e, i) in events" :key="i" :class="e.kind" @click="toggleEvent(i)">
          <span class="time">{{ (e.atMs / 1000).toFixed(2) }}s</span>
          <pre v-if="openEvents.has(i) && e.kind !== 'info'">{{ prettyJson(e.text) }}</pre>
          <span v-else class="one">{{ e.text }}</span>
        </li>
      </ol>
    </section>

    <p class="hint">
      <template v-if="isSubscription">Send would post the subscription as a query; use Subscribe. </template>
      Sent as JSON <code>{query, variables}</code> with <code>{{ "{{" }}variables}}</code> filled in. The query is saved as a
      <code>.graphql</code> file next to the request.
    </p>
  </div>
</template>

<style scoped>
.gql {
  container-type: inline-size;
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-height: 0;
  flex: 1;
}
.bar {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  font-size: 12px;
}
.spacer {
  flex: 1;
}
.op {
  display: inline-flex;
  gap: 6px;
  align-items: center;
  color: var(--muted);
}
.panes {
  display: grid;
  grid-template-columns: minmax(0, 3fr) minmax(0, 2fr);
  gap: 10px;
  min-height: 260px;
  flex: 1;
}
.panes.docs {
  grid-template-columns: minmax(0, 3fr) minmax(0, 2fr) minmax(0, 2fr);
}
@container (max-width: 760px) {
  .panes,
  .panes.docs {
    grid-template-columns: minmax(0, 1fr);
  }
}
.pane {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-height: 0;
  min-width: 0;
}
.label {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}
.explorer {
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 8px;
  overflow: auto;
  max-height: 520px;
}
.search {
  width: 100%;
  box-sizing: border-box;
}
.crumbs {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
h4 {
  margin: 4px 0;
  font-size: 13px;
}
.fields {
  list-style: none;
  margin: 0;
  padding: 0;
  font-size: 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.fields .old {
  opacity: 0.6;
  text-decoration: line-through;
}
.type {
  background: none;
  border: none;
  padding: 0;
  font: inherit;
  font-family: var(--mono);
  color: var(--syn-tag, var(--accent));
  cursor: pointer;
}
.link {
  background: none;
  border: none;
  color: var(--accent);
  cursor: pointer;
  font-size: 12px;
  padding: 0;
  text-transform: none;
  letter-spacing: 0;
}
.link.on {
  font-weight: 700;
}
.ghost {
  border: 1px solid var(--border);
  background: var(--surface);
  color: inherit;
  border-radius: 8px;
  padding: 4px 10px;
  cursor: pointer;
}
.ghost.on {
  border-color: var(--accent);
}
.muted {
  color: var(--muted);
  margin: 0;
}
.small {
  font-size: 12px;
}
.warn {
  color: var(--m-delete);
  font-size: 12px;
  margin: 0;
}
.changes {
  font-size: 12px;
  margin: 0;
  display: flex;
  flex-wrap: wrap;
  gap: 4px 8px;
  align-items: baseline;
}
.changes .list {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}
.add {
  color: var(--m-get);
}
.del {
  color: var(--m-delete);
}
.sub {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.sub-bar {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
}
.primary {
  border: 0;
  background: var(--accent);
  color: #fff;
  border-radius: 8px;
  padding: 5px 14px;
  font-weight: 600;
  cursor: pointer;
}
.primary:disabled {
  opacity: 0.55;
  cursor: default;
}
.events {
  list-style: none;
  margin: 0;
  padding: 6px;
  border: 1px solid var(--border);
  border-radius: 8px;
  max-height: 260px;
  overflow: auto;
  font: 12px/1.5 var(--mono);
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.events li {
  display: grid;
  grid-template-columns: 52px minmax(0, 1fr);
  gap: 6px;
  cursor: pointer;
}
.events li.info,
.events li.done {
  color: var(--muted);
}
.events li.error {
  color: var(--m-delete);
}
.events .time {
  color: var(--muted);
}
.events pre {
  margin: 0;
  white-space: pre-wrap;
}
.events .one {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.hint {
  font-size: 12px;
  color: var(--muted);
  margin: 0;
}
</style>
