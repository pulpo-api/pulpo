/** Pasting a curl command or a URL into Pulpo turns it into a request. */
import type { Auth, Form, Header, HttpMethod, HttpRequest } from "./core";
import { HTTP_METHODS } from "./core";

export interface Pasted {
  method: HttpMethod;
  url: string;
  headers: Header[];
  body: string | null;
  auth: Auth | null;
  form: Form | null;
  /** A short name for the new request: `GET /orders`. */
  name: string;
}

/** Shell words of a curl command copied for bash, zsh, PowerShell or Windows cmd. */
export function shellWords(text: string): string[] {
  return /\^"|\^\r?\n/.test(text) ? cmdWords(text) : posixWords(text);
}

/** Windows cmd ("Copy as cURL (cmd)"): `^` escapes, then the C runtime's quoting rules. */
function cmdWords(text: string): string[] {
  const src = text.replace(/\^\r?\n/g, " ").replace(/\^([\s\S])/g, "$1");
  const words: string[] = [];
  let word = "";
  let started = false;
  let quoted = false;
  for (let i = 0; i < src.length; i++) {
    const c = src[i];
    if (c === "\\" && src[i + 1] === '"') {
      word += '"';
      started = true;
      i++;
    } else if (c === '"') {
      quoted = !quoted;
      started = true;
    } else if (!quoted && /\s/.test(c)) {
      if (started) words.push(word);
      word = "";
      started = false;
    } else {
      word += c;
      started = true;
    }
  }
  if (started) words.push(word);
  return words;
}

/** bash/zsh (and PowerShell's backtick line breaks); new lines inside quotes are kept. */
function posixWords(text: string): string[] {
  const src = text.replace(/[\`]\r?\n/g, " ");
  const words: string[] = [];
  let word = "";
  let started = false;
  let i = 0;
  const push = () => {
    if (started) words.push(word);
    word = "";
    started = false;
  };
  while (i < src.length) {
    const c = src[i];
    if (/\s/.test(c)) {
      push();
      i++;
    } else if (c === "$" && src[i + 1] === "'") {
      // ANSI-C quoting: $'line\nbreak'.
      i += 2;
      started = true;
      while (i < src.length && src[i] !== "'") {
        if (src[i] === "\\" && i + 1 < src.length) {
          const e = src[i + 1];
          word += e === "n" ? "\n" : e === "t" ? "\t" : e === "r" ? "\r" : e;
          i += 2;
        } else word += src[i++];
      }
      i++;
    } else if (c === "'") {
      i++;
      started = true;
      while (i < src.length && src[i] !== "'") word += src[i++];
      i++;
    } else if (c === '"') {
      i++;
      started = true;
      while (i < src.length && src[i] !== '"') {
        if (src[i] === "\\" && i + 1 < src.length && /["\$`]/.test(src[i + 1])) {
          word += src[i + 1];
          i += 2;
        } else word += src[i++];
      }
      i++;
    } else if (c === "\\" && i + 1 < src.length) {
      word += src[i + 1];
      started = true;
      i += 2;
    } else {
      word += c;
      started = true;
      i++;
    }
  }
  push();
  return words;
}

const VALUE_FLAGS = new Set([
  "-X", "--request", "-H", "--header", "-d", "--data", "--data-raw", "--data-ascii", "--data-binary",
  "--data-urlencode", "--json", "-u", "--user", "-b", "--cookie", "-A", "--user-agent", "-e", "--referer",
  "-F", "--form", "--form-string", "--url", "--oauth2-bearer", "-o", "--output", "-m", "--max-time",
  "--connect-timeout", "-x", "--proxy", "-w", "--write-out", "-T", "--upload-file", "--cert", "--key",
  "--cacert", "-E", "-c", "--cookie-jar", "-r", "--range", "--retry", "-K", "--config", "--resolve",
]);

function pathName(url: string): string {
  try {
    const u = new URL(url.replace(/\{\{[^}]*\}\}/g, "x"));
    return u.pathname === "/" ? u.host : u.pathname;
  } catch {
    return url.replace(/^\w+:\/\//, "").slice(0, 40);
  }
}

/** A curl command as a request, or null when the text is not one. */
export function parseCurl(text: string): Pasted | null {
  const words = shellWords(text.trim());
  if (!words.length || !/^curl(\.exe)?$/i.test(words[0])) return null;
  let method: string | null = null;
  let url = "";
  const headers: Header[] = [];
  const data: string[] = [];
  const formFields: Form["fields"] = [];
  let auth: Auth | null = null;
  let get = false;
  let head = false;
  let json = false;
  for (let i = 1; i < words.length; i++) {
    let flag = words[i];
    let value: string | undefined;
    if (flag.startsWith("--") && flag.includes("=")) {
      value = flag.slice(flag.indexOf("=") + 1);
      flag = flag.slice(0, flag.indexOf("="));
    } else if (/^-[A-Za-z]./.test(flag) && VALUE_FLAGS.has(flag.slice(0, 2))) {
      // -XPOST, -H'Accept: x'
      value = flag.slice(2);
      flag = flag.slice(0, 2);
    }
    const takes = VALUE_FLAGS.has(flag);
    if (takes && value === undefined) value = words[++i] ?? "";
    switch (flag) {
      case "-X":
      case "--request":
        method = value!.toUpperCase();
        break;
      case "-H":
      case "--header": {
        const at = value!.indexOf(":");
        if (at > 0) headers.push({ name: value!.slice(0, at).trim(), value: value!.slice(at + 1).trim(), enabled: true });
        break;
      }
      case "-d":
      case "--data":
      case "--data-raw":
      case "--data-ascii":
      case "--data-binary":
      case "--data-urlencode":
        data.push(value!);
        break;
      case "--json":
        data.push(value!);
        json = true;
        break;
      case "-u":
      case "--user": {
        const at = value!.indexOf(":");
        auth = { type: "basic", username: at < 0 ? value! : value!.slice(0, at), password: at < 0 ? "" : value!.slice(at + 1) };
        break;
      }
      case "--oauth2-bearer":
        auth = { type: "bearer", token: value! };
        break;
      case "-b":
      case "--cookie":
        if (value!.includes("=")) headers.push({ name: "Cookie", value: value!, enabled: true });
        break;
      case "-A":
      case "--user-agent":
        headers.push({ name: "User-Agent", value: value!, enabled: true });
        break;
      case "-e":
      case "--referer":
        headers.push({ name: "Referer", value: value!, enabled: true });
        break;
      case "-F":
      case "--form":
      case "--form-string": {
        const at = value!.indexOf("=");
        if (at > 0) {
          const v = value!.slice(at + 1);
          const file = flag !== "--form-string" && v.startsWith("@");
          formFields.push({ name: value!.slice(0, at), value: file ? v.slice(1).replace(/;type=.*$/, "") : v, file, enabled: true });
        }
        break;
      }
      case "--url":
        url = value!;
        break;
      case "-G":
      case "--get":
        get = true;
        break;
      case "-I":
      case "--head":
        head = true;
        break;
      default:
        if (!takes && !flag.startsWith("-") && !url) url = flag;
    }
  }
  if (!url) return null;
  if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(url) && !url.startsWith("{{")) url = `http://${url}`;
  let body: string | null = data.length ? data.join("&") : null;
  if (get && body) {
    url += (url.includes("?") ? "&" : "?") + body;
    body = null;
  }
  const has = (name: string) => headers.some((h) => h.name.toLowerCase() === name);
  if (json) {
    if (!has("content-type")) headers.push({ name: "Content-Type", value: "application/json", enabled: true });
    if (!has("accept")) headers.push({ name: "Accept", value: "application/json", enabled: true });
  }
  const bearer = headers.find((h) => h.name.toLowerCase() === "authorization" && /^bearer\s/i.test(h.value));
  if (bearer && !auth) {
    auth = { type: "bearer", token: bearer.value.replace(/^bearer\s+/i, "") };
    headers.splice(headers.indexOf(bearer), 1);
  }
  const form: Form | null = formFields.length ? { kind: "multipart", fields: formFields } : null;
  const guessed = head ? "HEAD" : body !== null || form ? "POST" : "GET";
  const upper = (method ?? guessed) as HttpMethod;
  const final: HttpMethod = HTTP_METHODS.includes(upper) ? upper : guessed;
  if (form) {
    const ct = headers.findIndex((h) => h.name.toLowerCase() === "content-type" && /multipart/i.test(h.value));
    if (ct >= 0) headers.splice(ct, 1);
  }
  return { method: final, url, headers, body: form ? null : body, auth, form, name: `${final} ${pathName(url)}` };
}

/** `https://…`, `ws://…` or `GET https://…` as a request; null otherwise. */
export function parseUrlLine(text: string): Pasted | null {
  const line = text.trim();
  if (!line || /\s/.test(line.replace(/^([A-Za-z]+)\s+/, ""))) return null;
  const m = /^(?:([A-Za-z]+)\s+)?((?:https?|wss?|grpcs?|mqtts?):\/\/\S+)$/i.exec(line);
  if (!m) return null;
  const method = (m[1]?.toUpperCase() ?? "GET") as HttpMethod;
  if (!HTTP_METHODS.includes(method)) return null;
  return { method, url: m[2], headers: [], body: null, auth: null, form: null, name: `${method} ${pathName(m[2])}` };
}

/** What a paste means: a curl command, a URL, or nothing to do. */
export function parsePaste(text: string): Pasted | null {
  return parseCurl(text) ?? parseUrlLine(text);
}

/** Puts what was pasted into a request, replacing its method, URL, headers, body and auth. */
export function applyPasted(request: HttpRequest, pasted: Pasted) {
  request.method = pasted.method;
  request.url = pasted.url;
  request.headers = pasted.headers;
  request.body = pasted.body;
  request.form = pasted.form;
  request.bodyFile = null;
  request.graphql = null;
  request.soap = null;
  request.grpc = null;
  request.mqtt = null;
  request.socketio = null;
  request.disabledParams = [];
  request.pathParams = [];
  if (pasted.auth) request.auth = pasted.auth;
}
