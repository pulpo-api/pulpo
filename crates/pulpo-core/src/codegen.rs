//! Code for a request in other languages and tools: curl, fetch, axios, Python requests,
//! Java HttpClient, Go, C# HttpClient and PowerShell.
//!
//! [`prepare`] turns a request into what goes on the wire (auth applied, path params in the
//! URL, disabled parts left out); each language then prints it. `{{variables}}` that are not
//! resolved stay as they are, so snippets can be shared without secrets.

use crate::collection::{self, CollectionError, RequestEntry};
use crate::environment::{self, SecretStore};
use crate::http::{
    encode_form, encode_query, mime_for, sniff_content_type, ApiKeyLocation, Auth, FormKind,
    HttpRequest,
};
use crate::local;
use crate::variables::{self, Variables, DYNAMIC};
use serde::{Deserialize, Serialize};
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Curl,
    Fetch,
    Axios,
    Python,
    Java,
    Go,
    Csharp,
    Powershell,
}

impl Language {
    pub const ALL: [Language; 8] = [
        Language::Curl,
        Language::Fetch,
        Language::Axios,
        Language::Python,
        Language::Java,
        Language::Go,
        Language::Csharp,
        Language::Powershell,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Language::Curl => "curl",
            Language::Fetch => "fetch",
            Language::Axios => "axios",
            Language::Python => "python",
            Language::Java => "java",
            Language::Go => "go",
            Language::Csharp => "csharp",
            Language::Powershell => "powershell",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Language::Curl => "curl (bash)",
            Language::Fetch => "JavaScript fetch",
            Language::Axios => "JavaScript axios",
            Language::Python => "Python requests",
            Language::Java => "Java HttpClient",
            Language::Go => "Go net/http",
            Language::Csharp => "C# HttpClient",
            Language::Powershell => "PowerShell",
        }
    }

    pub fn parse(text: &str) -> Option<Language> {
        Language::ALL
            .into_iter()
            .find(|l| l.id() == text.trim().to_ascii_lowercase())
    }
}

/// A body as it goes on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    None,
    Text(String),
    Urlencoded(Vec<(String, String)>),
    /// `(name, value, is_file)`; file values are paths.
    Multipart(Vec<(String, String, bool)>),
    File(String),
}

/// A request ready to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Basic auth, printed with each language's own helper.
    pub basic: Option<(String, String)>,
    pub body: Body,
    pub insecure: bool,
    pub follow_redirects: bool,
    pub timeout_ms: Option<u64>,
}

fn has_var(text: &str) -> bool {
    text.contains("{{")
}

/// Encodes for a URL, but keeps `{{variables}}` readable.
fn encode_keeping_vars(text: &str, encode: fn(&str) -> String) -> String {
    if has_var(text) {
        text.to_string()
    } else {
        encode(text)
    }
}

/// What a request sends: auth applied, `:path` params replaced, disabled headers out.
pub fn prepare(request: &HttpRequest) -> Prepared {
    // Path params, encoded unless they are still variables.
    let mut url = request.url.trim().to_string();
    for param in &request.path_params {
        let value = encode_keeping_vars(&param.value, encode_query);
        let pattern = format!(":{}", param.name);
        let mut out = String::new();
        let mut rest = url.as_str();
        while let Some(i) = rest.find(&pattern) {
            let after = &rest[i + pattern.len()..];
            let boundary = after
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
            let segment_start = i > 0 && rest[..i].ends_with('/');
            out.push_str(&rest[..i]);
            if boundary && segment_start {
                out.push_str(&value);
            } else {
                out.push_str(&pattern);
            }
            rest = after;
        }
        out.push_str(rest);
        url = out;
    }

    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|h| h.enabled && !h.name.trim().is_empty())
        .map(|h| (h.name.trim().to_string(), h.value.clone()))
        .collect();
    let has = |headers: &Vec<(String, String)>, name: &str| {
        headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    };

    let mut basic = None;
    match &request.auth {
        Some(Auth::Bearer { token }) if !has(&headers, "authorization") => {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
        }
        Some(Auth::Basic { username, password }) if !has(&headers, "authorization") => {
            basic = Some((username.clone(), password.clone()));
        }
        Some(Auth::ApiKey {
            key,
            value,
            location,
        }) => match location {
            ApiKeyLocation::Header => {
                if !has(&headers, key) {
                    headers.push((key.clone(), value.clone()));
                }
            }
            ApiKeyLocation::Query => {
                let (base, hash) = match url.find('#') {
                    Some(i) => (url[..i].to_string(), url[i..].to_string()),
                    None => (url.clone(), String::new()),
                };
                let sep = if base.contains('?') { '&' } else { '?' };
                url = format!(
                    "{base}{sep}{}={}{hash}",
                    encode_keeping_vars(key, encode_query),
                    encode_keeping_vars(value, encode_query)
                );
            }
        },
        _ => {}
    }

    let body = if let Some(form) = &request.form {
        let fields = form
            .fields
            .iter()
            .filter(|f| f.enabled && !f.name.is_empty());
        match form.kind {
            FormKind::Urlencoded => {
                if !has(&headers, "content-type") {
                    headers.push((
                        "Content-Type".into(),
                        "application/x-www-form-urlencoded".into(),
                    ));
                }
                Body::Urlencoded(fields.map(|f| (f.name.clone(), f.value.clone())).collect())
            }
            FormKind::Multipart => {
                // The boundary goes in Content-Type: the client writes it.
                headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-type"));
                Body::Multipart(
                    fields
                        .map(|f| (f.name.clone(), f.value.clone(), f.file))
                        .collect(),
                )
            }
        }
    } else if let Some(file) = request
        .body_file
        .as_deref()
        .filter(|f| !f.trim().is_empty())
    {
        if !has(&headers, "content-type") {
            headers.push(("Content-Type".into(), mime_for(file).into()));
        }
        Body::File(file.to_string())
    } else {
        match request.body.as_deref().filter(|b| !b.is_empty()) {
            Some(body) => {
                // Pulpo sends the detected type when none is set; so does the code.
                let stopped = request
                    .headers
                    .iter()
                    .any(|h| !h.enabled && h.name.trim().eq_ignore_ascii_case("content-type"));
                if !has(&headers, "content-type") && !stopped {
                    headers.push(("Content-Type".into(), sniff_content_type(body).into()));
                }
                Body::Text(body.to_string())
            }
            None => Body::None,
        }
    };

    Prepared {
        method: request.method.to_string(),
        url,
        headers,
        basic,
        body,
        insecure: request.settings.verify_ssl == Some(false),
        follow_redirects: request.settings.follow_redirects != Some(false),
        timeout_ms: request.settings.timeout_ms,
    }
}

// ---------------------------------------------------------------- literals

/// A double-quoted string literal. JSON escapes are valid in JS, Python, Java, Go and C#.
fn q(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// A single-quoted shell (bash) word.
fn sh(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A single-quoted PowerShell string.
fn ps(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

fn urlencoded(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                encode_keeping_vars(k, encode_form),
                encode_keeping_vars(v, encode_form)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn content_type(p: &Prepared) -> Option<&str> {
    p.headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------- languages

fn curl(p: &Prepared) -> String {
    let mut parts = vec![format!("curl {}", sh(&p.url))];
    if p.method != "GET" {
        parts.push(format!("-X {}", p.method));
    }
    for (name, value) in &p.headers {
        parts.push(format!("-H {}", sh(&format!("{name}: {value}"))));
    }
    if let Some((user, pass)) = &p.basic {
        parts.push(format!("-u {}", sh(&format!("{user}:{pass}"))));
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => parts.push(format!("--data-raw {}", sh(text))),
        Body::Urlencoded(pairs) => {
            for (k, v) in pairs {
                parts.push(format!("--data-urlencode {}", sh(&format!("{k}={v}"))));
            }
        }
        Body::Multipart(fields) => {
            for (name, value, file) in fields {
                let at = if *file { "@" } else { "" };
                parts.push(format!("-F {}", sh(&format!("{name}={at}{value}"))));
            }
        }
        Body::File(path) => parts.push(format!("--data-binary {}", sh(&format!("@{path}")))),
    }
    if p.follow_redirects {
        parts.push("-L".into());
    }
    if p.insecure {
        parts.push("-k".into());
    }
    if let Some(ms) = p.timeout_ms {
        parts.push(format!("--max-time {}", ms.div_ceil(1000)));
    }
    parts.join(" \\\n  ")
}

/// Shared by fetch and axios: the FormData / URLSearchParams / file body, and imports.
fn js_body(p: &Prepared, stream_files: bool) -> (Vec<&'static str>, String, Option<String>) {
    let mut imports = Vec::new();
    let mut setup = String::new();
    let body = match &p.body {
        Body::None => None,
        Body::Text(text) => Some(q(text)),
        Body::Urlencoded(pairs) => Some(format!(
            "new URLSearchParams([{}])",
            pairs
                .iter()
                .map(|(k, v)| format!("[{}, {}]", q(k), q(v)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Body::Multipart(fields) => {
            setup.push_str("const form = new FormData();\n");
            for (name, value, file) in fields {
                if *file {
                    if !imports.contains(&"import fs from \"node:fs\";") {
                        imports.push("import fs from \"node:fs\";");
                    }
                    let _ = writeln!(
                        setup,
                        "form.append({}, await fs.openAsBlob({}), {});",
                        q(name),
                        q(value),
                        q(&file_name(value))
                    );
                } else {
                    let _ = writeln!(setup, "form.append({}, {});", q(name), q(value));
                }
            }
            setup.push('\n');
            Some("form".into())
        }
        Body::File(path) => {
            imports.push("import fs from \"node:fs\";");
            Some(if stream_files {
                format!("fs.createReadStream({})", q(path))
            } else {
                format!("await fs.openAsBlob({})", q(path))
            })
        }
    };
    (imports, setup, body)
}

fn js_headers(p: &Prepared, basic_inline: bool) -> Vec<String> {
    let mut lines: Vec<String> = p
        .headers
        .iter()
        .map(|(n, v)| format!("{}: {}", q(n), q(v)))
        .collect();
    if basic_inline {
        if let Some((user, pass)) = &p.basic {
            lines.push(format!(
                "\"Authorization\": \"Basic \" + btoa({})",
                q(&format!("{user}:{pass}"))
            ));
        }
    }
    lines
}

fn fetch(p: &Prepared) -> String {
    let (imports, setup, body) = js_body(p, false);
    let mut out = String::new();
    for line in &imports {
        let _ = writeln!(out, "{line}");
    }
    if !imports.is_empty() {
        out.push('\n');
    }
    if p.insecure {
        out.push_str("// SSL verification is off in Pulpo; in Node set NODE_TLS_REJECT_UNAUTHORIZED=0 (never in production).\n");
    }
    out.push_str(&setup);
    let _ = writeln!(out, "const response = await fetch({}, {{", q(&p.url));
    let _ = writeln!(out, "  method: {},", q(&p.method));
    let headers = js_headers(p, true);
    if !headers.is_empty() {
        out.push_str("  headers: {\n");
        for h in headers {
            let _ = writeln!(out, "    {h},");
        }
        out.push_str("  },\n");
    }
    if let Some(body) = body {
        let _ = writeln!(out, "  body: {body},");
    }
    if !p.follow_redirects {
        out.push_str("  redirect: \"manual\",\n");
    }
    if let Some(ms) = p.timeout_ms {
        let _ = writeln!(out, "  signal: AbortSignal.timeout({ms}),");
    }
    out.push_str("});\n\nconsole.log(response.status, await response.text());");
    out
}

fn axios(p: &Prepared) -> String {
    let (mut imports, setup, body) = js_body(p, true);
    imports.insert(0, "import axios from \"axios\";");
    if p.insecure {
        imports.push("import https from \"node:https\";");
    }
    let mut out = String::new();
    for line in &imports {
        let _ = writeln!(out, "{line}");
    }
    out.push('\n');
    out.push_str(&setup);
    out.push_str("const response = await axios({\n");
    let _ = writeln!(out, "  method: {},", q(&p.method.to_ascii_lowercase()));
    let _ = writeln!(out, "  url: {},", q(&p.url));
    let headers = js_headers(p, false);
    if !headers.is_empty() {
        out.push_str("  headers: {\n");
        for h in headers {
            let _ = writeln!(out, "    {h},");
        }
        out.push_str("  },\n");
    }
    if let Some((user, pass)) = &p.basic {
        let _ = writeln!(
            out,
            "  auth: {{ username: {}, password: {} }},",
            q(user),
            q(pass)
        );
    }
    if let Some(body) = body {
        let _ = writeln!(out, "  data: {body},");
    }
    if !p.follow_redirects {
        out.push_str("  maxRedirects: 0,\n");
    }
    if let Some(ms) = p.timeout_ms {
        let _ = writeln!(out, "  timeout: {ms},");
    }
    if p.insecure {
        out.push_str("  httpsAgent: new https.Agent({ rejectUnauthorized: false }),\n");
    }
    out.push_str(
        "  validateStatus: () => true,\n});\n\nconsole.log(response.status, response.data);",
    );
    out
}

fn python(p: &Prepared) -> String {
    let mut args = vec![q(&p.url)];
    if !p.headers.is_empty() {
        let mut h = String::from("headers={\n");
        for (n, v) in &p.headers {
            let _ = writeln!(h, "        {}: {},", q(n), q(v));
        }
        h.push_str("    }");
        args.push(h);
    }
    if let Some((user, pass)) = &p.basic {
        args.push(format!("auth=({}, {})", q(user), q(pass)));
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => args.push(format!("data={}.encode(\"utf-8\")", q(text))),
        Body::Urlencoded(pairs) => args.push(format!(
            "data=[{}]",
            pairs
                .iter()
                .map(|(k, v)| format!("({}, {})", q(k), q(v)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Body::Multipart(fields) => {
            let texts: Vec<_> = fields
                .iter()
                .filter(|f| !f.2)
                .map(|(n, v, _)| format!("{}: {}", q(n), q(v)))
                .collect();
            let files: Vec<_> = fields
                .iter()
                .filter(|f| f.2)
                .map(|(n, v, _)| format!("{}: open({}, \"rb\")", q(n), q(v)))
                .collect();
            if !texts.is_empty() {
                args.push(format!("data={{{}}}", texts.join(", ")));
            }
            if !files.is_empty() {
                args.push(format!("files={{{}}}", files.join(", ")));
            }
        }
        Body::File(path) => args.push(format!("data=open({}, \"rb\")", q(path))),
    }
    if p.insecure {
        args.push("verify=False".into());
    }
    if !p.follow_redirects {
        args.push("allow_redirects=False".into());
    }
    if let Some(ms) = p.timeout_ms {
        args.push(format!("timeout={}", ms as f64 / 1000.0));
    }
    let call = match p.method.as_str() {
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" => {
            format!("requests.{}(", p.method.to_ascii_lowercase())
        }
        other => format!("requests.request({}, ", q(other)),
    };
    format!(
        "import requests\n\nresponse = {call}\n    {},\n)\n\nprint(response.status_code, response.text)",
        args.join(",\n    ")
    )
}

fn java(p: &Prepared) -> String {
    let mut imports = vec![
        "java.net.URI",
        "java.net.http.HttpClient",
        "java.net.http.HttpRequest",
        "java.net.http.HttpResponse",
    ];
    let mut notes = Vec::new();
    let body = match &p.body {
        Body::None => "HttpRequest.BodyPublishers.noBody()".to_string(),
        Body::Text(text) => format!("HttpRequest.BodyPublishers.ofString({})", q(text)),
        Body::Urlencoded(pairs) => format!(
            "HttpRequest.BodyPublishers.ofString({})",
            q(&urlencoded(pairs))
        ),
        Body::File(path) => {
            imports.push("java.nio.file.Path");
            format!("HttpRequest.BodyPublishers.ofFile(Path.of({}))", q(path))
        }
        Body::Multipart(_) => {
            notes.push("// Java's HttpClient has no multipart builder: use a library such as Apache HttpClient 5 (MultipartEntityBuilder).");
            "HttpRequest.BodyPublishers.noBody()".to_string()
        }
    };
    if p.basic.is_some() {
        imports.push("java.nio.charset.StandardCharsets");
        imports.push("java.util.Base64");
    }
    if p.timeout_ms.is_some() {
        imports.push("java.time.Duration");
    }
    if p.insecure {
        notes.push("// SSL verification is off in Pulpo: Java needs a custom SSLContext for that (not shown).");
    }
    let mut out = String::new();
    for i in &imports {
        let _ = writeln!(out, "import {i};");
    }
    out.push_str(
        "\npublic class Main {\n    public static void main(String[] args) throws Exception {\n",
    );
    for n in notes {
        let _ = writeln!(out, "        {n}");
    }
    let redirect = if p.follow_redirects {
        "NORMAL"
    } else {
        "NEVER"
    };
    let _ = writeln!(
        out,
        "        HttpClient client = HttpClient.newBuilder()\n            .followRedirects(HttpClient.Redirect.{redirect})\n            .build();"
    );
    let _ = writeln!(
        out,
        "        HttpRequest request = HttpRequest.newBuilder()\n            .uri(URI.create({}))",
        q(&p.url)
    );
    for (n, v) in &p.headers {
        let _ = writeln!(out, "            .header({}, {})", q(n), q(v));
    }
    if let Some((user, pass)) = &p.basic {
        let _ = writeln!(
            out,
            "            .header(\"Authorization\", \"Basic \" + Base64.getEncoder().encodeToString({}.getBytes(StandardCharsets.UTF_8)))",
            q(&format!("{user}:{pass}"))
        );
    }
    let _ = writeln!(out, "            .method({}, {body})", q(&p.method));
    if let Some(ms) = p.timeout_ms {
        let _ = writeln!(out, "            .timeout(Duration.ofMillis({ms}))");
    }
    out.push_str("            .build();\n");
    out.push_str("        HttpResponse<String> response = client.send(request, HttpResponse.BodyHandlers.ofString());\n");
    out.push_str(
        "        System.out.println(response.statusCode() + \" \" + response.body());\n    }\n}",
    );
    out
}

fn go(p: &Prepared) -> String {
    let mut imports = vec!["fmt", "io", "net/http"];
    let mut setup = String::new();
    let body = match &p.body {
        Body::None => "nil".to_string(),
        Body::Text(text) => {
            imports.push("strings");
            format!("strings.NewReader({})", q(text))
        }
        Body::Urlencoded(pairs) => {
            imports.push("strings");
            format!("strings.NewReader({})", q(&urlencoded(pairs)))
        }
        Body::File(path) => {
            imports.push("os");
            let _ = writeln!(setup, "\tfile, err := os.Open({})\n\tif err != nil {{\n\t\tpanic(err)\n\t}}\n\tdefer file.Close()", q(path));
            "file".to_string()
        }
        Body::Multipart(fields) => {
            imports.extend(["bytes", "mime/multipart"]);
            setup.push_str("\tvar buf bytes.Buffer\n\tform := multipart.NewWriter(&buf)\n");
            for (name, value, file) in fields {
                if *file {
                    if !imports.contains(&"os") {
                        imports.push("os");
                    }
                    let _ = writeln!(
                        setup,
                        "\tif data, err := os.ReadFile({}); err != nil {{\n\t\tpanic(err)\n\t}} else {{\n\t\tpart, _ := form.CreateFormFile({}, {})\n\t\tpart.Write(data)\n\t}}",
                        q(value),
                        q(name),
                        q(&file_name(value))
                    );
                } else {
                    let _ = writeln!(setup, "\tform.WriteField({}, {})", q(name), q(value));
                }
            }
            setup.push_str("\tform.Close()\n");
            "&buf".to_string()
        }
    };
    if p.insecure {
        imports.push("crypto/tls");
    }
    if p.timeout_ms.is_some() {
        imports.push("time");
    }
    imports.sort();
    imports.dedup();
    let mut out = String::from("package main\n\nimport (\n");
    for i in &imports {
        let _ = writeln!(out, "\t{}", q(i));
    }
    out.push_str(")\n\nfunc main() {\n");
    out.push_str(&setup);
    let _ = writeln!(
        out,
        "\treq, err := http.NewRequest({}, {}, {body})\n\tif err != nil {{\n\t\tpanic(err)\n\t}}",
        q(&p.method),
        q(&p.url)
    );
    for (n, v) in &p.headers {
        let _ = writeln!(out, "\treq.Header.Add({}, {})", q(n), q(v));
    }
    if matches!(p.body, Body::Multipart(_)) {
        out.push_str("\treq.Header.Set(\"Content-Type\", form.FormDataContentType())\n");
    }
    if let Some((user, pass)) = &p.basic {
        let _ = writeln!(out, "\treq.SetBasicAuth({}, {})", q(user), q(pass));
    }
    out.push_str("\tclient := &http.Client{");
    let mut fields = Vec::new();
    if let Some(ms) = p.timeout_ms {
        fields.push(format!("\n\t\tTimeout: {ms} * time.Millisecond,"));
    }
    if p.insecure {
        fields.push("\n\t\tTransport: &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}},".into());
    }
    if !p.follow_redirects {
        fields.push("\n\t\tCheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },".into());
    }
    if fields.is_empty() {
        out.push_str("}\n");
    } else {
        let _ = writeln!(out, "{}\n\t}}", fields.concat());
    }
    out.push_str("\tresp, err := client.Do(req)\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tdefer resp.Body.Close()\n");
    out.push_str("\tdata, _ := io.ReadAll(resp.Body)\n\tfmt.Println(resp.Status, string(data))\n}");
    out
}

fn csharp(p: &Prepared) -> String {
    let mut out = String::from(
        "using System.Net.Http;\nusing System.Net.Http.Headers;\nusing System.Text;\n\n",
    );
    let mut handler = Vec::new();
    if p.insecure {
        handler.push("ServerCertificateCustomValidationCallback = HttpClientHandler.DangerousAcceptAnyServerCertificateValidator");
    }
    if !p.follow_redirects {
        handler.push("AllowAutoRedirect = false");
    }
    if handler.is_empty() {
        out.push_str("using var client = new HttpClient();\n");
    } else {
        let _ = writeln!(
            out,
            "using var client = new HttpClient(new HttpClientHandler {{ {} }});",
            handler.join(", ")
        );
    }
    if let Some(ms) = p.timeout_ms {
        let _ = writeln!(out, "client.Timeout = TimeSpan.FromMilliseconds({ms});");
    }
    let _ = writeln!(
        out,
        "var request = new HttpRequestMessage(new HttpMethod({}), {});",
        q(&p.method),
        q(&p.url)
    );
    let ctype = content_type(p).map(str::to_string);
    for (n, v) in p
        .headers
        .iter()
        .filter(|(n, _)| !n.eq_ignore_ascii_case("content-type"))
    {
        let _ = writeln!(
            out,
            "request.Headers.TryAddWithoutValidation({}, {});",
            q(n),
            q(v)
        );
    }
    if let Some((user, pass)) = &p.basic {
        let _ = writeln!(
            out,
            "request.Headers.Authorization = new AuthenticationHeaderValue(\"Basic\", Convert.ToBase64String(Encoding.UTF8.GetBytes({})));",
            q(&format!("{user}:{pass}"))
        );
    }
    let content = match &p.body {
        Body::None => None,
        Body::Text(text) => Some(format!("new StringContent({}, Encoding.UTF8)", q(text))),
        Body::Urlencoded(pairs) => Some(format!(
            "new FormUrlEncodedContent(new[] {{ {} }})",
            pairs
                .iter()
                .map(|(k, v)| format!("KeyValuePair.Create({}, {})", q(k), q(v)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Body::File(path) => Some(format!("new StreamContent(File.OpenRead({}))", q(path))),
        Body::Multipart(fields) => {
            out.push_str("var form = new MultipartFormDataContent();\n");
            for (name, value, file) in fields {
                if *file {
                    let _ = writeln!(
                        out,
                        "form.Add(new StreamContent(File.OpenRead({})), {}, {});",
                        q(value),
                        q(name),
                        q(&file_name(value))
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "form.Add(new StringContent({}), {});",
                        q(value),
                        q(name)
                    );
                }
            }
            Some("form".into())
        }
    };
    if let Some(content) = content {
        let _ = writeln!(out, "request.Content = {content};");
        if let (Some(ct), false) = (&ctype, matches!(p.body, Body::Multipart(_))) {
            let _ = writeln!(
                out,
                "request.Content.Headers.ContentType = MediaTypeHeaderValue.Parse({});",
                q(ct)
            );
        }
    }
    out.push_str("var response = await client.SendAsync(request);\n");
    out.push_str("Console.WriteLine($\"{(int)response.StatusCode} {await response.Content.ReadAsStringAsync()}\");");
    out
}

fn powershell(p: &Prepared) -> String {
    let mut out = String::new();
    let ctype = content_type(p).map(str::to_string);
    let headers: Vec<_> = p
        .headers
        .iter()
        .filter(|(n, _)| !n.eq_ignore_ascii_case("content-type"))
        .collect();
    let mut args = vec![
        format!("-Uri {}", ps(&p.url)),
        format!("-Method {}", p.method),
    ];
    if !headers.is_empty() || p.basic.is_some() {
        out.push_str("$headers = @{\n");
        for (n, v) in &headers {
            let _ = writeln!(out, "    {} = {}", ps(n), ps(v));
        }
        if let Some((user, pass)) = &p.basic {
            let _ = writeln!(
                out,
                "    'Authorization' = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes({}))",
                ps(&format!("{user}:{pass}"))
            );
        }
        out.push_str("}\n");
        args.push("-Headers $headers".into());
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => {
            if text.contains('\n') && !text.contains("\n'@") {
                let _ = writeln!(out, "$body = @'\n{text}\n'@");
            } else {
                let _ = writeln!(out, "$body = {}", ps(text));
            }
            args.push("-Body $body".into());
        }
        Body::Urlencoded(pairs) => args.push(format!("-Body {}", ps(&urlencoded(pairs)))),
        Body::File(path) => args.push(format!("-InFile {}", ps(path))),
        Body::Multipart(fields) => {
            out.push_str("$form = @{\n");
            for (name, value, file) in fields {
                if *file {
                    let _ = writeln!(out, "    {} = Get-Item {}", ps(name), ps(value));
                } else {
                    let _ = writeln!(out, "    {} = {}", ps(name), ps(value));
                }
            }
            out.push_str("}\n");
            args.push("-Form $form".into());
        }
    }
    if let (Some(ct), false) = (&ctype, matches!(p.body, Body::Multipart(_))) {
        args.push(format!("-ContentType {}", ps(ct)));
    }
    if p.insecure {
        args.push("-SkipCertificateCheck".into());
    }
    if !p.follow_redirects {
        args.push("-MaximumRedirection 0".into());
    }
    if let Some(ms) = p.timeout_ms {
        args.push(format!("-TimeoutSec {}", ms.div_ceil(1000)));
    }
    args.push("-SkipHttpErrorCheck".into());
    let _ = writeln!(
        out,
        "# PowerShell 7+\n$response = Invoke-WebRequest `\n    {}",
        args.join(" `\n    ")
    );
    out.push_str("$response.StatusCode\n$response.Content");
    out
}

/// Prints a prepared request in a language.
pub fn render(language: Language, prepared: &Prepared) -> String {
    match language {
        Language::Curl => curl(prepared),
        Language::Fetch => fetch(prepared),
        Language::Axios => axios(prepared),
        Language::Python => python(prepared),
        Language::Java => java(prepared),
        Language::Go => go(prepared),
        Language::Csharp => csharp(prepared),
        Language::Powershell => powershell(prepared),
    }
}

pub fn generate(language: Language, request: &HttpRequest) -> String {
    render(language, &prepare(request))
}

/// How variables are written in the code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeOptions {
    /// Replace `{{variables}}` with the environment's values.
    pub resolve: bool,
    /// Also secrets and values set by scripts (usually tokens). Off by default.
    pub include_secrets: bool,
}

/// Code for a request of a workspace, with what it inherits from its folders (auth,
/// headers, settings). Unresolved, secret and dynamic variables stay as `{{name}}`.
pub fn for_entry(
    root: &std::path::Path,
    entry: &RequestEntry,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    language: Language,
    options: CodeOptions,
) -> Result<String, CollectionError> {
    let mut request = entry.request.clone();
    request.auth = Some(collection::effective_auth(root, entry)?.0);
    request.settings = collection::effective_settings(root, entry)?;
    request.headers = collection::effective_headers(root, entry)?;

    if options.resolve {
        let mut vars = Variables::new();
        let keep = |vars: &mut Variables, name: &str| {
            vars.insert(name.to_string(), format!("{{{{{name}}}}}"));
        };
        if let Some(id) = environment_id {
            let env = environment::load_environment(root, id, store, false)?;
            let values = environment::variables_for(root, id, store)?;
            for variable in env.variables {
                match values.get(&variable.name) {
                    Some(value) if !variable.secret || options.include_secrets => {
                        vars.insert(variable.name.clone(), value.clone());
                    }
                    _ => keep(&mut vars, &variable.name),
                }
            }
        }
        for (name, value) in local::load_local(root, environment_id) {
            if options.include_secrets {
                vars.insert(name, value);
            } else {
                keep(&mut vars, &name);
            }
        }
        // Dynamic values change on every send: keep them as variables.
        for name in DYNAMIC {
            keep(&mut vars, name);
        }
        request = match variables::resolve_request(&request, &vars) {
            Ok(resolved) => resolved,
            Err(missing) => {
                for name in missing {
                    keep(&mut vars, &name);
                }
                variables::resolve_request(&request, &vars).unwrap_or(request)
            }
        };
    }
    Ok(generate(language, &request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Form, FormField, Header, Param, Settings};
    use crate::HttpMethod;

    fn post() -> HttpRequest {
        let mut r = HttpRequest::new(HttpMethod::Post, "{{base}}/orders/:id");
        r.path_params = vec![Param::new("id", "a b")];
        r.headers = vec![
            Header::new("Content-Type", "application/json"),
            Header {
                enabled: false,
                ..Header::new("X-Off", "1")
            },
        ];
        r.body = Some("{\"note\": \"it's \\\"ok\\\"\"}".into());
        r.auth = Some(Auth::Bearer {
            token: "{{token}}".into(),
        });
        r
    }

    #[test]
    fn prepares_what_goes_on_the_wire() {
        let p = prepare(&post());
        assert_eq!(p.url, "{{base}}/orders/a%20b");
        assert_eq!(
            p.headers,
            vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Authorization".to_string(), "Bearer {{token}}".to_string()),
            ],
            "disabled headers are left out, auth is added"
        );

        let mut r = HttpRequest::new(HttpMethod::Get, "https://x.test/a?b=1");
        r.auth = Some(Auth::ApiKey {
            key: "api key".into(),
            value: "{{key}}".into(),
            location: ApiKeyLocation::Query,
        });
        assert_eq!(prepare(&r).url, "https://x.test/a?b=1&api%20key={{key}}");

        r.path_params = vec![Param::new("id", "{{id}}")];
        r.url = "https://x.test/:id/:identity".into();
        r.auth = None;
        assert_eq!(
            prepare(&r).url,
            "https://x.test/{{id}}/:identity",
            "only whole segments"
        );
    }

    #[test]
    fn curl_quotes_for_bash() {
        let code = generate(Language::Curl, &post());
        assert_eq!(
            code,
            "curl '{{base}}/orders/a%20b' \\\n  -X POST \\\n  -H 'Content-Type: application/json' \\\n  -H 'Authorization: Bearer {{token}}' \\\n  --data-raw '{\"note\": \"it'\\''s \\\"ok\\\"\"}' \\\n  -L"
        );
    }

    #[test]
    fn every_language_has_the_essentials() {
        let request = post();
        for language in Language::ALL {
            let code = generate(language, &request);
            assert!(
                code.contains("{{base}}/orders/a%20b"),
                "{language:?}: url\n{code}"
            );
            assert!(
                code.contains("Bearer {{token}}"),
                "{language:?}: auth\n{code}"
            );
            assert!(
                code.contains("application/json"),
                "{language:?}: content type\n{code}"
            );
            assert!(code.contains("note"), "{language:?}: body\n{code}");
            assert!(
                !code.contains("X-Off"),
                "{language:?}: disabled header\n{code}"
            );
        }
        // Strings are valid literals: the quote inside the body is escaped.
        assert!(generate(Language::Python, &request)
            .contains(r#"data="{\"note\": \"it's \\\"ok\\\"\"}".encode("utf-8")"#));
        assert!(generate(Language::Powershell, &request)
            .contains("$body = '{\"note\": \"it''s \\\"ok\\\"\"}'"));
    }

    #[test]
    fn forms_files_basic_auth_and_settings() {
        let mut r = HttpRequest::new(HttpMethod::Put, "https://x.test/up");
        r.headers = vec![Header::new("Content-Type", "multipart/form-data")];
        r.form = Some(Form {
            kind: FormKind::Multipart,
            fields: vec![
                FormField::text("title", "Hi"),
                FormField::file("doc", "files/cv.pdf"),
            ],
        });
        r.auth = Some(Auth::Basic {
            username: "ana".into(),
            password: "{{pw}}".into(),
        });
        r.settings = Settings {
            verify_ssl: Some(false),
            follow_redirects: Some(false),
            timeout_ms: Some(2500),
            proxy: None,
        };
        let curl = generate(Language::Curl, &r);
        assert!(
            curl.contains("-F 'title=Hi'") && curl.contains("-F 'doc=@files/cv.pdf'"),
            "{curl}"
        );
        assert!(
            curl.contains("-u 'ana:{{pw}}'")
                && curl.contains("-k")
                && curl.contains("--max-time 3"),
            "{curl}"
        );
        assert!(
            !curl.contains("-L") && !curl.contains("multipart/form-data'"),
            "{curl}"
        );

        let py = generate(Language::Python, &r);
        assert!(
            py.contains("files={\"doc\": open(\"files/cv.pdf\", \"rb\")}")
                && py.contains("auth=(\"ana\", \"{{pw}}\")"),
            "{py}"
        );
        assert!(
            py.contains("verify=False")
                && py.contains("allow_redirects=False")
                && py.contains("timeout=2.5"),
            "{py}"
        );
        assert!(generate(Language::Go, &r).contains("form.FormDataContentType()"));
        assert!(generate(Language::Csharp, &r).contains("MultipartFormDataContent"));
        assert!(generate(Language::Powershell, &r).contains("'doc' = Get-Item 'files/cv.pdf'"));
        assert!(generate(Language::Java, &r).contains("no multipart builder"));

        r.form = Some(Form {
            kind: FormKind::Urlencoded,
            fields: vec![FormField::text("q", "a b&c")],
        });
        assert!(generate(Language::Curl, &r).contains("--data-urlencode 'q=a b&c'"));
        assert!(generate(Language::Java, &r).contains("ofString(\"q=a+b%26c\")"));

        r.form = None;
        r.body_file = Some("data/img.png".into());
        r.headers.clear();
        let curl = generate(Language::Curl, &r);
        assert!(
            curl.contains("--data-binary '@data/img.png'")
                && curl.contains("Content-Type: image/png"),
            "{curl}"
        );
    }

    #[test]
    fn resolves_variables_but_not_secrets_unless_asked() {
        use crate::collection::{create_request, init_workspace, save_request};
        use crate::environment::{create_environment, save_environment, MemoryStore};

        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let store = MemoryStore::default();
        let mut env = create_environment(ws.path(), "Local", &store).unwrap();
        env.variables = vec![
            crate::environment::Variable {
                name: "base".into(),
                value: "https://api.test".into(),
                secret: false,
                has_value: true,
            },
            crate::environment::Variable {
                name: "token".into(),
                value: "s3cret".into(),
                secret: true,
                has_value: true,
            },
        ];
        save_environment(ws.path(), &env, &store).unwrap();
        local::apply_changes(
            ws.path(),
            Some(&env.id),
            &[("orderId".to_string(), Some("42".to_string()))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        let mut entry = create_request(ws.path(), "", "Order").unwrap();
        entry.request.url = "{{base}}/orders/{{orderId}}?n={{$guid}}&x={{nope}}".into();
        entry.request.auth = Some(Auth::Bearer {
            token: "{{token}}".into(),
        });
        save_request(ws.path(), &entry).unwrap();

        let code = |options| {
            for_entry(
                ws.path(),
                &entry,
                Some(&env.id),
                &store,
                Language::Curl,
                options,
            )
            .unwrap()
        };
        let raw = code(CodeOptions::default());
        assert!(raw.contains("'{{base}}/orders/{{orderId}}"), "{raw}");

        let resolved = code(CodeOptions {
            resolve: true,
            include_secrets: false,
        });
        assert!(
            resolved.contains("'https://api.test/orders/{{orderId}}?n={{$guid}}&x={{nope}}'"),
            "{resolved}"
        );
        assert!(
            resolved.contains("Bearer {{token}}") && !resolved.contains("s3cret"),
            "{resolved}"
        );

        let with_secrets = code(CodeOptions {
            resolve: true,
            include_secrets: true,
        });
        assert!(
            with_secrets.contains("https://api.test/orders/42?")
                && with_secrets.contains("Bearer s3cret"),
            "{with_secrets}"
        );
    }

    #[test]
    fn parses_language_ids() {
        assert_eq!(Language::parse("Python"), Some(Language::Python));
        assert_eq!(Language::parse("cobol"), None);
        assert_eq!(Language::ALL.len(), 8);
    }
}
