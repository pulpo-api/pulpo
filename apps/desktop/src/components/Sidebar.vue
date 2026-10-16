<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import TreeView from "./TreeView.vue";
import type { RequestEntry, TreeNode, Workspace } from "../workspace";

const props = defineProps<{
  workspace: Workspace | null;
  selectedId: string;
  folderId: string;
  dirty: Set<string>;
  persistent: boolean;
}>();
const emit = defineEmits<{
  select: [entry: RequestEntry];
  folder: [id: string];
  open: [];
  newRequest: [name: string];
  newFolder: [name: string];
  import: [];
}>();

const query = ref("");
const searchBox = ref<HTMLInputElement | null>(null);
defineExpose({
  focusSearch: () => {
    searchBox.value?.focus();
    searchBox.value?.select();
  },
});

/** Keeps requests whose name, URL or method match every word, and the folders that lead to them. */
function filter(nodes: TreeNode[], words: string[]): TreeNode[] {
  const out: TreeNode[] = [];
  for (const node of nodes) {
    if (node.type === "folder") {
      const nameMatches = words.every((w) => node.name.toLowerCase().includes(w));
      const children = nameMatches ? node.children : filter(node.children, words);
      if (children.length) out.push({ ...node, children });
    } else {
      const haystack = `${node.name} ${node.request.url} ${node.request.method} ${node.protocol}`.toLowerCase();
      if (words.every((w) => haystack.includes(w))) out.push(node);
    }
  }
  return out;
}

const words = computed(() => query.value.toLowerCase().split(/\s+/).filter(Boolean));
const visibleTree = computed(() => {
  const tree = props.workspace?.tree ?? [];
  return words.value.length ? filter(tree, words.value) : tree;
});

const creating = ref<"request" | "folder" | null>(null);
const draft = ref("");
const input = ref<HTMLInputElement | null>(null);

async function start(kind: "request" | "folder") {
  creating.value = kind;
  draft.value = "";
  await nextTick();
  input.value?.focus();
}

function confirm() {
  const name = draft.value.trim();
  if (name && creating.value === "request") emit("newRequest", name);
  if (name && creating.value === "folder") emit("newFolder", name);
  creating.value = null;
}
</script>

<template>
  <aside class="sidebar">
    <div class="brand">
      <img src="/pulpo.svg" alt="" width="28" height="28" />
      <span>Pulpo</span>
    </div>

    <template v-if="workspace">
      <div class="ws">
        <button class="ws-name" :title="workspace.root" @click="emit('folder', '')">{{ workspace.name }}</button>
        <button v-if="persistent" class="icon" title="Open another folder" @click="emit('open')">⇄</button>
      </div>
      <div class="actions">
        <button @click="start('request')">+ Request</button>
        <button @click="start('folder')">+ Folder</button>
        <button title="Import Postman, OpenAPI or curl" @click="emit('import')">Import</button>
      </div>
      <input
        ref="searchBox"
        v-model="query"
        class="search"
        type="search"
        placeholder="Search requests  (Ctrl+K)"
        spellcheck="false"
        @keydown.esc="query = ''"
      />
      <input
        v-if="creating"
        ref="input"
        v-model="draft"
        class="draft"
        :placeholder="creating === 'request' ? 'Request name, then Enter' : 'Folder name, then Enter'"
        @keydown.enter="confirm"
        @keydown.esc="creating = null"
        @blur="creating = null"
      />
      <nav>
        <TreeView
          :nodes="visibleTree"
          :selected-id="selectedId"
          :folder-id="folderId"
          :dirty="dirty"
          :expand-all="words.length > 0"
          @select="emit('select', $event)"
          @folder="emit('folder', $event)"
        />
        <p v-if="!workspace.tree.length" class="empty">Empty workspace. Add a request to start.</p>
        <p v-else-if="!visibleTree.length" class="empty">No request matches “{{ query }}”.</p>
      </nav>
      <footer :title="workspace.root">
        <span class="dot" :class="{ off: !persistent }"></span>
        {{ persistent ? workspace.root : "In memory: changes are not saved" }}
      </footer>
    </template>
  </aside>
</template>

<style scoped>
.sidebar {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 16px 12px;
  background: var(--surface);
  border-right: 1px solid var(--border);
  min-width: 0;
  min-height: 0;
}
.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  font-weight: 700;
  font-size: 17px;
  padding: 0 6px 4px;
}
.ws {
  display: flex;
  align-items: center;
  gap: 6px;
}
.ws-name {
  flex: 1;
  min-width: 0;
  border: 0;
  background: transparent;
  text-align: left;
  font-weight: 600;
  padding: 4px 6px;
  border-radius: 6px;
  cursor: pointer;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ws-name:hover {
  background: var(--surface-2);
}
.icon {
  border: 1px solid var(--border);
  background: var(--surface);
  border-radius: 6px;
  cursor: pointer;
  padding: 2px 8px;
}
.actions {
  display: flex;
  gap: 6px;
}
.actions button {
  flex: 1;
  border: 1px dashed var(--border);
  background: transparent;
  border-radius: 8px;
  padding: 6px;
  color: var(--muted);
  cursor: pointer;
}
.actions button:hover {
  border-color: var(--accent);
  color: var(--accent);
}
.search {
  border: 1px solid var(--border);
  background: var(--surface-2);
  border-radius: 8px;
  padding: 7px 10px;
  outline: none;
}
.search:focus {
  border-color: var(--accent);
}
.draft {
  border: 1px solid var(--accent);
  border-radius: 8px;
  background: var(--surface-2);
  padding: 7px 10px;
  outline: none;
}
nav {
  flex: 1;
  overflow: auto;
  min-height: 0;
}
.empty {
  color: var(--muted);
  font-size: 13px;
  padding: 0 6px;
}
footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 6px;
  font-size: 12px;
  color: var(--muted);
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.dot {
  flex: none;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--ok);
}
.dot.off {
  background: var(--m-post);
}
</style>
