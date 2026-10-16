<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { completionKeymap } from "@codemirror/autocomplete";
import { EditorState, Prec } from "@codemirror/state";
import { EditorView, drawSelection, keymap, placeholder as placeholderExt } from "@codemirror/view";
import { refreshVariables, variables } from "../cmVariables";
import { varContext } from "../varContext";

/** A one-line text field that highlights `{{variables}}`, shows their value on hover and
 * completes them after `{{`. Behaves like an input: no new lines, Enter emits `enter`. */
const props = withDefaults(defineProps<{ modelValue: string; placeholder?: string; ariaLabel?: string }>(), {
  placeholder: "",
  ariaLabel: "",
});
const emit = defineEmits<{ "update:modelValue": [value: string]; enter: []; run: []; save: [] }>();

const host = ref<HTMLDivElement | null>(null);
let view: EditorView | null = null;

const theme = EditorView.theme({
  "&": {
    backgroundColor: "var(--surface-2)",
    color: "var(--text)",
    border: "1px solid var(--border)",
    borderRadius: "6px",
    fontSize: "13px",
    minWidth: "0",
  },
  "&.cm-focused": { outline: "none", borderColor: "var(--accent)" },
  ".cm-scroller": { fontFamily: "var(--mono)", overflow: "hidden", lineHeight: "1.4" },
  ".cm-content": { padding: "5px 8px", caretColor: "var(--accent)" },
  ".cm-line": { padding: "0" },
  ".cm-placeholder": { color: "var(--muted)" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": { backgroundColor: "var(--accent-soft) !important" },
  ".cm-tooltip": { backgroundColor: "var(--surface)", border: "1px solid var(--border)", borderRadius: "6px" },
});

/** Pasted or typed line breaks become spaces: it is one line. */
const singleLine = EditorState.transactionFilter.of((tr) => {
  if (!tr.docChanged || tr.newDoc.lines === 1) return tr;
  const text = tr.newDoc.toString().replace(/\r?\n/g, " ");
  return [tr, { changes: { from: 0, to: tr.newDoc.length, insert: text }, sequential: true }];
});

onMounted(() => {
  view = new EditorView({
    parent: host.value!,
    state: EditorState.create({
      doc: props.modelValue,
      extensions: [
        Prec.highest(
          keymap.of([
            { key: "Mod-Enter", run: () => (emit("run"), true) },
            { key: "Mod-s", run: () => (emit("save"), true) },
          ]),
        ),
        variables(),
        // Enter accepts a completion first (completionKeymap); otherwise it is "enter".
        keymap.of([...completionKeymap, { key: "Enter", run: () => (emit("enter"), true) }, ...historyKeymap, ...defaultKeymap]),
        history(),
        drawSelection(),
        singleLine,
        theme,
        placeholderExt(props.placeholder),
        EditorView.contentAttributes.of({ "aria-label": props.ariaLabel || props.placeholder, spellcheck: "false" }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) emit("update:modelValue", update.state.doc.toString());
        }),
      ],
    }),
  });
});

watch(
  () => props.modelValue,
  (value) => {
    if (view && value !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } });
    }
  },
);

watch(varContext, () => view?.dispatch({ effects: refreshVariables.of(null) }));

onBeforeUnmount(() => view?.destroy());

defineExpose({ focus: () => view?.focus() });
</script>

<template>
  <div ref="host" class="var-input"></div>
</template>

<style scoped>
.var-input {
  min-width: 0;
}
</style>
