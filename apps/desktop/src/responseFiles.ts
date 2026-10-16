import type { HttpResponse } from "./core";

/** What the Preview view can show for a response. */
export type PreviewKind = "html" | "image" | "pdf" | "binary" | null;

export function contentTypeOf(response: HttpResponse): string {
  return (response.headers.find((h) => h.name.toLowerCase() === "content-type")?.value ?? "").toLowerCase();
}

/** Mirrors `is_binary_type` in pulpo-core. */
export function isBinaryType(contentType: string): boolean {
  const ct = contentType.toLowerCase();
  return (
    ["image/", "audio/", "video/", "font/"].some((p) => ct.startsWith(p)) ||
    [
      "application/pdf",
      "application/octet-stream",
      "application/zip",
      "application/gzip",
      "application/x-gzip",
      "application/msword",
      "application/vnd.openxmlformats",
      "application/vnd.ms-excel",
      "application/x-tar",
      "application/x-7z",
      "application/wasm",
    ].some((p) => ct.startsWith(p))
  );
}

export function previewKind(response: HttpResponse): PreviewKind {
  const ct = contentTypeOf(response);
  if (ct.includes("html")) return "html";
  if (ct.startsWith("image/") && response.bodyBase64) return "image";
  if (ct.startsWith("application/pdf") && response.bodyBase64) return "pdf";
  if (response.bodyBase64 || !response.bodyIsText) return "binary";
  return null;
}

export function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  return btoa(binary);
}

const EXTENSIONS: [string, string][] = [
  ["json", "json"],
  ["html", "html"],
  ["xml", "xml"],
  ["csv", "csv"],
  ["text/plain", "txt"],
  ["javascript", "js"],
  ["pdf", "pdf"],
  ["png", "png"],
  ["jpeg", "jpg"],
  ["gif", "gif"],
  ["webp", "webp"],
  ["svg", "svg"],
  ["zip", "zip"],
  ["gzip", "gz"],
];

/** A file name for saving: Content-Disposition's, else the last part of the URL, with an
 * extension that matches the content type. */
export function suggestedFileName(response: HttpResponse, url: string): string {
  const disposition = response.headers.find((h) => h.name.toLowerCase() === "content-disposition")?.value ?? "";
  const star = /filename\*\s*=\s*(?:UTF-8|utf-8)''([^;]+)/.exec(disposition);
  const plain = /filename\s*=\s*"?([^";]+)"?/.exec(disposition);
  const fromHeader = star ? decodeURIComponent(star[1].trim()) : plain?.[1].trim();
  if (fromHeader) return fromHeader.replace(/[\\/]/g, "_");

  const ct = contentTypeOf(response);
  const ext = EXTENSIONS.find(([needle]) => ct.includes(needle))?.[1] ?? (response.bodyBase64 ? "bin" : "txt");
  let last = "";
  try {
    last = decodeURIComponent(new URL(url).pathname.split("/").filter(Boolean).pop() ?? "");
  } catch {
    last = "";
  }
  last = last.replace(/[^\w.-]+/g, "_");
  if (!last) return `response.${ext}`;
  return /\.\w{1,5}$/.test(last) ? last : `${last}.${ext}`;
}
