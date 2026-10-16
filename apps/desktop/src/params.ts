/** Query and path params of a URL, kept as typed: nothing is encoded or decoded, so
 * `{{variables}}` survive. Mirrors `path_param_names` / `apply_path_params` in pulpo-core. */

export interface Param {
  name: string;
  value: string;
}

export interface UrlParts {
  base: string;
  params: Param[];
  hash: string;
}

export function splitUrl(url: string): UrlParts {
  const hashAt = url.indexOf("#");
  const hash = hashAt >= 0 ? url.slice(hashAt) : "";
  const noHash = hashAt >= 0 ? url.slice(0, hashAt) : url;
  const queryAt = noHash.indexOf("?");
  if (queryAt < 0) return { base: noHash, params: [], hash };
  const params = noHash
    .slice(queryAt + 1)
    .split("&")
    .filter((pair) => pair.length > 0)
    .map((pair) => {
      const eq = pair.indexOf("=");
      return eq < 0 ? { name: pair, value: "" } : { name: pair.slice(0, eq), value: pair.slice(eq + 1) };
    });
  return { base: noHash.slice(0, queryAt), params, hash };
}

export function joinUrl(base: string, params: Param[], hash = ""): string {
  const query = params
    .filter((p) => p.name || p.value)
    .map((p) => (p.value === "" ? p.name : `${p.name}=${p.value}`))
    .join("&");
  return `${base}${query ? `?${query}` : ""}${hash}`;
}

function pathStart(url: string): number {
  const scheme = url.indexOf("://");
  if (scheme < 0) return 0;
  const slash = url.indexOf("/", scheme + 3);
  return slash < 0 ? url.length : slash;
}

const NAME = /^[A-Za-z0-9_-]+/;

/** Names of the `:name` path segments (never the port), in order. */
export function pathParamNames(url: string): string[] {
  const path = url.slice(pathStart(url)).split(/[?#]/)[0];
  return path
    .split("/")
    .filter((segment) => segment.startsWith(":"))
    .map((segment) => NAME.exec(segment.slice(1))?.[0] ?? "")
    .filter((name) => name.length > 0);
}

/** Replaces `:name` path segments with their encoded values. */
export function applyPathParams(url: string, params: Param[]): string {
  if (!params.length) return url;
  const start = pathStart(url);
  const head = url.slice(0, start);
  const rest = url.slice(start);
  const cut = rest.search(/[?#]/);
  const path = cut < 0 ? rest : rest.slice(0, cut);
  const tail = cut < 0 ? "" : rest.slice(cut);
  const replaced = path
    .split("/")
    .map((segment) => {
      if (!segment.startsWith(":")) return segment;
      const name = NAME.exec(segment.slice(1))?.[0] ?? "";
      const param = params.find((p) => p.name === name);
      return name && param ? encodeURIComponent(param.value) + segment.slice(1 + name.length) : segment;
    })
    .join("/");
  return head + replaced + tail;
}
