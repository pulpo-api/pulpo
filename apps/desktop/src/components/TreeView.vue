<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import type { RequestEntry, TreeNode } from "../workspace";
import {
  draftName,
  dragging,
  dropHint,
  renamingId,
  startRenaming,
  within,
  type DropPosition,
} from "../treeState";

defineOptions({ name: "TreeView" });

const props = defineProps<{
  nodes: TreeNode[];
  selectedId: string;
  folderId: string;
  dirty: Set<string>;
  depth?: number;
  expandAll?: boolean;
  /** Drag and drop is off while the tree is filtered: you would not see where things go. */
  organizable?: boolean;
}>();
const emit = defineEmits<{
  select: [entry: RequestEntry];
  folder: [id: string];
  menu: [node: TreeNode, x: number, y: number];
  rename: [id: string, name: string];
  drop: [id: string, targetId: string, position: DropPosition];
}>();

const collapsed = ref(new Set<string>());

function toggle(id: string) {
  if (collapsed.value.has(id)) collapsed.value.delete(id);
  else collapsed.value.add(id);
  emit("folder", id);
}

function badge(node: RequestEntry): string {
  return node.protocol === "http" ? node.request.method : node.protocol.toUpperCase().slice(0, 4);
}

function badgeClass(node: RequestEntry): string {
  return node.protocol === "http" ? `m-${node.request.method.toLowerCase()}` : `p-${node.protocol}`;
}

const pad = (props.depth ?? 0) * 12;

// ---------------------------------------------------------------- rename

const renameInput = ref<HTMLInputElement[]>([]);

function startRename(node: TreeNode) {
  startRenaming(node.id, node.name);
}

// Focus the input when this level holds the node being renamed.
watch(renamingId, async (id) => {
  if (!props.nodes.some((n) => n.id === id)) return;
  await nextTick();
  renameInput.value[0]?.focus();
  renameInput.value[0]?.select();
});

function commitRename(node: TreeNode) {
  if (renamingId.value !== node.id) return;
  renamingId.value = "";
  const name = draftName.value.trim();
  if (name && name !== node.name) emit("rename", node.id, name);
}

function onKey(event: KeyboardEvent, node: TreeNode) {
  if (event.key === "F2") {
    event.preventDefault();
    void startRename(node);
  }
}

// ---------------------------------------------------------------- drag and drop

function onDragStart(event: DragEvent, node: TreeNode) {
  if (!props.organizable || renamingId.value) {
    event.preventDefault();
    return;
  }
  dragging.value = { id: node.id, folder: node.type === "folder" };
  event.dataTransfer?.setData("text/plain", node.id);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
}

function positionFor(event: DragEvent, node: TreeNode): DropPosition {
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
  const y = (event.clientY - rect.top) / rect.height;
  if (node.type === "folder") return y < 0.25 ? "before" : y > 0.75 ? "after" : "into";
  return y < 0.5 ? "before" : "after";
}

function onDragOver(event: DragEvent, node: TreeNode) {
  const source = dragging.value;
  if (!source) return;
  event.stopPropagation();
  // A folder cannot go inside itself: no drop here (nor in the empty area below).
  if (source.folder && within(node.id, source.id)) {
    dropHint.value = null;
    return;
  }
  event.preventDefault();
  dropHint.value = { id: node.id, position: positionFor(event, node) };
}

function onDrop(event: DragEvent, node: TreeNode) {
  const source = dragging.value;
  const hint = dropHint.value;
  event.preventDefault();
  event.stopPropagation();
  dragging.value = null;
  dropHint.value = null;
  if (!source || !hint || hint.id !== node.id || source.id === node.id) return;
  emit("drop", source.id, node.id, hint.position);
}

function onDragEnd() {
  dragging.value = null;
  dropHint.value = null;
}

function hintClass(node: TreeNode) {
  const hint = dropHint.value;
  return hint?.id === node.id ? `drop-${hint.position}` : "";
}
</script>

<template>
  <template v-for="node in nodes" :key="node.id">
    <template v-if="node.type === 'folder'">
      <div
        class="row folder"
        :class="[{ target: node.id === folderId, dragged: dragging?.id === node.id }, hintClass(node)]"
        :style="{ paddingLeft: `${pad + 6}px` }"
        role="button"
        tabindex="0"
        :draggable="organizable && renamingId !== node.id"
        @click="renamingId !== node.id && toggle(node.id)"
        @keydown.enter="renamingId !== node.id && toggle(node.id)"
        @keydown="onKey($event, node)"
        @dblclick.stop="startRename(node)"
        @contextmenu.prevent="emit('menu', node, $event.clientX, $event.clientY)"
        @dragstart="onDragStart($event, node)"
        @dragover="onDragOver($event, node)"
        @drop="onDrop($event, node)"
        @dragend="onDragEnd"
      >
        <span class="chev">{{ collapsed.has(node.id) && !expandAll ? "▸" : "▾" }}</span>
        <input
          v-if="renamingId === node.id"
          ref="renameInput"
          v-model="draftName"
          class="rename"
          spellcheck="false"
          @click.stop
          @keydown.enter.stop="commitRename(node)"
          @keydown.esc.stop="renamingId = ''"
          @blur="commitRename(node)"
        />
        <span v-else class="name">{{ node.name }}</span>
      </div>
      <TreeView
        v-if="expandAll || !collapsed.has(node.id)"
        :nodes="node.children"
        :selected-id="selectedId"
        :folder-id="folderId"
        :dirty="dirty"
        :depth="(depth ?? 0) + 1"
        :expand-all="expandAll"
        :organizable="organizable"
        @select="emit('select', $event)"
        @folder="emit('folder', $event)"
        @menu="(n, x, y) => emit('menu', n, x, y)"
        @rename="(id, name) => emit('rename', id, name)"
        @drop="(id, target, position) => emit('drop', id, target, position)"
      />
    </template>
    <div
      v-else
      class="row item"
      :class="[{ active: node.id === selectedId, dragged: dragging?.id === node.id }, hintClass(node)]"
      :style="{ paddingLeft: `${pad + 8}px` }"
      role="button"
      tabindex="0"
      :draggable="organizable && renamingId !== node.id"
      @click="renamingId !== node.id && emit('select', node)"
      @keydown.enter="renamingId !== node.id && emit('select', node)"
      @keydown="onKey($event, node)"
      @dblclick.stop="startRename(node)"
      @contextmenu.prevent="emit('menu', node, $event.clientX, $event.clientY)"
      @dragstart="onDragStart($event, node)"
      @dragover="onDragOver($event, node)"
      @drop="onDrop($event, node)"
      @dragend="onDragEnd"
    >
      <span class="badge" :class="badgeClass(node)">{{ badge(node) }}</span>
      <input
        v-if="renamingId === node.id"
        ref="renameInput"
        v-model="draftName"
        class="rename"
        spellcheck="false"
        @click.stop
        @keydown.enter.stop="commitRename(node)"
        @keydown.esc.stop="renamingId = ''"
        @blur="commitRename(node)"
      />
      <span v-else class="name">{{ node.name }}</span>
      <span v-if="dirty.has(node.id)" class="dot" title="Unsaved changes"></span>
    </div>
  </template>
</template>

<style scoped>
.row {
  position: relative;
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 6px 8px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  cursor: pointer;
  text-align: left;
  user-select: none;
}
.row:hover {
  background: var(--surface-2);
}
.row:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}
.row.dragged {
  opacity: 0.45;
}
/* Where a dragged item would land. */
.row.drop-before::before,
.row.drop-after::after {
  content: "";
  position: absolute;
  left: 8px;
  right: 8px;
  height: 2px;
  border-radius: 2px;
  background: var(--accent);
}
.row.drop-before::before {
  top: -1px;
}
.row.drop-after::after {
  bottom: -1px;
}
.row.drop-into {
  background: var(--accent-soft);
  box-shadow: inset 0 0 0 1px var(--accent);
}
.folder {
  margin-top: 6px;
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--muted);
}
.folder.target {
  color: var(--text);
}
.chev {
  width: 10px;
}
.item.active {
  background: var(--accent-soft);
}
.badge {
  flex: none;
  width: 46px;
  font: 600 10px/1 var(--mono);
  text-align: right;
}
.name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.rename {
  flex: 1;
  min-width: 0;
  border: 1px solid var(--accent);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text);
  padding: 2px 6px;
  font: inherit;
  text-transform: none;
  letter-spacing: 0;
  outline: none;
}
.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--accent);
  flex: none;
}
.m-get { color: var(--m-get); }
.m-post { color: var(--m-post); }
.m-put { color: var(--m-put); }
.m-patch { color: var(--m-patch); }
.m-delete { color: var(--m-delete); }
.m-head, .m-options { color: var(--muted); }
.p-graphql { color: var(--p-graphql); }
.p-grpc { color: var(--p-grpc); }
.p-websocket { color: var(--p-websocket); }
.p-soap { color: var(--p-soap); }
.p-sse { color: var(--p-sse); }
</style>
