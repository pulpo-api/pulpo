<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { backend, type DiffItem, type GitPanel } from "../workspace";

const props = defineProps<{ root: string; unsaved: number; names: Record<string, string> }>();
const emit = defineEmits<{ close: []; open: [id: string]; reload: [] }>();

const panel = ref<GitPanel | null>(null);
const problem = ref("");
const note = ref("");
const busy = ref("");
const message = ref("");
const newBranch = ref("");
const available = computed(() => !!backend.gitPanel);

async function load() {
  if (!backend.gitPanel) return;
  try {
    panel.value = await backend.gitPanel(props.root);
    problem.value = "";
  } catch (e) {
    problem.value = String(e);
  }
}
onMounted(load);

const changes = computed(() => panel.value?.changes ?? null);
const otherFiles = computed(() => panel.value?.status.files.length ?? 0);
const changedCount = computed(() => {
  const c = changes.value;
  if (!c) return otherFiles.value;
  return c.added.length + c.removed.length + c.changed.length + c.folders.length + c.environments.length;
});
const nothingToCommit = computed(() => (panel.value?.status.files.length ?? 0) === 0);

function where(item: DiffItem): string {
  return item.folder.length ? `${item.folder.join(" / ")} / ` : "";
}

async function run(label: string, work: () => Promise<string | void>, reload = false) {
  busy.value = label;
  problem.value = "";
  note.value = "";
  try {
    const out = await work();
    note.value = typeof out === "string" && out ? out.split("\n").slice(-2).join(" ") : `${label}: done.`;
    if (reload) emit("reload");
  } catch (e) {
    problem.value = String(e);
  }
  busy.value = "";
  await load();
}

function commit() {
  void run("Commit", async () => {
    const id = await backend.gitCommit!(props.root, message.value);
    message.value = "";
    return `Committed ${id}.`;
  });
}

function switchTo(branch: string, create = false) {
  if (!branch || (branch === panel.value?.status.branch && !create)) return;
  void run(create ? "New branch" : "Switch", async () => {
    await backend.gitSwitch!(props.root, branch, create);
    newBranch.value = "";
    return `On ${branch}.`;
  }, true);
}

const blockedByUnsaved = computed(() =>
  props.unsaved ? `Save your ${props.unsaved} unsaved request(s) first (Ctrl+S).` : "",
);
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card panel" role="dialog" aria-label="Git">
      <header>
        <div>
          <h2>Git</h2>
          <p v-if="panel?.status.repo" class="sub">
            On <b>{{ panel.status.branch }}</b>
            <template v-if="panel.status.upstream">
              · {{ panel.status.upstream }}
              <span v-if="panel.status.ahead" title="Commits to push">↑{{ panel.status.ahead }}</span>
              <span v-if="panel.status.behind" title="Commits to pull">↓{{ panel.status.behind }}</span>
            </template>
            <template v-else> · not pushed yet</template>
          </p>
        </div>
        <button class="ghost" title="Close" @click="emit('close')">✕</button>
      </header>

      <p v-if="!available" class="note">Git runs in the desktop app (or use <code>pulpo diff</code> in a terminal).</p>
      <p v-else-if="panel && !panel.status.repo" class="note">
        This workspace is not in a git repository. Run <code>git init</code> in its folder (or in a parent one) to version it.
      </p>
      <template v-else-if="panel">
        <div class="bar">
          <select
            :value="panel.status.branch"
            :disabled="!!busy || !!blockedByUnsaved"
            aria-label="Branch"
            @change="switchTo(($event.target as HTMLSelectElement).value)"
          >
            <option v-for="b in panel.branches" :key="b" :value="b">{{ b }}</option>
          </select>
          <form class="new" @submit.prevent="switchTo(newBranch.trim(), true)">
            <input v-model="newBranch" placeholder="new-branch" spellcheck="false" aria-label="New branch name" />
            <button class="ghost" :disabled="!newBranch.trim() || !!busy || !!blockedByUnsaved">Create</button>
          </form>
          <span class="grow"></span>
          <button class="ghost" :disabled="!!busy || !!blockedByUnsaved" @click="run('Pull', () => backend.gitPull!(root), true)">
            {{ busy === "Pull" ? "Pulling…" : "Pull" }}
          </button>
          <button class="ghost" :disabled="!!busy" @click="run('Push', () => backend.gitPush!(root))">
            {{ busy === "Push" ? "Pushing…" : "Push" }}
          </button>
        </div>
        <p v-if="blockedByUnsaved" class="warn">{{ blockedByUnsaved }}</p>

        <div class="list">
          <p v-if="!changedCount" class="empty">No changes since the last commit.</p>
          <template v-if="changes">
            <div v-for="item in changes.added" :key="`a${item.id}`" class="change">
              <span class="tag add">added</span>
              <code class="method">{{ item.method }}</code>
              <button class="link" @click="emit('open', item.id)">{{ where(item) }}{{ names[item.id] ?? item.name }}</button>
            </div>
            <div v-for="c in changes.changed" :key="`c${c.item.id}`" class="change">
              <span class="tag mod">{{ c.was ? "moved" : "changed" }}</span>
              <code class="method">{{ c.item.method }}</code>
              <button class="link" @click="emit('open', c.item.id)">{{ where(c.item) }}{{ c.item.name }}</button>
              <ul>
                <li v-for="(line, i) in c.changes" :key="i">{{ line }}</li>
              </ul>
            </div>
            <div v-for="item in changes.removed" :key="`r${item.id}`" class="change">
              <span class="tag del">removed</span>
              <code class="method">{{ item.method }}</code>
              <span>{{ where(item) }}{{ item.name }}</span>
            </div>
            <div v-for="(line, i) in [...changes.folders, ...changes.environments]" :key="`f${i}`" class="change">
              <span class="tag mod">changed</span>
              <span>{{ line }}</span>
            </div>
          </template>
          <details v-if="otherFiles" class="files">
            <summary>{{ otherFiles }} changed file(s)</summary>
            <div v-for="f in panel.status.files" :key="f.path" class="file">
              <span class="state">{{ f.state }}</span> <code>{{ f.path }}</code>
            </div>
          </details>
        </div>

        <form class="commit" @submit.prevent="commit">
          <textarea
            v-model="message"
            rows="2"
            placeholder="Commit message (what changed in the API calls)"
            aria-label="Commit message"
          ></textarea>
          <button
            class="primary"
            :disabled="!!busy || nothingToCommit || !message.trim() || !!blockedByUnsaved"
            :title="nothingToCommit ? 'Nothing to commit' : 'Commit every change of this workspace'"
          >
            {{ busy === "Commit" ? "Committing…" : "Commit" }}
          </button>
        </form>
      </template>
      <p v-if="problem" class="warn">{{ problem }}</p>
      <p v-if="note" class="ok" role="status">{{ note }}</p>
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
.note,
.empty,
.warn,
.ok {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--muted);
}
.warn {
  color: var(--m-post);
  white-space: pre-wrap;
}
.ok {
  color: var(--ok);
}
.bar {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
}
.grow {
  flex: 1;
}
.new {
  display: flex;
  gap: 6px;
}
select,
input,
textarea {
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface-2);
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 13px;
  outline: none;
  min-width: 0;
}
textarea {
  flex: 1;
  resize: vertical;
  font-family: inherit;
}
.list {
  overflow: auto;
  min-height: 120px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.change {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 4px 8px;
  font-size: 13px;
}
.change ul {
  flex-basis: 100%;
  margin: 0 0 0 76px;
  padding-left: 14px;
  color: var(--muted);
  font-size: 12px;
}
.tag {
  width: 62px;
  font-size: 11px;
  text-transform: uppercase;
  font-weight: 600;
}
.tag.add {
  color: var(--m-get);
}
.tag.mod {
  color: var(--m-post);
}
.tag.del {
  color: var(--m-delete);
}
.method {
  font-size: 11px;
  color: var(--muted);
  width: 54px;
}
.link {
  background: none;
  border: none;
  padding: 0;
  color: var(--accent);
  cursor: pointer;
  font-size: 13px;
  text-align: left;
}
.link:hover {
  text-decoration: underline;
}
.files {
  font-size: 12px;
  color: var(--muted);
}
.file .state {
  display: inline-block;
  width: 70px;
}
.commit {
  display: flex;
  gap: 8px;
  align-items: flex-end;
}
@container (max-width: 520px) {
  .change ul {
    margin-left: 0;
  }
}
</style>
