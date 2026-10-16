/** CodeMirror support for `{{variables}}`: highlighting by where the value comes from,
 * a tooltip with the value and its source, and completion after `{{`. */
import { autocompletion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { EditorState, RangeSetBuilder, StateEffect } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  ViewPlugin,
  hoverTooltip,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { knownVariables, lookup } from "./varContext";

const PATTERN = /\{\{\s*([^{}]*?)\s*\}\}/g;

/** Dispatch it when the variables change (environment switched, a script set a value). */
export const refreshVariables = StateEffect.define<null>();

function decorate(view: EditorView): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  for (const { from, to } of view.visibleRanges) {
    const text = view.state.doc.sliceString(from, to);
    for (const match of text.matchAll(PATTERN)) {
      const name = match[1];
      if (!name) continue;
      const start = from + (match.index ?? 0);
      builder.add(
        start,
        start + match[0].length,
        Decoration.mark({ class: `cm-var cm-var-${lookup(name).source}` }),
      );
    }
  }
  return builder.finish();
}

const highlighter = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = decorate(view);
    }
    update(update: ViewUpdate) {
      const refreshed = update.transactions.some((tr) => tr.effects.some((e) => e.is(refreshVariables)));
      if (update.docChanged || update.viewportChanged || refreshed) this.decorations = decorate(update.view);
    }
  },
  { decorations: (plugin) => plugin.decorations },
);

const SOURCE_LABEL: Record<string, string> = {
  local: "local",
  environment: "environment",
  secret: "secret",
  dynamic: "dynamic",
  missing: "missing",
};

const tooltip = hoverTooltip((view, pos) => {
  const line = view.state.doc.lineAt(pos);
  for (const match of line.text.matchAll(PATTERN)) {
    const start = line.from + (match.index ?? 0);
    const end = start + match[0].length;
    if (pos < start || pos > end || !match[1]) continue;
    const info = lookup(match[1]);
    return {
      pos: start,
      end,
      above: true,
      create() {
        const dom = document.createElement("div");
        dom.className = "cm-var-tooltip";
        const head = dom.appendChild(document.createElement("div"));
        head.className = "cm-var-tooltip-head";
        head.textContent = info.name;
        const badge = head.appendChild(document.createElement("span"));
        badge.className = `cm-var-badge cm-var-badge-${info.source}`;
        badge.textContent = SOURCE_LABEL[info.source];
        if (info.value) {
          const value = dom.appendChild(document.createElement("code"));
          value.textContent = info.value.length > 200 ? `${info.value.slice(0, 200)}…` : info.value;
        }
        const detail = dom.appendChild(document.createElement("div"));
        detail.className = "cm-var-tooltip-detail";
        detail.textContent = info.detail;
        return { dom };
      },
    };
  }
  return null;
});

function complete(context: CompletionContext): CompletionResult | null {
  const before = context.matchBefore(/\{\{\s*[\w$.-]*$/);
  if (!before) return null;
  const nameStart = before.from + before.text.search(/[\w$.-]*$/);
  const closed = context.state.doc.sliceString(context.pos, context.pos + 2) === "}}";
  return {
    from: nameStart,
    options: knownVariables().map((info) => ({
      label: info.name,
      detail: info.source === "secret" ? "secret" : info.value || SOURCE_LABEL[info.source],
      info: info.detail,
      type: info.source === "dynamic" ? "function" : "variable",
      apply: closed ? info.name : `${info.name}}}`,
    })),
    validFor: /^[\w$.-]*$/,
  };
}

const theme = EditorView.baseTheme({
  ".cm-var": { borderRadius: "3px", padding: "0 1px" },
  ".cm-var-environment, .cm-var-secret": { color: "var(--accent)", backgroundColor: "var(--accent-soft)" },
  ".cm-var-local": { color: "var(--m-get)", backgroundColor: "rgba(60, 170, 110, 0.12)" },
  ".cm-var-dynamic": { color: "var(--m-patch)", backgroundColor: "rgba(150, 110, 220, 0.12)" },
  ".cm-var-missing": {
    color: "var(--m-delete)",
    textDecoration: "underline wavy var(--m-delete)",
    textUnderlineOffset: "3px",
  },
  ".cm-var-tooltip": { padding: "6px 8px", maxWidth: "360px", fontSize: "12px", lineHeight: "1.5" },
  ".cm-var-tooltip-head": { display: "flex", gap: "8px", alignItems: "center", fontFamily: "var(--mono)", fontWeight: "600" },
  ".cm-var-tooltip code": { display: "block", margin: "4px 0", wordBreak: "break-all", fontFamily: "var(--mono)" },
  ".cm-var-tooltip-detail": { color: "var(--muted)" },
  ".cm-var-badge": { fontFamily: "inherit", fontWeight: "500", fontSize: "10px", padding: "0 6px", borderRadius: "999px" },
  ".cm-var-badge-environment, .cm-var-badge-secret": { background: "var(--accent-soft)", color: "var(--accent)" },
  ".cm-var-badge-local": { background: "rgba(60, 170, 110, 0.15)", color: "var(--m-get)" },
  ".cm-var-badge-dynamic": { background: "rgba(150, 110, 220, 0.15)", color: "var(--m-patch)" },
  ".cm-var-badge-missing": { background: "rgba(220, 70, 70, 0.15)", color: "var(--m-delete)" },
});

/** Everything at once, for editors that hold request data (not scripts). Editors that
 * already have completion (basicSetup) get ours as a source instead of a second one. */
export function variables(options: { ownCompletion: boolean } = { ownCompletion: true }) {
  const completion = options.ownCompletion
    ? autocompletion({ override: [complete], icons: false })
    : EditorState.languageData.of(() => [{ autocomplete: complete }]);
  return [highlighter, tooltip, completion, theme];
}
