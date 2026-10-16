<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { basicSetup } from "codemirror";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap, placeholder as placeholderExt } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { json } from "@codemirror/lang-json";
import { javascript } from "@codemirror/lang-javascript";
import { xml } from "@codemirror/lang-xml";
import { tags } from "@lezer/highlight";

export type Language = "json" | "javascript" | "xml" | "text";

const props = withDefaults(
  defineProps<{ modelValue: string; language?: Language; readonly?: boolean; placeholder?: string }>(),
  { language: "text", readonly: false, placeholder: "" },
);
const emit = defineEmits<{ "update:modelValue": [value: string]; run: []; save: [] }>();

const host = ref<HTMLDivElement | null>(null);
let view: EditorView | null = null;
const languageSlot = new Compartment();
const readonlySlot = new Compartment();

function languageExtension(language: Language) {
  switch (language) {
    case "json":
      return json();
    case "javascript":
      return javascript();
    case "xml":
      return xml();
    default:
      return [];
  }
}

const highlight = HighlightStyle.define([
  { tag: [tags.string, tags.special(tags.string)], color: "var(--syn-string)" },
  { tag: [tags.number, tags.bool, tags.null, tags.atom], color: "var(--syn-number)" },
  { tag: [tags.keyword, tags.controlKeyword, tags.operatorKeyword, tags.definitionKeyword], color: "var(--syn-keyword)" },
  { tag: [tags.propertyName, tags.attributeName], color: "var(--syn-property)" },
  { tag: [tags.function(tags.variableName), tags.function(tags.propertyName)], color: "var(--syn-function)" },
  { tag: [tags.tagName, tags.typeName, tags.className], color: "var(--syn-tag)" },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: "var(--muted)", fontStyle: "italic" },
  { tag: tags.invalid, color: "var(--m-delete)" },
]);

const theme = EditorView.theme({
  "&": { backgroundColor: "var(--surface-2)", color: "var(--text)", borderRadius: "8px", height: "100%", fontSize: "13px" },
  "&.cm-focused": { outline: "1px solid var(--accent)" },
  ".cm-scroller": { fontFamily: "var(--mono)", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--accent)", padding: "8px 0" },
  ".cm-gutters": { backgroundColor: "transparent", color: "var(--muted)", border: "none" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "transparent" },
  "&.cm-focused .cm-activeLine": { backgroundColor: "var(--accent-soft)" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": { backgroundColor: "var(--accent-soft) !important" },
  ".cm-cursor": { borderLeftColor: "var(--accent)" },
  ".cm-placeholder": { color: "var(--muted)" },
  ".cm-foldPlaceholder": { backgroundColor: "var(--surface)", border: "1px solid var(--border)", color: "var(--muted)" },
  ".cm-tooltip": { backgroundColor: "var(--surface)", border: "1px solid var(--border)" },
  ".cm-matchingBracket": { backgroundColor: "var(--accent-soft)", outline: "none" },
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
        basicSetup,
        theme,
        syntaxHighlighting(highlight),
        EditorView.lineWrapping,
        placeholderExt(props.placeholder),
        languageSlot.of(languageExtension(props.language)),
        readonlySlot.of([EditorState.readOnly.of(props.readonly), EditorView.editable.of(!props.readonly)]),
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

watch(
  () => props.language,
  (language) => view?.dispatch({ effects: languageSlot.reconfigure(languageExtension(language)) }),
);

watch(
  () => props.readonly,
  (readonly) =>
    view?.dispatch({
      effects: readonlySlot.reconfigure([EditorState.readOnly.of(readonly), EditorView.editable.of(!readonly)]),
    }),
);

onBeforeUnmount(() => view?.destroy());
</script>

<template>
  <div ref="host" class="code-editor"></div>
</template>

<style scoped>
.code-editor {
  flex: 1;
  min-height: 0;
  overflow: hidden;
  display: flex;
}
.code-editor :deep(.cm-editor) {
  flex: 1;
  min-width: 0;
}
</style>
