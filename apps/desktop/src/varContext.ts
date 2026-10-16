import { shallowRef } from "vue";
import type { Variable } from "./workspace";
import { DYNAMIC_DESCRIPTIONS } from "./variables";

/** Where a `{{variable}}` gets its value. */
export type VarSource = "local" | "environment" | "secret" | "dynamic" | "missing";

export interface VarInfo {
  name: string;
  source: VarSource;
  /** What to show; secrets are never shown. */
  value: string;
  /** One line for the tooltip and the autocomplete list. */
  detail: string;
}

/** What the editors know about variables: the selected environment and the values set by
 * scripts (`.pulpo/local.json`), which win over it, as in pulpo-core. */
export const varContext = shallowRef<{ environment: string; variables: Variable[]; local: Record<string, string> }>({
  environment: "",
  variables: [],
  local: {},
});

export function lookup(name: string): VarInfo {
  const { environment, variables, local } = varContext.value;
  const env = environment || "no environment";
  if (name in local) {
    return { name, source: "local", value: local[name], detail: `Set by a script (local to this machine, ${env})` };
  }
  const variable = variables.find((v) => v.name === name);
  if (variable?.secret) {
    return variable.hasValue
      ? { name, source: "secret", value: "••••••", detail: `Secret of ${env}, kept in the OS keychain` }
      : { name, source: "missing", value: "", detail: `Secret of ${env} without a value: set it in ⚙` };
  }
  if (variable) return { name, source: "environment", value: variable.value, detail: `From ${env}` };
  if (name in DYNAMIC_DESCRIPTIONS) {
    return { name, source: "dynamic", value: "", detail: DYNAMIC_DESCRIPTIONS[name] };
  }
  return {
    name,
    source: "missing",
    value: "",
    detail: environment
      ? `Not defined in ${environment}: add it in ⚙, or set it from a script`
      : "No environment selected: pick one in the top bar",
  };
}

/** Every variable that can be used now, for autocompletion. */
export function knownVariables(): VarInfo[] {
  const { variables, local } = varContext.value;
  const names = new Set([...Object.keys(local), ...variables.map((v) => v.name), ...Object.keys(DYNAMIC_DESCRIPTIONS)]);
  return [...names].filter(Boolean).map(lookup);
}
