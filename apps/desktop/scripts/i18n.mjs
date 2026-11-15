// Checks the translations in src/locales against the texts the app shows.
// Usage: node scripts/i18n.mjs            (report)
//        node scripts/i18n.mjs --keys F   (also write every text to F, to start a new language)
// The English text is the key: wrap new texts in $t("…") in templates or t("…") in code.
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";

const SRC = new URL("../src/", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const TERNARY = /(?<![\w.])\$?t\([^"'()]*\?\s*("(?:[^"\\]|\\.)*")\s*:\s*("(?:[^"\\]|\\.)*")/g;
const CALL =/(?<![\w.])\$?t\(\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')/g;
// Fixed lists shown through $t(variable).
const LISTS = {
  "App.vue": [/\["[^"]+",\s*"([^"]+)"\]/g],
  "components/ExportDialog.vue": [/label:\s*"([^"]+)"/g, /hint:\s*\n?\s*"([^"]+)"/g],
  "components/BodyEditor.vue": [/label:\s*"([^"]+)"/g],
  "components/ResponseViewer.vue": [/label:\s*"([^"]+)"/g, /^\s+\w+:\s*"([A-Z][^"]+)",$/gm],
  "components/ImportDialog.vue": [/^\s+\w+:\s*"([^"]+)",?$/gm],
};
const GROUPS = ["Request", "Flow", "Environment", "Action", "Language"];

function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return name === "locales" ? [] : files(path);
    return /\.(vue|ts)$/.test(name) && !name.endsWith(".d.ts") ? [path] : [];
  });
}

const keys = new Set(GROUPS);
for (const file of files(SRC)) {
  const text = readFileSync(file, "utf8").replaceAll("&quot;", '"');
  for (const m of text.matchAll(CALL)) keys.add(m[1].slice(1, -1).replace(/\\(.)/g, "$1"));
  // $t(cond ? "One" : "Many {0}", …): both texts are keys.
  for (const m of text.matchAll(TERNARY)) {
    keys.add(m[1].slice(1, -1).replace(/\\(.)/g, "$1"));
    keys.add(m[2].slice(1, -1).replace(/\\(.)/g, "$1"));
  }
  for (const pattern of LISTS[relative(SRC, file).replaceAll("\\", "/")] ?? []) {
    for (const m of text.matchAll(pattern)) keys.add(m[1]);
  }
}
const all = [...keys].filter((k) => /[A-Za-z]/.test(k)).sort();
const keysOut = process.argv.indexOf("--keys");
if (keysOut > 0) writeFileSync(process.argv[keysOut + 1], JSON.stringify(all, null, 1) + "\n");

const holes = (s) => (s.match(/\{\w+\}/g) ?? []).sort().join();
let failed = false;
for (const name of readdirSync(join(SRC, "locales")).filter((n) => n.endsWith(".json"))) {
  const dict = JSON.parse(readFileSync(join(SRC, "locales", name), "utf8"));
  const missing = all.filter((k) => !dict[k]);
  const broken = all.filter((k) => dict[k] && holes(k) !== holes(dict[k]));
  const unused = Object.keys(dict).filter((k) => !keys.has(k));
  const done = Math.round((100 * (all.length - missing.length)) / all.length);
  console.log(`${name}: ${done}% (${missing.length} missing, ${broken.length} with other placeholders, ${unused.length} unused)`);
  for (const k of broken) console.log(`  placeholders: ${JSON.stringify(k)}`);
  if (broken.length) failed = true;
}
console.log(`${all.length} texts in the app`);
process.exit(failed ? 1 : 0);
