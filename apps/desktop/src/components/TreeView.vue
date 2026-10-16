<script setup lang="ts">
import { ref } from "vue";
import type { RequestEntry, TreeNode } from "../workspace";

defineOptions({ name: "TreeView" });

const props = defineProps<{
  nodes: TreeNode[];
  selectedId: string;
  folderId: string;
  dirty: Set<string>;
  depth?: number;
  expandAll?: boolean;
}>();
const emit = defineEmits<{ select: [entry: RequestEntry]; folder: [id: string] }>();

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
</script>

<template>
  <template v-for="node in nodes" :key="node.id">
    <template v-if="node.type === 'folder'">
      <button
        class="row folder"
        :class="{ target: node.id === folderId }"
        :style="{ paddingLeft: `${pad + 6}px` }"
        @click="toggle(node.id)"
      >
        <span class="chev">{{ collapsed.has(node.id) && !expandAll ? "▸" : "▾" }}</span>
        <span class="name">{{ node.name }}</span>
      </button>
      <TreeView
        v-if="expandAll || !collapsed.has(node.id)"
        :nodes="node.children"
        :selected-id="selectedId"
        :folder-id="folderId"
        :dirty="dirty"
        :depth="(depth ?? 0) + 1"
        :expand-all="expandAll"
        @select="emit('select', $event)"
        @folder="emit('folder', $event)"
      />
    </template>
    <button
      v-else
      class="row item"
      :class="{ active: node.id === selectedId }"
      :style="{ paddingLeft: `${pad + 8}px` }"
      @click="emit('select', node)"
    >
      <span class="badge" :class="badgeClass(node)">{{ badge(node) }}</span>
      <span class="name">{{ node.name }}</span>
      <span v-if="dirty.has(node.id)" class="dot" title="Unsaved changes"></span>
    </button>
  </template>
</template>

<style scoped>
.row {
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
}
.row:hover {
  background: var(--surface-2);
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
