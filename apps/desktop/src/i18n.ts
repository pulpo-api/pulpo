/**
 * Pulpo's languages. The English text is the key: `t("Send")`. A missing translation
 * shows the English text, so nothing ever breaks. Dictionaries live in `locales/<code>.json`
 * and load when picked; contributors add a language by adding a file and a line here.
 */
import { ref } from "vue";

export const LOCALES = [
  { code: "en", name: "English" },
  { code: "es", name: "Español" },
  { code: "pt", name: "Português" },
  { code: "fr", name: "Français" },
  { code: "de", name: "Deutsch" },
  { code: "it", name: "Italiano" },
  { code: "zh", name: "中文（简体）" },
  { code: "ja", name: "日本語" },
] as const;
export type Locale = (typeof LOCALES)[number]["code"];

const STORE = "pulpo.locale";
const files = import.meta.glob<Record<string, string>>("./locales/*.json", { import: "default" });

function initial(): Locale {
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(STORE);
  } catch {
    // Private mode: fall back to the system language.
  }
  const wanted = (saved ?? navigator.language ?? "en").toLowerCase().slice(0, 2);
  return (LOCALES.find((l) => l.code === wanted)?.code ?? "en") as Locale;
}

export const locale = ref<Locale>("en");
const dictionary = ref<Record<string, string>>({});

export async function setLocale(code: Locale) {
  const loader = files[`./locales/${code}.json`];
  dictionary.value = code === "en" || !loader ? {} : await loader();
  locale.value = code;
  document.documentElement.lang = code;
  try {
    localStorage.setItem(STORE, code);
  } catch {
    // Not remembered; it still applies now.
  }
}

/** Loads the saved (or the system's) language; call before mounting the app. */
export function initLocale() {
  return setLocale(initial());
}

/** The text in the current language; `{name}` placeholders are filled from `vars`. */
export function t(text: string, vars?: Record<string, string | number | null | undefined>): string {
  const out = dictionary.value[text] || text;
  return vars ? out.replace(/\{(\w+)\}/g, (all, name) => (name in vars ? String(vars[name] ?? "") : all)) : out;
}
