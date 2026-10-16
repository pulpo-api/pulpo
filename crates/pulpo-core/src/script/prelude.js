// Pulpo script runtime: a Postman-compatible `pm` API in plain JavaScript.
// Rust defines `__input` (JSON) before this file and calls __run(label, source)
// for every script, then __finish() to read the result back.
"use strict";

var __state = {
  changes: {},
  local: Object.assign({}, __input.local || {}),
  env: __input.env || {},
  request: __input.request,
  response: __input.response || null,
  tests: [],
  logs: [],
  errors: [],
};

function __str(value) {
  if (value === undefined) return "undefined";
  if (value === null) return "null";
  if (typeof value === "string") return value;
  if (value instanceof Error) return value.name + ": " + value.message;
  try {
    var json = JSON.stringify(value);
    return json === undefined ? String(value) : json;
  } catch (e) {
    return String(value);
  }
}

function __lookup(name) {
  if (Object.prototype.hasOwnProperty.call(__state.local, name)) return __state.local[name];
  if (Object.prototype.hasOwnProperty.call(__state.env, name)) return __state.env[name];
  return undefined;
}

function __uuid() {
  var hex = "0123456789abcdef";
  var out = "";
  for (var i = 0; i < 36; i++) {
    if (i === 8 || i === 13 || i === 18 || i === 23) out += "-";
    else if (i === 14) out += "4";
    else if (i === 19) out += hex[(Math.random() * 4) | 8];
    else out += hex[(Math.random() * 16) | 0];
  }
  return out;
}

var __dynamic = {
  $guid: __uuid,
  $timestamp: function () { return String(Math.floor(Date.now() / 1000)); },
  $isoTimestamp: function () { return new Date().toISOString(); },
  $randomInt: function () { return String(Math.floor(Math.random() * 1001)); },
};

function __replaceIn(text) {
  return String(text).replace(/\{\{([^{}]*)\}\}/g, function (whole, raw) {
    var name = raw.trim();
    var value = __lookup(name);
    if (value !== undefined) return value;
    if (__dynamic[name]) return __dynamic[name]();
    return whole;
  });
}

var __variables = {
  get: function (name) { return __lookup(name); },
  has: function (name) { return __lookup(name) !== undefined; },
  set: function (name, value) {
    var v = value === undefined || value === null ? "" : (typeof value === "string" ? value : __str(value));
    __state.local[name] = v;
    __state.changes[name] = v;
  },
  unset: function (name) {
    delete __state.local[name];
    __state.changes[name] = null;
  },
  clear: function () {
    Object.keys(__state.local).forEach(function (k) { __state.changes[k] = null; });
    __state.local = {};
  },
  toObject: function () { return Object.assign({}, __state.env, __state.local); },
  replaceIn: __replaceIn,
};

// ------------------------------------------------------------------ request

function __headerList() {
  return {
    get: function (name) {
      var n = String(name).toLowerCase();
      var h = __state.request.headers.filter(function (x) { return x.enabled && x.name.toLowerCase() === n; })[0];
      return h ? h.value : undefined;
    },
    has: function (name) { return this.get(name) !== undefined; },
    upsert: function (header) {
      var n = String(header.key).toLowerCase();
      var existing = __state.request.headers.filter(function (x) { return x.name.toLowerCase() === n; })[0];
      if (existing) {
        existing.value = String(header.value);
        existing.enabled = true;
      } else {
        __state.request.headers.push({ name: String(header.key), value: String(header.value), enabled: true });
      }
    },
    add: function (header) {
      __state.request.headers.push({ name: String(header.key), value: String(header.value), enabled: true });
    },
    remove: function (name) {
      var n = String(name).toLowerCase();
      __state.request.headers = __state.request.headers.filter(function (x) { return x.name.toLowerCase() !== n; });
    },
    toObject: function () {
      var out = {};
      __state.request.headers.forEach(function (x) { if (x.enabled) out[x.name] = x.value; });
      return out;
    },
  };
}

var __request = {
  get method() { return __state.request.method; },
  set method(value) { __state.request.method = String(value).toUpperCase(); },
  get url() {
    var raw = __state.request.url;
    return { toString: function () { return raw; }, raw: raw };
  },
  set url(value) { __state.request.url = String(value); },
  headers: __headerList(),
  body: {
    mode: "raw",
    get raw() { return __state.request.body === null ? "" : __state.request.body; },
    set raw(value) { __state.request.body = value === null || value === undefined ? null : String(value); },
  },
};

// ------------------------------------------------------------------ response

function __response() {
  return __state.response ? __responseApi(__state.response) : undefined;
}

function __responseApi(r) {
  var api = {
    code: r.status,
    status: r.statusText,
    responseTime: r.durationMs,
    responseSize: r.sizeBytes,
    headers: {
      get: function (name) {
        var n = String(name).toLowerCase();
        var h = r.headers.filter(function (x) { return x.name.toLowerCase() === n; })[0];
        return h ? h.value : undefined;
      },
      has: function (name) { return this.get(name) !== undefined; },
      toObject: function () {
        var out = {};
        r.headers.forEach(function (x) { out[x.name] = x.value; });
        return out;
      },
    },
    text: function () { return r.body; },
    json: function () { return JSON.parse(r.body); },
  };
  api.to = {
    have: {
      status: function (code) {
        if (typeof code === "number" ? r.status !== code : r.statusText !== code) {
          throw new Error("expected response to have status " + code + " but got " + r.status);
        }
      },
      header: function (name) {
        if (api.headers.get(name) === undefined) throw new Error("expected response to have header " + name);
      },
    },
    be: {
      get ok() {
        if (r.status < 200 || r.status > 299) throw new Error("expected a 2xx response but got " + r.status);
        return true;
      },
    },
  };
  return api;
}

// ------------------------------------------------------------------ expect (chai subset)

function __deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  var ka = Object.keys(a), kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  for (var i = 0; i < ka.length; i++) {
    if (!__deepEqual(a[ka[i]], b[ka[i]])) return false;
  }
  return true;
}

function __typeOf(v) {
  if (v === null) return "null";
  if (Array.isArray(v)) return "array";
  return typeof v;
}

function __expect(actual) {
  var negate = false;
  var deep = false;
  var a = {};
  function check(ok, message) {
    if (negate ? ok : !ok) throw new Error((negate ? "expected not: " : "expected: ") + message);
    return a;
  }
  ["to", "be", "been", "is", "that", "which", "and", "has", "have", "with", "at", "of", "same", "does", "still"].forEach(function (word) {
    Object.defineProperty(a, word, { get: function () { return a; } });
  });
  Object.defineProperty(a, "not", { get: function () { negate = !negate; return a; } });
  Object.defineProperty(a, "deep", { get: function () { deep = true; return a; } });
  a.equal = a.equals = a.eq = function (expected) {
    return check(deep ? __deepEqual(actual, expected) : actual === expected, __str(actual) + " to equal " + __str(expected));
  };
  a.eql = function (expected) { return check(__deepEqual(actual, expected), __str(actual) + " to deeply equal " + __str(expected)); };
  a.oneOf = function (list) { return check(list.indexOf(actual) >= 0, __str(actual) + " to be one of " + __str(list)); };
  a.within = function (lo, hi) { return check(actual >= lo && actual <= hi, __str(actual) + " to be within " + lo + ".." + hi); };
  a.above = a.greaterThan = a.gt = function (n) { return check(actual > n, __str(actual) + " to be above " + n); };
  a.below = a.lessThan = a.lt = function (n) { return check(actual < n, __str(actual) + " to be below " + n); };
  a.least = a.gte = function (n) { return check(actual >= n, __str(actual) + " to be at least " + n); };
  a.most = a.lte = function (n) { return check(actual <= n, __str(actual) + " to be at most " + n); };
  a.include = a.includes = a.contain = a.contains = function (item) {
    var ok;
    if (typeof actual === "string") ok = actual.indexOf(item) >= 0;
    else if (Array.isArray(actual)) ok = actual.some(function (x) { return deep ? __deepEqual(x, item) : x === item; });
    else if (actual && typeof actual === "object") ok = Object.keys(item || {}).every(function (k) { return __deepEqual(actual[k], item[k]); });
    else ok = false;
    return check(ok, __str(actual) + " to include " + __str(item));
  };
  a.property = function (name, value) {
    var has = actual !== null && actual !== undefined && Object.prototype.hasOwnProperty.call(Object(actual), name);
    if (arguments.length > 1) return check(has && __deepEqual(actual[name], value), "property " + name + " to be " + __str(value));
    return check(has, __str(actual) + " to have property " + name);
  };
  a.lengthOf = a.length = function (n) {
    var len = actual === null || actual === undefined ? undefined : actual.length;
    return check(len === n, "length " + len + " to be " + n);
  };
  a.a = a.an = function (type) { return check(__typeOf(actual) === String(type).toLowerCase(), __str(actual) + " to be a " + type); };
  a.match = function (re) { return check(re.test(String(actual)), __str(actual) + " to match " + re); };
  a.status = function (code) { return check(actual && actual.code === code, "status " + (actual && actual.code) + " to be " + code); };
  Object.defineProperty(a, "ok", { get: function () { return check(!!actual, __str(actual) + " to be truthy"); } });
  Object.defineProperty(a, "true", { get: function () { return check(actual === true, __str(actual) + " to be true"); } });
  Object.defineProperty(a, "false", { get: function () { return check(actual === false, __str(actual) + " to be false"); } });
  Object.defineProperty(a, "null", { get: function () { return check(actual === null, __str(actual) + " to be null"); } });
  Object.defineProperty(a, "undefined", { get: function () { return check(actual === undefined, __str(actual) + " to be undefined"); } });
  Object.defineProperty(a, "exist", { get: function () { return check(actual !== null && actual !== undefined, __str(actual) + " to exist"); } });
  Object.defineProperty(a, "empty", {
    get: function () {
      var empty = actual === "" || (Array.isArray(actual) && actual.length === 0) ||
        (actual && typeof actual === "object" && Object.keys(actual).length === 0);
      return check(empty, __str(actual) + " to be empty");
    },
  });
  return a;
}

// ------------------------------------------------------------------ pm

var __current = "";

// ------------------------------------------------------------------ pm.sendRequest

function __toRequest(req) {
  if (typeof req === "string") req = { url: req };
  var headers = [];
  var h = req.header || req.headers;
  if (Array.isArray(h)) {
    h.forEach(function (x) {
      if (x && !x.disabled) headers.push({ name: String(x.key || x.name), value: __replaceIn(String(x.value)), enabled: true });
    });
  } else if (h && typeof h === "object") {
    Object.keys(h).forEach(function (k) { headers.push({ name: k, value: __replaceIn(String(h[k])), enabled: true }); });
  }
  function hasHeader(name) {
    return headers.some(function (x) { return x.name.toLowerCase() === name; });
  }
  var body = null;
  var b = req.body;
  if (typeof b === "string") {
    body = b;
  } else if (b && b.mode === "urlencoded") {
    body = (b.urlencoded || [])
      .filter(function (x) { return !x.disabled; })
      .map(function (x) { return encodeURIComponent(x.key) + "=" + encodeURIComponent(__replaceIn(String(x.value))); })
      .join("&");
    if (!hasHeader("content-type")) headers.push({ name: "Content-Type", value: "application/x-www-form-urlencoded", enabled: true });
  } else if (b && b.mode === "graphql") {
    body = JSON.stringify({ query: b.graphql && b.graphql.query, variables: (b.graphql && b.graphql.variables) || {} });
    if (!hasHeader("content-type")) headers.push({ name: "Content-Type", value: "application/json", enabled: true });
  } else if (b && b.mode === "formdata") {
    throw new Error("pm.sendRequest: form-data bodies are not supported yet");
  } else if (b && b.raw !== undefined) {
    body = String(b.raw);
  }
  if (body !== null) body = __replaceIn(body);
  var url = req.url;
  if (url && typeof url === "object") url = url.raw || url.toString();
  return {
    method: String(req.method || "GET").toUpperCase(),
    url: __replaceIn(String(url)),
    headers: headers,
    body: body,
    timeoutMs: typeof req.timeout === "number" ? req.timeout : null,
  };
}

// ------------------------------------------------------------------ CryptoJS (digests computed in Rust)

function __utf8Bytes(str) {
  var out = [];
  for (var i = 0; i < str.length; i++) {
    var c = str.charCodeAt(i);
    if (c >= 0xd800 && c < 0xdc00 && i + 1 < str.length) {
      c = 0x10000 + ((c - 0xd800) << 10) + (str.charCodeAt(++i) - 0xdc00);
    }
    if (c < 0x80) out.push(c);
    else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
    else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
  }
  return out;
}

function __utf8Decode(bytes) {
  var out = "";
  for (var i = 0; i < bytes.length; i++) {
    var b = bytes[i], c;
    if (b < 0x80) c = b;
    else if (b < 0xe0) c = ((b & 31) << 6) | (bytes[++i] & 63);
    else if (b < 0xf0) c = ((b & 15) << 12) | ((bytes[++i] & 63) << 6) | (bytes[++i] & 63);
    else c = ((b & 7) << 18) | ((bytes[++i] & 63) << 12) | ((bytes[++i] & 63) << 6) | (bytes[++i] & 63);
    if (c > 0xffff) {
      c -= 0x10000;
      out += String.fromCharCode(0xd800 + (c >> 10), 0xdc00 + (c & 1023));
    } else {
      out += String.fromCharCode(c);
    }
  }
  return out;
}

function __toHex(bytes) {
  var out = "";
  for (var i = 0; i < bytes.length; i++) out += (bytes[i] < 16 ? "0" : "") + bytes[i].toString(16);
  return out;
}

function __fromHex(hex) {
  var out = [];
  for (var i = 0; i + 1 < hex.length; i += 2) out.push(parseInt(hex.substr(i, 2), 16));
  return out;
}

function __toBase64(bytes) {
  var out = "";
  for (var i = 0; i < bytes.length; i += 3) {
    var a = bytes[i], b = bytes[i + 1], c = bytes[i + 2];
    var n = (a << 16) | ((b || 0) << 8) | (c || 0);
    out += __b64[(n >> 18) & 63] + __b64[(n >> 12) & 63] + (b === undefined ? "=" : __b64[(n >> 6) & 63]) + (c === undefined ? "=" : __b64[n & 63]);
  }
  return out;
}

function __fromBase64(str) {
  var clean = String(str).replace(/[^A-Za-z0-9+/]/g, ""), out = [], bits = 0, value = 0;
  for (var i = 0; i < clean.length; i++) {
    value = (value << 6) | __b64.indexOf(clean[i]);
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((value >> bits) & 255);
    }
  }
  return out;
}

function __wordArray(bytes) {
  return {
    __bytes: bytes,
    sigBytes: bytes.length,
    toString: function (encoder) { return (encoder || __enc.Hex).stringify(this); },
    concat: function (other) {
      this.__bytes = this.__bytes.concat(__bytesOf(other));
      this.sigBytes = this.__bytes.length;
      return this;
    },
  };
}

var __enc = {
  Hex: { stringify: function (wa) { return __toHex(wa.__bytes); }, parse: function (s) { return __wordArray(__fromHex(String(s))); } },
  Base64: { stringify: function (wa) { return __toBase64(wa.__bytes); }, parse: function (s) { return __wordArray(__fromBase64(s)); } },
  Utf8: { stringify: function (wa) { return __utf8Decode(wa.__bytes); }, parse: function (s) { return __wordArray(__utf8Bytes(String(s))); } },
  Latin1: {
    stringify: function (wa) { return wa.__bytes.map(function (b) { return String.fromCharCode(b); }).join(""); },
    parse: function (s) { var out = []; for (var i = 0; i < s.length; i++) out.push(s.charCodeAt(i) & 255); return __wordArray(out); },
  },
};

function __bytesOf(value) {
  if (value && value.__bytes) return value.__bytes;
  return __utf8Bytes(String(value));
}

function __digest(algorithm, data, key) {
  var hex = __pulpo_crypto(algorithm, key === undefined ? "" : __toHex(__bytesOf(key)), __toHex(__bytesOf(data)));
  if (hex.indexOf("error:") === 0) throw new Error("CryptoJS: " + hex.slice(6));
  return __wordArray(__fromHex(hex));
}

var CryptoJS = {
  MD5: function (data) { return __digest("md5", data); },
  SHA1: function (data) { return __digest("sha1", data); },
  SHA256: function (data) { return __digest("sha256", data); },
  SHA512: function (data) { return __digest("sha512", data); },
  HmacMD5: function (data, key) { return __digest("hmac-md5", data, key); },
  HmacSHA1: function (data, key) { return __digest("hmac-sha1", data, key); },
  HmacSHA256: function (data, key) { return __digest("hmac-sha256", data, key); },
  HmacSHA512: function (data, key) { return __digest("hmac-sha512", data, key); },
  enc: __enc,
  lib: { WordArray: { create: function (bytes) { return __wordArray(Array.prototype.slice.call(bytes || [])); } } },
};

function require(name) {
  if (name === "crypto-js") return CryptoJS;
  throw new Error("require('" + name + "') is not available in Pulpo scripts");
}

var pm = {
  info: { get requestName() { return __input.requestName; }, get eventName() { return __input.phase; } },
  environment: Object.assign({ get name() { return __input.environmentName || ""; } }, __variables),
  collectionVariables: __variables,
  globals: __variables,
  variables: __variables,
  request: __request,
  get response() { return __response(); },
  // Cookies of the jar that match the request URL (read-only, like Postman's pm.cookies).
  cookies: {
    get: function (name) {
      var found = (__input.cookies || []).filter(function (c) { return c[0] === name; })[0];
      return found ? found[1] : undefined;
    },
    has: function (name) { return (__input.cookies || []).some(function (c) { return c[0] === name; }); },
    toObject: function () {
      var out = {};
      (__input.cookies || []).forEach(function (c) { out[c[0]] = c[1]; });
      return out;
    },
  },
  test: function (name, fn) {
    try {
      fn();
      __state.tests.push({ name: String(name), passed: true, error: null });
    } catch (e) {
      __state.tests.push({ name: String(name), passed: false, error: String((e && e.message) || e) });
    }
  },
  expect: __expect,
  sendRequest: function (req, callback) {
    var out = JSON.parse(__pulpo_http(JSON.stringify(__toRequest(req))));
    var err = out.ok ? null : new Error(out.error);
    var res = out.ok ? __responseApi(out.response) : undefined;
    if (typeof callback === "function") {
      callback(err, res);
    } else if (err) {
      throw err;
    }
    return res;
  },
};

var console = {};
["log", "info", "warn", "error", "debug"].forEach(function (level) {
  console[level] = function () {
    var parts = [];
    for (var i = 0; i < arguments.length; i++) parts.push(__str(arguments[i]));
    __state.logs.push((level === "log" ? "" : level + ": ") + (__current ? "[" + __current + "] " : "") + parts.join(" "));
  };
});

var __b64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
function btoa(input) {
  var str = String(input), out = "";
  for (var i = 0; i < str.length; i += 3) {
    var a = str.charCodeAt(i), b = str.charCodeAt(i + 1), c = str.charCodeAt(i + 2);
    var n = (a << 16) | ((b || 0) << 8) | (c || 0);
    out += __b64[(n >> 18) & 63] + __b64[(n >> 12) & 63] + (isNaN(b) ? "=" : __b64[(n >> 6) & 63]) + (isNaN(c) ? "=" : __b64[n & 63]);
  }
  return out;
}
function atob(input) {
  var str = String(input).replace(/=+$/, ""), out = "", bits = 0, value = 0;
  for (var i = 0; i < str.length; i++) {
    var idx = __b64.indexOf(str[i]);
    if (idx < 0) continue;
    value = (value << 6) | idx;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out += String.fromCharCode((value >> bits) & 255);
    }
  }
  return out;
}

function __run(label, source) {
  __current = label;
  try {
    var fn = new Function("pm", "console", "postman", "atob", "btoa", "CryptoJS", "require", source);
    fn(pm, console, pm, atob, btoa, CryptoJS, require);
  } catch (e) {
    __state.errors.push(label + ": " + String((e && e.message) || e));
  }
  __current = "";
}

function __finish() {
  return JSON.stringify({
    changes: __state.changes,
    request: __state.request,
    tests: __state.tests,
    logs: __state.logs,
    errors: __state.errors,
  });
}
