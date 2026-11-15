<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";

export interface PaletteItem {
  id: string;
  label: string;
  /** Group shown on the right: Request, Flow, Environment, Action… */
  group: string;
  /** Folder path, shortcut or other context, in grey. */
  hint?: string;
  /** HTTP method (or WS, GQL…) shown as a coloured badge. */
  badge?: string;
  run: () => void;
}

const props = defineProps<{ items: PaletteItem[]; placeholder: string; empty: string }>();
const emit = defineEmits<{ close: [] }>();

const query = ref("");
const active = ref(0);
const input = ref<HTMLInputElement | null>(null);
const list = ref<HTMLElement | null>(null);

/** Subsequence match: every typed character in order; consecutive and word-start hits score more. */
function score(text: string, q: string): number {
  if (!q) return 1;
  const t = text.toLowerCase();
  let at = 0;
  let total = 0;
  let streak = 0;
  for (const ch of q) {
    const found = t.indexOf(ch, at);
    if (found < 0) return 0;
    streak = found === at ? streak + 1 : 1;
    const wordStart = found === 0 || /[\s/._-]/.test(t[found - 1]);
    total += streak * 2 + (wordStart ? 3 : 0);
    at = found + 1;
  }
  return total - t.length / 100;
}

const shown = computed(() => {
  const raw = query.value.trim().toLowerCase();
  // ">" lists only actions, as in VS Code.
  const actionsOnly = raw.startsWith(">");
  const q = (actionsOnly ? raw.slice(1) : raw).replace(/\s+/g, " ").trim();
  return props.items
    .filter((i) => !actionsOnly || i.id.startsWith("action:"))
    .map((item) => ({ item, s: Math.max(score(item.label, q), score(`${item.hint ?? ""} ${item.label}`, q) * 0.8) }))
    .filter((x) => x.s > 0)
    .sort((a, b) => b.s - a.s)
    .slice(0, 60)
    .map((x) => x.item);
});

watch(query, () => (active.value = 0));
watch(active, async () => {
  await nextTick();
  list.value?.querySelector(".on")?.scrollIntoView({ block: "nearest" });
});

function run(item: PaletteItem | undefined) {
  if (!item) return;
  emit("close");
  item.run();
}

function onKey(event: KeyboardEvent) {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    active.value = Math.min(active.value + 1, shown.value.length - 1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    active.value = Math.max(active.value - 1, 0);
  } else if (event.key === "Enter") {
    event.preventDefault();
    run(shown.value[active.value]);
  } else if (event.key === "Escape") {
    event.preventDefault();
    emit("close");
  }
}

onMounted(() => input.value?.focus());
</script>

<template>
  <div class="backdrop" @click.self="emit('close')">
    <section class="card palette" role="dialog" :aria-label="placeholder">
      <input
        ref="input"
        v-model="query"
        class="query"
        :placeholder="placeholder"
        :aria-label="placeholder"
        role="combobox"
        aria-expanded="true"
        aria-controls="palette-list"
        @keydown="onKey"
      />
      <ul id="palette-list" ref="list" role="listbox">
        <li
          v-for="(item, i) in shown"
          :key="item.id"
          role="option"
          :aria-selected="i === active"
          :class="{ on: i === active }"
          @mousemove="active = i"
          @click="run(item)"
        >
          <span v-if="item.badge" class="badge" :class="`m-${item.badge.toLowerCase()}`">{{ item.badge }}</span>
          <span class="label">{{ item.label }}</span>
          <span v-if="item.hint" class="hint">{{ item.hint }}</span>
          <span class="group">{{ $t(item.group) }}</span>
        </li>
        <li v-if="!shown.length" class="none">{{ empty }}</li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.backdrop {
  position: fixed;
  inset: 0;
  background: rgba(10, 12, 20, 0.35);
  display: flex;
  justify-content: center;
  align-items: flex-start;
  padding-top: 12vh;
  z-index: 20;
}
.palette {
  width: min(640px, 94vw);
  padding: 8px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.query {
  width: 100%;
  font: inherit;
  font-size: 15px;
  padding: 8px 10px;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: var(--surface-2);
  color: var(--text);
  outline: none;
}
.query:focus {
  border-color: var(--accent);
}
ul {
  list-style: none;
  margin: 0;
  padding: 0;
  max-height: 52vh;
  overflow: auto;
}
li {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 6px 10px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 13.5px;
}
li.on {
  background: var(--accent-soft);
}
.badge {
  font: 700 10.5px var(--mono);
  width: 46px;
  flex: none;
}
.label {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.hint {
  color: var(--muted);
  font-size: 12px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  min-width: 0;
}
.group {
  margin-left: auto;
  color: var(--muted);
  font-size: 11px;
  flex: none;
}
.m-get { color: var(--m-get); }
.m-post { color: var(--m-post); }
.m-put { color: var(--m-put); }
.m-patch { color: var(--m-patch); }
.m-delete { color: var(--m-delete); }
.m-head,
.m-options { color: var(--muted); }
.none {
  color: var(--muted);
  cursor: default;
}
</style>
