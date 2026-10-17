<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  backend,
  passed,
  toEntry,
  toWorkspacePath,
  type DataRow,
  type Environment,
  type LoadProgress,
  type LoadSummary,
  type RequestEntry,
  type RunReport,
  type TreeNode,
  type Workspace,
} from "../workspace";

/** Runs the requests of a folder (or the whole collection) in order, like `pulpo run`:
 * the same engine sends each one, so what passes here passes in CI. */
const props = defineProps<{
  workspace: Workspace;
  folderId: string;
  environments: Environment[];
  environmentId: string;
}>();
const emit = defineEmits<{ close: []; open: [id: string] }>();

type Result = RunReport & { key: string; method: string };

const tab = ref<"run" | "load">("run");
const environmentId = ref(props.environmentId);

// ------------------------------------------------------------------ what to run

function findFolder(nodes: TreeNode[], id: string): TreeNode[] | null {
  for (const node of nodes) {
    if (node.type !== "folder") continue;
    if (node.id === id) return node.children;
    const found = findFolder(node.children, id);
    if (found) return found;
  }
  return null;
}

function flatten(nodes: TreeNode[], out: RequestEntry[] = []): RequestEntry[] {
  for (const node of nodes) {
    if (node.type === "folder") flatten(node.children, out);
    else out.push(toEntry(node));
  }
  return out;
}

const folderName = computed(() => {
  if (!props.folderId) return props.workspace.name;
  const find = (nodes: TreeNode[]): string | null => {
    for (const node of nodes) {
      if (node.type !== "folder") continue;
      if (node.id === props.folderId) return node.name;
      const found = find(node.children);
      if (found) return found;
    }
    return null;
  };
  return find(props.workspace.tree) ?? props.folderId;
});
const requests = computed(() =>
  flatten(props.folderId ? (findFolder(props.workspace.tree, props.folderId) ?? []) : props.workspace.tree),
);
const chosen = ref<Set<string>>(new Set(requests.value.map((r) => r.id)));
watch(requests, (list) => (chosen.value = new Set(list.map((r) => r.id))));
const selection = computed(() => requests.value.filter((r) => chosen.value.has(r.id)));

function toggle(id: string) {
  const next = new Set(chosen.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  chosen.value = next;
}

// ------------------------------------------------------------------ options

const iterations = ref(1);
const delayMs = ref(0);
const retries = ref(0);
const bail = ref(false);
const updateSnapshots = ref(false);
const dataPath = ref("");
const rows = ref<DataRow[]>([]);
const dataProblem = ref("");
const canUseData = !!backend.loadData;

async function loadRows() {
  dataProblem.value = "";
  rows.value = [];
  if (!dataPath.value.trim() || !backend.loadData) return;
  try {
    rows.value = await backend.loadData(props.workspace.root, dataPath.value.trim());
    if (rows.value.length) iterations.value = rows.value.length;
    else dataProblem.value = "The file has no rows.";
  } catch (e) {
    dataProblem.value = String(e);
  }
}

async function pickData() {
  const picked = await backend.pickDataFile?.();
  if (!picked) return;
  dataPath.value = toWorkspacePath(props.workspace.root, picked) ?? picked;
  await loadRows();
}

function clearData() {
  dataPath.value = "";
  rows.value = [];
  dataProblem.value = "";
  iterations.value = 1;
}

const dataVariables = computed(() =>
  Object.keys(rows.value[0] ?? {})
    .map((c) => "{{" + c + "}}")
    .join(" "),
);

// ------------------------------------------------------------------ running

const results = ref<Result[]>([]);
const running = ref(false);
const stopping = ref(false);
const plannedTotal = ref(0);
const doneCount = ref(0);
const onlyFailed = ref(false);
const expanded = ref<Set<string>>(new Set());
const lastIterations = ref(1);
let wake: (() => void) | null = null;

function rowFor(iteration: number): DataRow {
  if (!rows.value.length) return {};
  return rows.value[Math.min(iteration, rows.value.length - 1)];
}

function sleep(ms: number) {
  return new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, ms);
    wake = () => {
      clearTimeout(timer);
      resolve();
    };
  });
}

async function runOne(entry: RequestEntry, iteration: number, total: number, snapshots = updateSnapshots.value) {
  const report = await backend.run(props.workspace.root, entry, environmentId.value || undefined, {
    iteration,
    iterations: total,
    data: rowFor(iteration),
    retries: Math.max(0, retries.value | 0),
    updateSnapshots: snapshots,
  });
  return { ...report, iteration, key: `${iteration}:${entry.id}`, method: entry.request.method } satisfies Result;
}

/** Runs `plan` (request, iteration) in order; results replace earlier ones with the same key. */
async function execute(plan: { entry: RequestEntry; iteration: number }[], total: number) {
  running.value = true;
  stopping.value = false;
  plannedTotal.value = plan.length;
  lastIterations.value = total;
  doneCount.value = 0;
  for (const { entry, iteration } of plan) {
    if (stopping.value) break;
    if (doneCount.value > 0 && delayMs.value > 0) await sleep(delayMs.value);
    if (stopping.value) break;
    const result = await runOne(entry, iteration, total);
    const at = results.value.findIndex((r) => r.key === result.key);
    if (at >= 0) results.value.splice(at, 1, result);
    else results.value.push(result);
    doneCount.value++;
    if (bail.value && !passed(result)) break;
  }
  running.value = false;
}

async function start() {
  if (!selection.value.length) return;
  results.value = [];
  expanded.value = new Set();
  const total = Math.max(1, iterations.value | 0);
  const plan = [];
  for (let iteration = 0; iteration < total; iteration++) {
    for (const entry of selection.value) plan.push({ entry, iteration });
  }
  await execute(plan, total);
}

async function rerunFailed() {
  const failed = results.value.filter((r) => !passed(r));
  const plan = failed
    .map((r) => ({ entry: requests.value.find((e) => e.id === r.id), iteration: r.iteration ?? 0 }))
    .filter((p): p is { entry: RequestEntry; iteration: number } => !!p.entry);
  await execute(plan, lastIterations.value);
}

async function updateSnapshot(result: Result) {
  const entry = requests.value.find((e) => e.id === result.id);
  if (!entry) return;
  const fresh = await runOne(entry, result.iteration ?? 0, lastIterations.value, true);
  results.value.splice(results.value.findIndex((r) => r.key === result.key), 1, fresh);
}

function stop() {
  stopping.value = true;
  wake?.();
}

function toggleRow(key: string) {
  const next = new Set(expanded.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expanded.value = next;
}

const totals = computed(() => {
  const list = results.value;
  return {
    passed: list.filter(passed).length,
    failed: list.filter((r) => !passed(r)).length,
    tests: list.reduce((n, r) => n + r.tests.length, 0),
    testsPassed: list.reduce((n, r) => n + r.tests.filter((t) => t.passed).length, 0),
    ms: list.reduce((n, r) => n + (r.response?.durationMs ?? 0), 0),
  };
});

const shown = computed(() => (onlyFailed.value ? results.value.filter((r) => !passed(r)) : results.value));
const groups = computed(() => {
  const out: { iteration: number; items: Result[] }[] = [];
  for (const r of shown.value) {
    const iteration = r.iteration ?? 0;
    let group = out.find((g) => g.iteration === iteration);
    if (!group) out.push((group = { iteration, items: [] }));
    group.items.push(r);
  }
  return out.sort((a, b) => a.iteration - b.iteration);
});

function statusOf(r: Result): string {
  if (r.response) return `${r.response.status} ${r.response.statusText}`.trim();
  return r.error ? r.error.message : "";
}

function snapshotFailed(r: Result) {
  return r.tests.some((t) => !t.passed && t.name.startsWith("Snapshot"));
}

const exportMessage = ref("");
async function exportReport(format: "html" | "junit") {
  if (!backend.saveRunReport) return;
  try {
    const path = await backend.saveRunReport(folderName.value, results.value, format);
    exportMessage.value = path ? `Saved to ${path}` : "";
  } catch (e) {
    exportMessage.value = String(e);
  }
}

// ------------------------------------------------------------------ load test

const users = ref(10);
const durationSecs = ref(30);
const rampUpSecs = ref(0);
const loadRunning = ref(false);
const loadProgress = ref<LoadProgress | null>(null);
const loadSummary = ref<LoadSummary | null>(null);
const loadProblem = ref("");
const canLoad = !!backend.loadTest;

async function startLoad() {
  if (!backend.loadTest || !selection.value.length) return;
  loadRunning.value = true;
  loadSummary.value = null;
  loadProblem.value = "";
  loadProgress.value = { elapsedSecs: 0, done: 0, failed: 0 };
  try {
    loadSummary.value = await backend.loadTest(
      props.workspace.root,
      selection.value,
      environmentId.value || undefined,
      {
        users: Math.min(200, Math.max(1, users.value | 0)),
        durationSecs: Math.max(1, durationSecs.value | 0),
        rampUpSecs: Math.max(0, rampUpSecs.value | 0),
        data: rows.value,
      },
      (p) => (loadProgress.value = p),
    );
  } catch (e) {
    loadProblem.value = String(e);
  } finally {
    loadRunning.value = false;
  }
}

function stopLoad() {
  void backend.stopLoadTest?.();
}

onBeforeUnmount(() => {
  stop();
  if (loadRunning.value) stopLoad();
});
</script>

<template>
  <section class="card runner">
    <header>
      <div>
        <p class="kind">Runner</p>
        <h2>{{ folderName }}</h2>
        <p class="sub">
          Runs the requests in order with their scripts and tests, the same engine as <code>pulpo run</code> in CI.
        </p>
      </div>
      <div class="actions">
        <select v-model="environmentId" aria-label="Environment">
          <option value="">No environment</option>
          <option v-for="env in environments" :key="env.id" :value="env.id">{{ env.name }}</option>
        </select>
        <button class="ghost" title="Close the runner" @click="emit('close')">✕</button>
      </div>
    </header>

    <div class="tabs">
      <button :class="{ on: tab === 'run' }" @click="tab = 'run'">Run</button>
      <button :class="{ on: tab === 'load' }" @click="tab = 'load'">Load test</button>
    </div>

    <div class="setup">
      <div class="pick">
        <div class="pick-head">
          <span>{{ selection.length }} of {{ requests.length }} requests</span>
          <button class="link" @click="chosen = new Set(requests.map((r) => r.id))">All</button>
          <button class="link" @click="chosen = new Set()">None</button>
        </div>
        <label v-for="r in requests" :key="r.id" class="req">
          <input type="checkbox" :checked="chosen.has(r.id)" @change="toggle(r.id)" />
          <span class="method" :style="{ color: `var(--m-${r.request.method.toLowerCase()}, var(--muted))` }">{{
            r.request.method
          }}</span>
          <span class="name">{{ r.name }}</span>
        </label>
        <p v-if="!requests.length" class="hint">No requests here yet.</p>
      </div>

      <div v-if="tab === 'run'" class="options">
        <label>Iterations <input v-model.number="iterations" type="number" min="1" max="10000" /></label>
        <label>Delay (ms) <input v-model.number="delayMs" type="number" min="0" step="100" /></label>
        <label title="Sends again after a network error or a 5xx answer"
          >Retries <input v-model.number="retries" type="number" min="0" max="10"
        /></label>
        <label class="check"><input v-model="bail" type="checkbox" /> Stop at the first failure</label>
        <label class="check" title="Rewrite the saved snapshots with these responses"
          ><input v-model="updateSnapshots" type="checkbox" /> Update snapshots</label
        >
        <div class="data">
          <span>Data file</span>
          <template v-if="canUseData">
            <input
              v-model="dataPath"
              placeholder="users.csv (CSV or JSON, one row per iteration)"
              spellcheck="false"
              @change="loadRows"
            />
            <button class="ghost small" @click="pickData">Browse…</button>
            <button v-if="dataPath" class="ghost small" @click="clearData">Clear</button>
          </template>
          <span v-else class="hint">CSV and JSON data files work in the desktop app.</span>
        </div>
        <p v-if="dataProblem" class="warn">{{ dataProblem }}</p>
        <p v-else-if="rows.length" class="hint">
          {{ rows.length }} rows · {{ dataVariables }}
        </p>
        <div class="go">
          <button v-if="!running" class="primary" :disabled="!selection.length" @click="start">
            Run {{ selection.length }} request{{ selection.length === 1 ? "" : "s" }}
            <template v-if="iterations > 1"> × {{ iterations }}</template>
          </button>
          <button v-else class="ghost" @click="stop">Stop</button>
        </div>
      </div>

      <div v-else class="options">
        <label>Virtual users <input v-model.number="users" type="number" min="1" max="200" /></label>
        <label>Duration (s) <input v-model.number="durationSecs" type="number" min="1" max="3600" /></label>
        <label>Ramp-up (s) <input v-model.number="rampUpSecs" type="number" min="0" /></label>
        <p class="hint">
          Each user runs the selected requests in a loop, with their scripts{{ rows.length ? " and one data row" : "" }}.
          Nothing is saved: no local values, cookies or snapshots.
        </p>
        <div class="go">
          <template v-if="canLoad">
            <button v-if="!loadRunning" class="primary" :disabled="!selection.length" @click="startLoad">
              Start load test
            </button>
            <button v-else class="ghost" @click="stopLoad">Stop</button>
          </template>
          <span v-else class="hint">Load tests run in the desktop app.</span>
        </div>
      </div>
    </div>

    <template v-if="tab === 'run' && (results.length || running)">
      <div class="summary">
        <span class="bar"
          ><span class="fill" :style="{ width: `${(doneCount / Math.max(1, plannedTotal)) * 100}%` }"
        /></span>
        <span>{{ doneCount }}/{{ plannedTotal }}</span>
        <span class="ok">{{ totals.passed }} passed</span>
        <span :class="{ ko: totals.failed }">{{ totals.failed }} failed</span>
        <span>tests {{ totals.testsPassed }}/{{ totals.tests }}</span>
        <span class="muted">{{ totals.ms }} ms</span>
        <label class="check"><input v-model="onlyFailed" type="checkbox" /> Only failed</label>
        <span class="spacer" />
        <button class="ghost small" :disabled="running || !totals.failed" @click="rerunFailed">Re-run failed</button>
        <template v-if="backend.saveRunReport">
          <button class="ghost small" :disabled="running" @click="exportReport('html')">HTML report</button>
          <button class="ghost small" :disabled="running" @click="exportReport('junit')">JUnit</button>
        </template>
      </div>
      <p v-if="exportMessage" class="hint">{{ exportMessage }}</p>

      <div class="results">
        <div v-for="g in groups" :key="g.iteration">
          <p v-if="lastIterations > 1" class="iteration">
            Iteration {{ g.iteration + 1 }}/{{ lastIterations }}
            <span v-if="rows.length" class="muted">{{
              Object.entries(rows[Math.min(g.iteration, rows.length - 1)] ?? {})
                .map(([k, v]) => `${k}=${v}`)
                .join(", ")
            }}</span>
          </p>
          <div v-for="r in g.items" :key="r.key" class="result" :class="{ failed: !passed(r) }">
            <button class="row" @click="toggleRow(r.key)">
              <span class="mark">{{ passed(r) ? "✓" : "✗" }}</span>
              <span class="method" :style="{ color: `var(--m-${r.method.toLowerCase()}, var(--muted))` }">{{
                r.method
              }}</span>
              <span class="name">{{ r.name }}</span>
              <span v-if="(r.attempts ?? 1) > 1" class="tag">{{ r.attempts }} attempts</span>
              <span class="status">{{ statusOf(r) }}</span>
              <span class="muted">{{ r.response ? `${r.response.durationMs} ms` : "" }}</span>
            </button>
            <div v-if="expanded.has(r.key) || !passed(r)" class="detail">
              <p v-for="(t, i) in r.tests" :key="i" :class="t.passed ? 'ok' : 'ko'">
                {{ t.passed ? "✓" : "✗" }} {{ t.name }}<template v-if="t.error"> — {{ t.error }}</template>
              </p>
              <p v-for="(e, i) in r.scriptErrors" :key="`e${i}`" class="ko">! {{ e }}</p>
              <p v-for="(l, i) in r.logs" :key="`l${i}`" class="log">&gt; {{ l }}</p>
              <p v-if="!r.tests.length && !r.scriptErrors.length && !r.logs.length" class="muted">No tests.</p>
              <div class="detail-actions">
                <button class="link" @click="emit('open', r.id)">Open request</button>
                <button v-if="snapshotFailed(r)" class="link" @click="updateSnapshot(r)">
                  Accept the new response as snapshot
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </template>

    <template v-if="tab === 'load'">
      <p v-if="loadProblem" class="warn">{{ loadProblem }}</p>
      <div v-if="loadRunning && loadProgress" class="summary">
        <span class="bar"
          ><span class="fill" :style="{ width: `${(loadProgress.elapsedSecs / Math.max(1, durationSecs)) * 100}%` }"
        /></span>
        <span>{{ loadProgress.elapsedSecs }} / {{ durationSecs }} s</span>
        <span>{{ loadProgress.done }} done</span>
        <span :class="{ ko: loadProgress.failed }">{{ loadProgress.failed }} failed</span>
      </div>
      <div v-if="loadSummary" class="load">
        <p class="hint">
          {{ loadSummary.users }} users · {{ (loadSummary.elapsedMs / 1000).toFixed(1) }} s ·
          {{ loadSummary.total.perSecond }} requests/s
        </p>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Request</th>
                <th>Count</th>
                <th>Failed</th>
                <th>Avg</th>
                <th>p50</th>
                <th>p90</th>
                <th>p95</th>
                <th>p99</th>
                <th>Max</th>
                <th>Req/s</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="s in [...loadSummary.requests, loadSummary.total]"
                :key="s.name"
                :class="{ total: s === loadSummary.total }"
              >
                <td>{{ s.name }}</td>
                <td>{{ s.count }}</td>
                <td :class="{ ko: s.failed }">{{ s.failed }}</td>
                <td>{{ s.avgMs }} ms</td>
                <td>{{ s.p50Ms }} ms</td>
                <td>{{ s.p90Ms }} ms</td>
                <td>{{ s.p95Ms }} ms</td>
                <td>{{ s.p99Ms }} ms</td>
                <td>{{ s.maxMs }} ms</td>
                <td>{{ s.perSecond }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-for="(e, i) in loadSummary.errors" :key="i" class="ko">! {{ e }}</p>
      </div>
    </template>
  </section>
</template>

<style scoped>
.runner {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 0;
  overflow: auto;
}
header {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}
.kind {
  margin: 0;
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--accent);
}
h2 {
  margin: 2px 0;
  font-size: 18px;
}
.sub,
.hint,
.warn,
.muted {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
}
.warn {
  color: var(--m-post);
}
.actions {
  display: flex;
  gap: 8px;
}
select,
input:not([type="checkbox"]) {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  color: var(--text);
  padding: 5px 8px;
  outline: none;
  font: inherit;
}
.tabs {
  display: flex;
  gap: 4px;
}
.tabs button {
  border: 0;
  background: transparent;
  padding: 6px 10px;
  border-radius: 6px;
  color: var(--muted);
  cursor: pointer;
}
.tabs button.on {
  background: var(--surface-2);
  color: var(--text);
  font-weight: 600;
}
.setup {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 16px;
}
@media (max-width: 760px) {
  .setup {
    grid-template-columns: minmax(0, 1fr);
  }
}
.pick {
  display: flex;
  flex-direction: column;
  gap: 2px;
  max-height: 260px;
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 6px;
}
.pick-head {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--muted);
  padding: 2px 4px 6px;
}
.req {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 3px 4px;
  border-radius: 6px;
  cursor: pointer;
  min-width: 0;
}
.req:hover {
  background: var(--surface-2);
}
.method {
  font: 600 11px var(--mono);
  min-width: 48px;
}
.name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.options {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.options > label {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  max-width: 320px;
}
.options > label input:not([type="checkbox"]) {
  width: 110px;
}
.options > label.check {
  justify-content: flex-start;
}
.data {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.data input {
  flex: 1 1 200px;
  min-width: 0;
}
.go {
  margin-top: 4px;
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
.ghost.small {
  padding: 4px 10px;
  font-size: 12px;
}
.primary {
  border: 0;
  background: var(--accent);
  color: #fff;
  font-weight: 600;
}
.link {
  border: 0;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  padding: 0;
  font-size: 12px;
}
button:disabled {
  opacity: 0.5;
  cursor: default;
}
.summary {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  padding: 8px 0;
  border-top: 1px solid var(--border);
  font-size: 13px;
}
.summary .check {
  display: flex;
  align-items: center;
  gap: 4px;
  color: var(--muted);
}
.spacer {
  flex: 1;
}
.bar {
  position: relative;
  width: 120px;
  height: 6px;
  border-radius: 3px;
  background: var(--surface-2);
  overflow: hidden;
}
.fill {
  position: absolute;
  inset: 0 auto 0 0;
  background: var(--accent);
  transition: width 0.2s;
}
.ok {
  color: var(--ok);
}
.ko {
  color: var(--m-delete);
}
.results {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.iteration {
  margin: 10px 0 4px;
  font-size: 12px;
  font-weight: 600;
  color: var(--muted);
}
.result {
  border-radius: 6px;
}
.result.failed {
  background: color-mix(in srgb, var(--m-delete) 7%, transparent);
}
.row {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  border: 0;
  background: transparent;
  color: var(--text);
  padding: 5px 8px;
  text-align: left;
  cursor: pointer;
  font: inherit;
}
.row .mark {
  width: 14px;
}
.result:not(.failed) .mark {
  color: var(--ok);
}
.result.failed .mark {
  color: var(--m-delete);
}
.row .status {
  margin-left: auto;
  font-size: 12px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 40%;
}
.tag {
  font-size: 11px;
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 0 6px;
  color: var(--muted);
}
.detail {
  padding: 2px 8px 8px 38px;
  font-size: 12px;
}
.detail p {
  margin: 2px 0;
  overflow-wrap: anywhere;
}
.detail .log {
  font-family: var(--mono);
  color: var(--muted);
}
.detail-actions {
  display: flex;
  gap: 14px;
  margin-top: 4px;
}
.table-wrap {
  overflow-x: auto;
}
table {
  border-collapse: collapse;
  width: 100%;
  font-size: 12px;
  font-variant-numeric: tabular-nums;
}
th,
td {
  padding: 5px 8px;
  border-bottom: 1px solid var(--border);
  text-align: right;
  white-space: nowrap;
}
th:first-child,
td:first-child {
  text-align: left;
}
tr.total td {
  font-weight: 600;
}
</style>
