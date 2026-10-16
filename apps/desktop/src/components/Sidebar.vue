<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import TreeView from "./TreeView.vue";
import type { RequestEntry, TreeNode, Workspace } from "../workspace";
import { dragging, dropHint, parentOf, startRenaming, type DropPosition } from "../treeState";

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
  rename: [id: string, name: string];
  duplicate: [id: string];
  delete: [node: TreeNode];
  move: [id: string, target: string, index: number];
  editFolder: [id: string];
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

// ---------------------------------------------------------------- organize

function childrenOf(folder: string): TreeNode[] {
  if (!folder) return props.workspace?.tree ?? [];
  let found: TreeNode[] = [];
  const walk = (nodes: TreeNode[]) => {
    for (const n of nodes) {
      if (n.type !== "folder") continue;
      if (n.id === folder) found = n.children;
      else walk(n.children);
    }
  };
  walk(props.workspace?.tree ?? []);
  return found;
}

/** Turns "dropped before/after/into a node" into "move into folder at index", where the
 * index counts the target's children without the moved node (as pulpo-core expects). */
function onDrop(id: string, targetId: string, position: DropPosition) {
  const target = position === "into" ? targetId : parentOf(targetId);
  const siblings = childrenOf(target).filter((n) => n.id !== id);
  let index = siblings.length;
  if (position !== "into") {
    const at = siblings.findIndex((n) => n.id === targetId);
    index = at < 0 ? siblings.length : position === "after" ? at + 1 : at;
  }
  emit("move", id, target, index);
}

/** Rows handle their own drops; only the empty part of the tree lands here. */
function onDragOverEmpty(event: DragEvent) {
  if (!dragging.value) return;
  event.preventDefault();
  dropHint.value = null;
}

/** Dropping on the empty part of the tree moves to the end of the top level. */
function onDropOnEmpty(event: DragEvent) {
  const source = dragging.value;
  dragging.value = null;
  dropHint.value = null;
  if (!source) return;
  event.preventDefault();
  emit("move", source.id, "", childrenOf("").filter((n) => n.id !== source.id).length);
}

// ---------------------------------------------------------------- context menu

const menu = ref<{ node: TreeNode; x: number; y: number } | null>(null);
const confirmDelete = ref(false);

function openMenu(node: TreeNode, x: number, y: number) {
  confirmDelete.value = false;
  // Keep the menu inside the window.
  menu.value = { node, x: Math.min(x, window.innerWidth - 200), y: Math.min(y, window.innerHeight - 220) };
}

function closeMenu() {
  menu.value = null;
}

function countRequests(node: TreeNode): number {
  return node.type === "request" ? 1 : node.children.reduce((sum, child) => sum + countRequests(child), 0);
}

function act(action: "rename" | "duplicate" | "delete" | "newRequest" | "newFolder" | "edit") {
  const node = menu.value?.node;
  if (!node) return;
  if (action === "delete" && !confirmDelete.value) {
    confirmDelete.value = true;
    return;
  }
  closeMenu();
  if (action === "rename") startRenaming(node.id, node.name);
  else if (action === "edit") emit("editFolder", node.id);
  else if (action === "duplicate") emit("duplicate", node.id);
  else if (action === "delete") emit("delete", node);
  else {
    // New items go into the folder (or next to the request).
    emit("folder", node.type === "folder" ? node.id : parentOf(node.id));
    void start(action === "newRequest" ? "request" : "folder");
  }
}

function onWindowKey(event: KeyboardEvent) {
  if (event.key === "Escape") closeMenu();
}
onMounted(() => window.addEventListener("keydown", onWindowKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onWindowKey));

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
        <button class="icon" title="Collection headers, auth, settings and scripts" @click="emit('editFolder', '')">⚙</button>
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
      <nav @dragover="onDragOverEmpty" @drop="onDropOnEmpty">
        <TreeView
          :nodes="visibleTree"
          :selected-id="selectedId"
          :folder-id="folderId"
          :dirty="dirty"
          :expand-all="words.length > 0"
          :organizable="words.length === 0"
          @select="emit('select', $event)"
          @folder="emit('folder', $event)"
          @menu="openMenu"
          @rename="(id, name) => emit('rename', id, name)"
          @drop="onDrop"
        />
        <p v-if="!workspace.tree.length" class="empty">Empty workspace. Add a request to start.</p>
        <p v-else-if="!visibleTree.length" class="empty">No request matches “{{ query }}”.</p>
      </nav>
      <p v-if="workspace.tree.length && !words.length" class="tip">Drag to move · double-click or F2 to rename · right-click for more</p>
      <footer :title="workspace.root">
        <span class="dot" :class="{ off: !persistent }"></span>
        {{ persistent ? workspace.root : "In memory: changes are not saved" }}
      </footer>
    </template>

    <div v-if="menu" class="menu-backdrop" @click="closeMenu" @contextmenu.prevent="closeMenu">
      <div class="menu card" role="menu" :style="{ left: `${menu.x}px`, top: `${menu.y}px` }" @click.stop>
        <button role="menuitem" @click="act('rename')">Rename <kbd>F2</kbd></button>
        <button role="menuitem" @click="act('duplicate')">Duplicate</button>
        <template v-if="menu.node.type === 'folder'">
          <button role="menuitem" @click="act('edit')">Headers, auth, settings…</button>
          <button role="menuitem" @click="act('newRequest')">New request here</button>
          <button role="menuitem" @click="act('newFolder')">New folder here</button>
        </template>
        <hr />
        <button role="menuitem" class="danger" :class="{ armed: confirmDelete }" @click="act('delete')">
          <template v-if="!confirmDelete">Delete</template>
          <template v-else-if="menu.node.type === 'folder'">
            Click again: delete the folder and its {{ countRequests(menu.node) }} request(s)
          </template>
          <template v-else>Click again to delete</template>
        </button>
      </div>
    </div>
  </aside>
</template>

<style scoped>
.tip {
  margin: 0;
  font-size: 11px;
  color: var(--muted);
  opacity: 0.8;
}
.menu-backdrop {
  position: fixed;
  inset: 0;
  z-index: 30;
}
.menu {
  position: fixed;
  min-width: 190px;
  max-width: 280px;
  padding: 6px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.25);
}
.menu button {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--text);
  padding: 7px 10px;
  text-align: left;
  cursor: pointer;
  font-size: 13px;
}
.menu button:hover {
  background: var(--surface-2);
}
.menu .danger {
  color: var(--m-delete);
}
.menu .danger.armed {
  background: rgba(220, 70, 70, 0.12);
}
.menu hr {
  border: 0;
  border-top: 1px solid var(--border);
  margin: 4px 0;
}
kbd {
  font: 11px var(--mono);
  color: var(--muted);
}
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
