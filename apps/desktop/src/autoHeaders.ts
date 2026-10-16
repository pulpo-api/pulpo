import { bodyMode, type Auth, type HttpRequest } from "./core";

/** A header Pulpo (or its HTTP client) adds by itself when sending. */
export interface AutoHeader {
  name: string;
  value: string;
  /** Where it comes from, for the user. */
  from: string;
  /** How to stop it: a disabled header of the same name, the cookies switch, or not at all. */
  stop: "header" | "cookies" | null;
  /** Whether writing the same header replaces it. */
  override: boolean;
}

/** Mirrors `mime_for` in pulpo-core (uploads). */
const MIME: Record<string, string> = {
  json: "application/json",
  xml: "application/xml",
  txt: "text/plain",
  log: "text/plain",
  csv: "text/csv",
  html: "text/html",
  htm: "text/html",
  pdf: "application/pdf",
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  zip: "application/zip",
  gz: "application/gzip",
};

/** Mirrors `sniff_content_type` in pulpo-core: the type of a raw body without a header. */
export function sniffContentType(body: string): string {
  const start = body.trimStart();
  const head = start.slice(0, 15).toLowerCase();
  if (start.startsWith("{") || start.startsWith("[")) return "application/json";
  if (head.startsWith("<!doctype html") || head.startsWith("<html")) return "text/html";
  if (start.startsWith("<")) return "application/xml";
  return "text/plain";
}

function contentType(request: HttpRequest): { value: string; stop: AutoHeader["stop"] } | null {
  switch (bodyMode(request)) {
    case "raw":
      return request.body ? { value: sniffContentType(request.body), stop: "header" } : null;
    case "urlencoded":
      return { value: "application/x-www-form-urlencoded", stop: "header" };
    case "multipart":
      // The boundary goes in it, so the client always writes it.
      return { value: "multipart/form-data; boundary=…", stop: null };
    case "file": {
      const ext = (request.bodyFile ?? "").split(".").pop()?.toLowerCase() ?? "";
      return { value: MIME[ext] ?? "application/octet-stream", stop: "header" };
    }
    default:
      return null;
  }
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return "from the URL";
  }
}

/** The headers that go out without being written: what Pulpo, the body, the auth and the
 * cookie jar add. `url` is the resolved URL (for Host). */
export function automaticHeaders(
  request: HttpRequest,
  context: { version: string; auth: Auth; environment: string; url: string },
): AutoHeader[] {
  const out: AutoHeader[] = [
    { name: "User-Agent", value: `Pulpo/${context.version}`, from: "Pulpo", stop: "header", override: true },
    { name: "Accept", value: "*/*", from: "Pulpo", stop: null, override: true },
    {
      name: "Accept-Encoding",
      value: "gzip, deflate, br",
      from: "Pulpo (it decompresses the response)",
      stop: "header",
      override: true,
    },
    { name: "Host", value: hostOf(context.url), from: "the URL", stop: null, override: false },
  ];
  const type = contentType(request);
  if (type) {
    out.push({ name: "Content-Type", value: type.value, from: "the body", stop: type.stop, override: type.stop !== null });
    out.push({ name: "Content-Length", value: "calculated when sending", from: "the body", stop: null, override: false });
  }
  const auth = context.auth;
  if (auth.type === "bearer") {
    out.push({ name: "Authorization", value: `Bearer ${auth.token}`, from: "the Auth tab", stop: null, override: true });
  } else if (auth.type === "basic") {
    out.push({
      name: "Authorization",
      value: `Basic (${auth.username}:… in base64)`,
      from: "the Auth tab",
      stop: null,
      override: true,
    });
  } else if (auth.type === "apiKey" && auth.in === "header") {
    out.push({ name: auth.key, value: auth.value, from: "the Auth tab", stop: null, override: true });
  }
  if (request.cookies ?? true) {
    out.push({
      name: "Cookie",
      value: "when the jar has one for this domain",
      from: `the cookie jar (${context.environment || "no environment"})`,
      stop: "cookies",
      override: true,
    });
  }
  return out;
}
