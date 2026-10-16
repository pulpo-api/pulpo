<script setup lang="ts">
import type { Folder, RequestItem } from "../sample";

defineProps<{ folders: Folder[]; selectedId: string }>();
const emit = defineEmits<{ select: [item: RequestItem] }>();

function badge(item: RequestItem): string {
  return item.protocol === "http" ? item.request.method : item.protocol.toUpperCase().slice(0, 4);
}

function badgeClass(item: RequestItem): string {
  return item.protocol === "http" ? `m-${item.request.method.toLowerCase()}` : `p-${item.protocol}`;
}
</script>

<template>
  <aside class="sidebar">
    <div class="brand">
      <img src="/pulpo.svg" alt="" width="28" height="28" />
      <span>Pulpo</span>
    </div>
    <input class="search" placeholder="Search requests…" />
    <nav>
      <section v-for="folder in folders" :key="folder.name">
        <h3>{{ folder.name }}</h3>
        <button
          v-for="item in folder.items"
          :key="item.id"
          class="item"
          :class="{ active: item.id === selectedId }"
          @click="emit('select', item)"
        >
          <span class="badge" :class="badgeClass(item)">{{ badge(item) }}</span>
          <span class="name">{{ item.name }}</span>
        </button>
      </section>
    </nav>
    <footer>
      <span class="dot"></span>
      main · git clean
    </footer>
  </aside>
</template>

<style scoped>
.sidebar {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px 12px;
  background: var(--surface);
  border-right: 1px solid var(--border);
  min-width: 0;
}
.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  font-weight: 700;
  font-size: 17px;
  padding: 0 6px;
}
.search {
  border: 1px solid var(--border);
  background: var(--surface-2);
  border-radius: 8px;
  padding: 8px 10px;
  outline: none;
}
.search:focus {
  border-color: var(--accent);
}
nav {
  flex: 1;
  overflow: auto;
}
h3 {
  margin: 14px 6px 6px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}
.item {
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
.item:hover {
  background: var(--surface-2);
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
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.m-get { color: var(--m-get); }
.m-post { color: var(--m-post); }
.m-put { color: var(--m-put); }
.m-patch { color: var(--m-patch); }
.m-delete { color: var(--m-delete); }
.p-graphql { color: var(--p-graphql); }
.p-grpc { color: var(--p-grpc); }
.p-websocket { color: var(--p-websocket); }
.p-soap { color: var(--p-soap); }
.p-sse { color: var(--p-sse); }
footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 6px;
  font-size: 12px;
  color: var(--muted);
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--ok);
}
</style>
