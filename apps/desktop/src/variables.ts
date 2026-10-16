/** UI-side `{{variable}}` substitution, used for previews and the browser fallback.
 * The desktop app resolves for real in pulpo-core (variables.rs). */

export type Vars = Record<string, string>;

const DYNAMIC: Record<string, () => string> = {
  $guid: () => crypto.randomUUID(),
  $timestamp: () => String(Math.floor(Date.now() / 1000)),
  $isoTimestamp: () => new Date().toISOString(),
  $randomInt: () => String(Math.floor(Math.random() * 1001)),
};

/** What each dynamic variable gives, for tooltips and completion. */
export const DYNAMIC_DESCRIPTIONS: Record<string, string> = {
  $guid: "Dynamic: a new random UUID on every send",
  $timestamp: "Dynamic: Unix time in seconds when sending",
  $isoTimestamp: "Dynamic: ISO 8601 date and time when sending",
  $randomInt: "Dynamic: a random integer from 0 to 1000",
};

export function resolveText(text: string, vars: Vars, missing: Set<string>, preview = false): string {
  return text.replace(/\{\{([^{}]*)\}\}/g, (whole, raw: string) => {
    const name = raw.trim();
    if (!name) return whole;
    if (name in vars) return vars[name];
    if (name in DYNAMIC) return preview ? `<${name}>` : DYNAMIC[name]();
    missing.add(name);
    return whole;
  });
}
