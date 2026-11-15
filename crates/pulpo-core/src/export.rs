//! Exporters: Postman v2.1, OpenAPI 3 and `.http` files. Exporting is the promise that
//! nobody is tied to Pulpo.

use crate::collection::{self, CollectionError, RequestEntry, TreeNode};
use crate::examples;
use crate::http::{ApiKeyLocation, Auth, FormKind, Header, HttpRequest};
use crate::oauth::{ClientAuth, Grant};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    Postman,
    OpenApi,
    Http,
    /// Documentation: one self-contained HTML page.
    Html,
}

impl ExportFormat {
    pub fn parse(name: &str) -> Option<ExportFormat> {
        match name.to_ascii_lowercase().as_str() {
            "postman" => Some(ExportFormat::Postman),
            "openapi" | "oas" => Some(ExportFormat::OpenApi),
            "http" | "rest" => Some(ExportFormat::Http),
            "html" | "docs" => Some(ExportFormat::Html),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Postman => "postman_collection.json",
            ExportFormat::OpenApi => "openapi.yaml",
            ExportFormat::Http => "http",
            ExportFormat::Html => "html",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exported {
    pub text: String,
    /// A file name for it: `<collection>.<extension>`.
    pub file_name: String,
    pub requests: usize,
    pub warnings: Vec<String>,
}

/// What a folder carries down to the requests inside.
#[derive(Clone, Default)]
pub(crate) struct Inherited {
    auth: Option<Auth>,
    headers: Vec<Header>,
    path: Vec<String>,
}

impl Inherited {
    pub(crate) fn enter(&self, name: &str, auth: &Option<Auth>, headers: &[Header]) -> Inherited {
        let mut next = self.clone();
        if auth.is_some() {
            next.auth = auth.clone();
        }
        for h in headers {
            next.headers
                .retain(|existing| !existing.name.eq_ignore_ascii_case(&h.name));
            next.headers.push(h.clone());
        }
        next.path.push(name.to_string());
        next
    }

    /// The request's auth and headers once the folders' are applied.
    pub(crate) fn apply(&self, request: &HttpRequest) -> (Option<Auth>, Vec<Header>) {
        let auth = request.auth.clone().or_else(|| self.auth.clone());
        let mut headers: Vec<Header> = self
            .headers
            .iter()
            .filter(|h| {
                !request
                    .headers
                    .iter()
                    .any(|own| own.name.eq_ignore_ascii_case(&h.name))
            })
            .cloned()
            .collect();
        headers.extend(request.headers.iter().cloned());
        (auth, headers)
    }
}

fn find_folder<'a>(nodes: &'a [TreeNode], id: &str) -> Option<&'a TreeNode> {
    nodes.iter().find_map(|node| match node {
        TreeNode::Folder {
            id: folder_id,
            children,
            ..
        } => {
            if folder_id == id {
                Some(node)
            } else {
                find_folder(children, id)
            }
        }
        TreeNode::Request(_) => None,
    })
}

fn content_type(headers: &[Header]) -> String {
    headers
        .iter()
        .find(|h| h.enabled && h.name.eq_ignore_ascii_case("content-type"))
        .map(|h| {
            h.value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

fn slug(name: &str) -> String {
    let s = collection::slugify(name);
    if s.is_empty() {
        "collection".into()
    } else {
        s
    }
}

/// Exports `folder_id` ("" = the whole workspace).
pub fn export(
    root: &Path,
    folder_id: &str,
    format: ExportFormat,
) -> Result<Exported, CollectionError> {
    let workspace = collection::load_workspace(root)?;
    let (name, nodes, inherited, scripts) = if folder_id.is_empty() {
        (
            workspace.name.clone(),
            workspace.tree.clone(),
            Inherited {
                auth: None,
                headers: Vec::new(),
                path: Vec::new(),
            },
            collection::folder_scripts(root, "")?,
        )
    } else {
        let Some(TreeNode::Folder { name, children, .. }) = find_folder(&workspace.tree, folder_id)
        else {
            return Err(CollectionError::InvalidPath(format!(
                "no folder {folder_id}"
            )));
        };
        // Auth and headers of the folders above (and the workspace) still apply.
        let mut inherited = Inherited::default();
        for ancestor in collection::ancestor_folders(&format!("{folder_id}/x")) {
            let meta = collection::folder_meta(root, &ancestor)?;
            inherited = inherited.enter("", &meta.auth, &meta.headers);
        }
        inherited.path.clear();
        (
            name.clone(),
            children.clone(),
            inherited,
            collection::folder_scripts(root, folder_id)?,
        )
    };
    let (root_auth, root_headers) = if folder_id.is_empty() {
        (workspace.auth.clone(), workspace.headers.clone())
    } else {
        (None, Vec::new())
    };
    let mut warnings = Vec::new();
    let mut count = 0;
    let text = match format {
        ExportFormat::Postman => {
            let mut root_inherited = inherited.clone();
            root_inherited.headers.extend(root_headers);
            let items = postman_items(root, &nodes, &root_inherited, &mut count, &mut warnings)?;
            let mut collection = json!({
                "info": {
                    "_postman_id": uuid::Uuid::new_v4().to_string(),
                    "name": name,
                    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
                },
                "item": items,
            });
            let auth = if folder_id.is_empty() {
                root_auth
            } else {
                inherited.auth.clone()
            };
            if let Some(auth) = auth.as_ref().and_then(|a| postman_auth(a, &mut warnings)) {
                collection["auth"] = auth;
            }
            let events = postman_events(&scripts.pre, &scripts.test);
            if !events.is_empty() {
                collection["event"] = Value::Array(events);
            }
            serde_json::to_string_pretty(&collection).unwrap_or_default() + "\n"
        }
        ExportFormat::OpenApi => {
            let mut top = inherited.clone();
            top.auth = root_auth.or(top.auth);
            top.headers.extend(root_headers);
            openapi(root, &name, &nodes, &top, &mut count, &mut warnings)?
        }
        ExportFormat::Html => {
            let mut top = inherited.clone();
            top.auth = root_auth.or(top.auth);
            top.headers.extend(root_headers);
            crate::docs::render(root, &name, &nodes, &top, &mut count)
        }
        ExportFormat::Http => {
            let mut top = inherited.clone();
            top.auth = root_auth.or(top.auth);
            top.headers.extend(root_headers);
            let mut out = format!(
                "# {name}, exported from Pulpo. Variables ({{{{name}}}}) come from your environments.\n"
            );
            http_file(&nodes, &top, &mut out, &mut count, &mut warnings);
            out
        }
    };
    warnings.sort();
    warnings.dedup();
    Ok(Exported {
        text,
        file_name: format!("{}.{}", slug(&name), format.extension()),
        requests: count,
        warnings,
    })
}

/// An environment as a Postman environment. Secret values are left empty: they live in
/// the keychain and should not travel in a file.
pub fn export_environment(
    root: &Path,
    environment_id: &str,
    store: &dyn crate::environment::SecretStore,
) -> Result<Exported, CollectionError> {
    let env = crate::environment::load_environment(root, environment_id, store, false)?;
    let secrets = env.variables.iter().filter(|v| v.secret).count();
    let document = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "name": env.name,
        "values": env.variables.iter().map(|v| json!({
            "key": v.name,
            "value": if v.secret { "" } else { v.value.as_str() },
            "type": if v.secret { "secret" } else { "default" },
            "enabled": true,
        })).collect::<Vec<_>>(),
        "_postman_variable_scope": "environment",
    });
    Ok(Exported {
        text: serde_json::to_string_pretty(&document).unwrap_or_default()
            + "
",
        file_name: format!("{}.postman_environment.json", slug(&env.name)),
        requests: 0,
        warnings: if secrets > 0 {
            vec![format!(
                "{secrets} secret value(s) left empty; fill them in after importing"
            )]
        } else {
            Vec::new()
        },
    })
}

// --------------------------------------------------------------------- Postman

fn kv(pairs: &[(&str, &str)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(k, v)| json!({ "key": k, "value": v, "type": "string" }))
            .collect(),
    )
}

fn postman_auth(auth: &Auth, warnings: &mut Vec<String>) -> Option<Value> {
    Some(match auth {
        Auth::None => json!({ "type": "noauth" }),
        Auth::Bearer { token } => json!({ "type": "bearer", "bearer": kv(&[("token", token)]) }),
        Auth::Basic { username, password } => json!({
            "type": "basic",
            "basic": kv(&[("username", username), ("password", password)])
        }),
        Auth::ApiKey {
            key,
            value,
            location,
        } => json!({
            "type": "apikey",
            "apikey": kv(&[
                ("key", key),
                ("value", value),
                ("in", if *location == ApiKeyLocation::Query { "query" } else { "header" }),
            ])
        }),
        Auth::OAuth2(c) => {
            let grant = match c.grant {
                Grant::AuthorizationCode if c.pkce => "authorization_code_with_pkce",
                Grant::AuthorizationCode => "authorization_code",
                Grant::ClientCredentials => "client_credentials",
                Grant::Password => "password_credentials",
            };
            json!({
                "type": "oauth2",
                "oauth2": kv(&[
                    ("grant_type", grant),
                    ("authUrl", &c.auth_url),
                    ("accessTokenUrl", &c.token_url),
                    ("clientId", &c.client_id),
                    ("clientSecret", &c.client_secret),
                    ("scope", &c.scope),
                    ("username", &c.username),
                    ("password", &c.password),
                    ("redirect_uri", &c.redirect_uri),
                    ("headerPrefix", &c.header_prefix),
                    ("client_authentication", if c.client_auth == ClientAuth::Body { "body" } else { "header" }),
                ])
            })
        }
        Auth::Digest(c) => json!({
            "type": "digest",
            "digest": kv(&[("username", &c.username), ("password", &c.password)])
        }),
        Auth::AwsV4(c) => json!({
            "type": "awsv4",
            "awsv4": kv(&[
                ("accessKey", &c.access_key),
                ("secretKey", &c.secret_key),
                ("sessionToken", &c.session_token),
                ("region", &c.region),
                ("service", &c.service),
            ])
        }),
        Auth::Hawk(c) => json!({
            "type": "hawk",
            "hawk": [
                { "key": "authId", "value": c.id, "type": "string" },
                { "key": "authKey", "value": c.key, "type": "string" },
                { "key": "algorithm", "value": c.algorithm.as_str(), "type": "string" },
                { "key": "extraData", "value": c.ext, "type": "string" },
                { "key": "includePayloadHash", "value": c.payload, "type": "boolean" },
            ]
        }),
        Auth::Ntlm(c) => json!({
            "type": "ntlm",
            "ntlm": kv(&[
                ("username", &c.username),
                ("password", &c.password),
                ("domain", &c.domain),
                ("workstation", &c.workstation),
            ])
        }),
    })
    .or_else(|| {
        warnings.push("an auth could not be exported".into());
        None
    })
}

fn postman_events(pre: &str, test: &str) -> Vec<Value> {
    let mut events = Vec::new();
    for (listen, code) in [("prerequest", pre), ("test", test)] {
        if !code.trim().is_empty() {
            events.push(json!({
                "listen": listen,
                "script": {
                    "type": "text/javascript",
                    "exec": code.lines().collect::<Vec<_>>()
                }
            }));
        }
    }
    events
}

fn postman_request(
    root: &Path,
    entry: &RequestEntry,
    inherited: &Inherited,
    warnings: &mut Vec<String>,
) -> Result<Value, CollectionError> {
    let r = &entry.request;
    // Folder headers do not exist in Postman: they go on each request.
    let (_, headers) = inherited.apply(r);
    let mut query: Vec<Value> = Vec::new();
    if let Some((_, q)) = r.url.split_once('?') {
        for pair in q.split('#').next().unwrap_or_default().split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            query.push(json!({ "key": k, "value": v }));
        }
    }
    for p in &r.disabled_params {
        query.push(json!({ "key": p.name, "value": p.value, "disabled": true }));
    }
    let mut url = json!({ "raw": r.url });
    if !query.is_empty() {
        url["query"] = Value::Array(query);
    }
    if !r.path_params.is_empty() {
        url["variable"] = Value::Array(
            r.path_params
                .iter()
                .map(|p| json!({ "key": p.name, "value": p.value }))
                .collect(),
        );
    }
    let mut request = json!({
        "method": r.method.to_string(),
        "header": headers.iter().map(|h| {
            let mut v = json!({ "key": h.name, "value": h.value });
            if !h.enabled { v["disabled"] = Value::Bool(true); }
            v
        }).collect::<Vec<_>>(),
        "url": url,
    });
    if let Some(form) = &r.form {
        let multipart = form.kind == FormKind::Multipart;
        let mode = if multipart { "formdata" } else { "urlencoded" };
        request["body"] = json!({
            "mode": mode,
            mode: form.fields.iter().map(|f| {
                let mut v = if f.file {
                    json!({ "key": f.name, "type": "file", "src": f.value })
                } else {
                    json!({ "key": f.name, "value": f.value, "type": "text" })
                };
                if !f.enabled { v["disabled"] = Value::Bool(true); }
                v
            }).collect::<Vec<_>>(),
        });
    } else if let Some(file) = &r.body_file {
        request["body"] = json!({ "mode": "file", "file": { "src": file } });
    } else if let Some(g) = &r.graphql {
        request["body"] = json!({
            "mode": "graphql",
            "graphql": { "query": g.query, "variables": g.variables }
        });
    } else if let Some(body) = r.body.as_ref().filter(|b| !b.is_empty()) {
        let language = match content_type(&headers).as_str() {
            ct if ct.contains("json") => "json",
            ct if ct.contains("xml") => "xml",
            "text/html" => "html",
            "application/javascript" => "javascript",
            _ => "text",
        };
        request["body"] = json!({
            "mode": "raw",
            "raw": body,
            "options": { "raw": { "language": language } }
        });
    }
    if let Some(auth) = r.auth.as_ref().and_then(|a| postman_auth(a, warnings)) {
        request["auth"] = auth;
    }
    let mut item = json!({ "name": entry.name, "request": request });
    let events = postman_events(&entry.scripts.pre, &entry.scripts.test);
    if !events.is_empty() {
        item["event"] = Value::Array(events);
    }
    let saved = examples::list_examples(root, &entry.id)?;
    if !saved.is_empty() {
        item["response"] = Value::Array(
            saved
                .iter()
                .map(|e| {
                    json!({
                        "name": e.name,
                        "code": e.status,
                        "header": e.headers.iter().map(|h| json!({ "key": h.name, "value": h.value })).collect::<Vec<_>>(),
                        "body": e.body,
                    })
                })
                .collect(),
        );
    }
    Ok(item)
}

fn postman_items(
    root: &Path,
    nodes: &[TreeNode],
    inherited: &Inherited,
    count: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<Value>, CollectionError> {
    let mut items = Vec::new();
    for node in nodes {
        match node {
            TreeNode::Folder {
                id,
                name,
                children,
                auth,
                headers,
                ..
            } => {
                let inner = inherited.enter(name, &None, headers);
                let mut item = json!({
                    "name": name,
                    "item": postman_items(root, children, &inner, count, warnings)?,
                });
                if let Some(auth) = auth.as_ref().and_then(|a| postman_auth(a, warnings)) {
                    item["auth"] = auth;
                }
                let scripts = collection::folder_scripts(root, id)?;
                let events = postman_events(&scripts.pre, &scripts.test);
                if !events.is_empty() {
                    item["event"] = Value::Array(events);
                }
                items.push(item);
            }
            TreeNode::Request(entry) => {
                *count += 1;
                items.push(postman_request(root, entry, inherited, warnings)?);
            }
        }
    }
    Ok(items)
}

// --------------------------------------------------------------------- OpenAPI

/// `{{base}}/users/:id` → server `{base}`, path `/users/{id}`, and the path params.
fn split_url(url: &str) -> (String, String, Vec<String>) {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let (server, path) = if let Some(rest) = url.strip_prefix("{{") {
        match rest.find("}}") {
            Some(end) => (format!("{{{}}}", &rest[..end]), rest[end + 2..].to_string()),
            None => (String::new(), url.to_string()),
        }
    } else if let Some(scheme_end) = url.find("://") {
        let after = &url[scheme_end + 3..];
        let host_end = after.find('/').unwrap_or(after.len());
        (
            url[..scheme_end + 3 + host_end].to_string(),
            after[host_end..].to_string(),
        )
    } else {
        (String::new(), url.to_string())
    };
    let mut params = Vec::new();
    let segments: Vec<String> = path
        .split('/')
        .map(|segment| {
            if let Some(name) = segment.strip_prefix(':') {
                params.push(name.to_string());
                format!("{{{name}}}")
            } else if segment.starts_with("{{") && segment.ends_with("}}") {
                let name = segment[2..segment.len() - 2].trim().to_string();
                params.push(name.clone());
                format!("{{{name}}}")
            } else {
                segment.to_string()
            }
        })
        .collect();
    let path = segments.join("/");
    let path = if path.is_empty() || !path.starts_with('/') {
        format!("/{path}")
    } else {
        path
    };
    (server, path, params)
}

fn example_value(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_string()))
}

fn security_scheme(auth: &Auth, warnings: &mut Vec<String>, at: &str) -> Option<(String, Value)> {
    Some(match auth {
        Auth::None => return None,
        Auth::Bearer { .. } => (
            "bearerAuth".into(),
            json!({ "type": "http", "scheme": "bearer" }),
        ),
        Auth::Basic { .. } => (
            "basicAuth".into(),
            json!({ "type": "http", "scheme": "basic" }),
        ),
        Auth::Digest(_) => (
            "digestAuth".into(),
            json!({ "type": "http", "scheme": "digest" }),
        ),
        Auth::ApiKey { key, location, .. } => (
            format!("apiKey_{}", collection::slugify(key).replace('-', "_")),
            json!({
                "type": "apiKey",
                "name": key,
                "in": if *location == ApiKeyLocation::Query { "query" } else { "header" }
            }),
        ),
        Auth::OAuth2(c) => {
            let scopes: Map<String, Value> = c
                .scope
                .split_whitespace()
                .map(|s| (s.to_string(), Value::String(String::new())))
                .collect();
            let flow = match c.grant {
                Grant::AuthorizationCode => (
                    "authorizationCode",
                    json!({ "authorizationUrl": c.auth_url, "tokenUrl": c.token_url, "scopes": scopes }),
                ),
                Grant::ClientCredentials => (
                    "clientCredentials",
                    json!({ "tokenUrl": c.token_url, "scopes": scopes }),
                ),
                Grant::Password => (
                    "password",
                    json!({ "tokenUrl": c.token_url, "scopes": scopes }),
                ),
            };
            let mut flows = Map::new();
            flows.insert(flow.0.into(), flow.1);
            ("oauth2".into(), json!({ "type": "oauth2", "flows": flows }))
        }
        other => {
            warnings.push(format!(
                "{at}: {} auth has no OpenAPI security scheme; left out",
                other.label()
            ));
            return None;
        }
    })
}

fn openapi(
    root: &Path,
    name: &str,
    nodes: &[TreeNode],
    top: &Inherited,
    count: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<String, CollectionError> {
    let mut paths: Map<String, Value> = Map::new();
    let mut servers: Vec<String> = Vec::new();
    let mut schemes: Map<String, Value> = Map::new();
    let mut operation_ids: Vec<String> = Vec::new();

    fn walk<'a>(
        nodes: &'a [TreeNode],
        inherited: &Inherited,
        out: &mut Vec<(&'a RequestEntry, Inherited)>,
    ) {
        for node in nodes {
            match node {
                TreeNode::Folder {
                    name,
                    children,
                    auth,
                    headers,
                    ..
                } => walk(children, &inherited.enter(name, auth, headers), out),
                TreeNode::Request(entry) => out.push((entry, inherited.clone())),
            }
        }
    }
    let mut requests = Vec::new();
    walk(nodes, top, &mut requests);

    for (entry, inherited) in requests {
        *count += 1;
        let r = &entry.request;
        let (auth, headers) = inherited.apply(r);
        let (server, path, path_params) = split_url(&r.url);
        if !server.is_empty() && !servers.contains(&server) {
            servers.push(server);
        }
        let method = r.method.to_string().to_ascii_lowercase();
        let path_item = paths
            .entry(path.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if path_item.get(&method).is_some() {
            warnings.push(format!(
                "{}: {} {path} is already in the document; skipped",
                entry.name, r.method
            ));
            continue;
        }
        let mut id = collection::slugify(&entry.name).replace('-', "_");
        if id.is_empty() {
            id = method.clone();
        }
        let base = id.clone();
        let mut n = 2;
        while operation_ids.contains(&id) {
            id = format!("{base}_{n}");
            n += 1;
        }
        operation_ids.push(id.clone());

        let mut parameters: Vec<Value> = path_params
            .iter()
            .map(|p| {
                let mut v = json!({ "name": p, "in": "path", "required": true, "schema": { "type": "string" } });
                if let Some(param) = r.path_params.iter().find(|pp| &pp.name == p) {
                    if !param.value.is_empty() && !param.value.contains("{{") {
                        v["example"] = Value::String(param.value.clone());
                    }
                }
                v
            })
            .collect();
        if let Some((_, q)) = r.url.split_once('?') {
            for pair in q.split('#').next().unwrap_or_default().split('&') {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                if k.is_empty() {
                    continue;
                }
                let mut p = json!({ "name": k, "in": "query", "schema": { "type": "string" } });
                if !v.is_empty() && !v.contains("{{") {
                    p["example"] = Value::String(v.to_string());
                }
                parameters.push(p);
            }
        }
        for p in &r.disabled_params {
            parameters.push(json!({ "name": p.name, "in": "query", "required": false, "schema": { "type": "string" } }));
        }
        for h in headers.iter().filter(|h| h.enabled) {
            let lower = h.name.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "content-type" | "authorization" | "accept" | "cookie"
            ) {
                continue;
            }
            parameters
                .push(json!({ "name": h.name, "in": "header", "schema": { "type": "string" } }));
        }

        let mut operation = json!({ "operationId": id, "summary": entry.name });
        if !inherited.path.is_empty() {
            operation["tags"] = json!([inherited.path.join(" / ")]);
        }
        if !parameters.is_empty() {
            operation["parameters"] = Value::Array(parameters);
        }
        if let Some(form) = &r.form {
            let multipart = form.kind == FormKind::Multipart;
            let properties: Map<String, Value> = form
                .fields
                .iter()
                .map(|f| {
                    let schema = if f.file {
                        json!({ "type": "string", "format": "binary" })
                    } else {
                        json!({ "type": "string" })
                    };
                    (f.name.clone(), schema)
                })
                .collect();
            let ct = if multipart {
                "multipart/form-data"
            } else {
                "application/x-www-form-urlencoded"
            };
            operation["requestBody"] = json!({
                "content": { ct: { "schema": { "type": "object", "properties": properties } } }
            });
        } else if r.body_file.is_some() {
            operation["requestBody"] = json!({
                "content": { "application/octet-stream": { "schema": { "type": "string", "format": "binary" } } }
            });
        } else if let Some(body) = r.body.as_ref().filter(|b| !b.trim().is_empty()) {
            let ct = match content_type(&headers) {
                ct if ct.is_empty() => "application/json".to_string(),
                ct => ct,
            };
            operation["requestBody"] =
                json!({ "content": { ct: { "example": example_value(body) } } });
        }
        let mut responses = Map::new();
        for example in examples::list_examples(root, &entry.id)? {
            let ct = content_type(&example.headers);
            let ct = if ct.is_empty() {
                "application/json".to_string()
            } else {
                ct
            };
            let mut response = json!({ "description": example.name });
            if !example.body.is_empty() {
                response["content"] = json!({ ct: { "example": example_value(&example.body) } });
            }
            responses
                .entry(example.status.to_string())
                .or_insert(response);
        }
        if responses.is_empty() {
            responses.insert("200".into(), json!({ "description": "OK" }));
        }
        operation["responses"] = Value::Object(responses);
        if let Some(auth) = &auth {
            match security_scheme(auth, warnings, &entry.name) {
                Some((scheme, definition)) => {
                    let scopes: Vec<String> = match auth {
                        Auth::OAuth2(c) => c.scope.split_whitespace().map(str::to_string).collect(),
                        _ => Vec::new(),
                    };
                    schemes.entry(scheme.clone()).or_insert(definition);
                    operation["security"] = json!([{ scheme: scopes }]);
                }
                None if matches!(auth, Auth::None) => operation["security"] = json!([]),
                None => {}
            }
        }
        path_item[&method] = operation;
    }

    // A server variable's default: its value in the first environment that sets it.
    let environments =
        crate::environment::list_environments(root, &crate::environment::MemoryStore::default())
            .unwrap_or_default();
    // Values may use other variables (`{{kongHost}}/bff/v1`): resolve them in the same
    // environment, a few levels deep.
    let default_for = |var: &str| {
        environments
            .iter()
            .filter(|e| e.variables.iter().any(|v| v.name == var && !v.secret))
            .find_map(|env| {
                let vars: crate::variables::Variables = env
                    .variables
                    .iter()
                    .filter(|v| !v.secret)
                    .map(|v| (v.name.clone(), v.value.clone()))
                    .collect();
                let mut value = vars.get(var)?.clone();
                for _ in 0..5 {
                    if !value.contains("{{") {
                        break;
                    }
                    let mut missing = std::collections::BTreeSet::new();
                    value = crate::variables::resolve_text(&value, &vars, &mut missing);
                    if !missing.is_empty() {
                        return None;
                    }
                }
                (!value.is_empty() && !value.contains("{{")).then_some(value)
            })
            .unwrap_or_else(|| "http://localhost".into())
    };
    let mut document = json!({
        "openapi": "3.0.3",
        "info": { "title": name, "version": "1.0.0" },
    });
    if !servers.is_empty() {
        document["servers"] = Value::Array(
            servers
                .iter()
                .map(|s| {
                    if let Some(var) = s.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
                        let mut variables = Map::new();
                        variables.insert(
                            var.to_string(),
                            json!({ "default": default_for(var), "description": format!("The {{{{{var}}}}} variable of your environments") }),
                        );
                        json!({ "url": s, "variables": variables })
                    } else {
                        json!({ "url": s })
                    }
                })
                .collect(),
        );
    }
    document["paths"] = Value::Object(paths);
    if !schemes.is_empty() {
        document["components"] = json!({ "securitySchemes": schemes });
    }
    serde_norway::to_string(&document).map_err(|e| CollectionError::Io(format!("OpenAPI: {e}")))
}

// --------------------------------------------------------------------- .http

fn http_file(
    nodes: &[TreeNode],
    inherited: &Inherited,
    out: &mut String,
    count: &mut usize,
    warnings: &mut Vec<String>,
) {
    for node in nodes {
        match node {
            TreeNode::Folder {
                name,
                children,
                auth,
                headers,
                ..
            } => http_file(
                children,
                &inherited.enter(name, auth, headers),
                out,
                count,
                warnings,
            ),
            TreeNode::Request(entry) => {
                *count += 1;
                let r = &entry.request;
                let (auth, mut headers) = inherited.apply(r);
                let mut url = r.url.clone();
                for p in &r.path_params {
                    url = url.replace(&format!(":{}", p.name), &p.value);
                }
                match &auth {
                    Some(Auth::Bearer { token }) => {
                        headers.push(Header::new("Authorization", format!("Bearer {token}")))
                    }
                    Some(Auth::Basic { username, password }) => headers.push(Header::new(
                        "Authorization",
                        format!("Basic {username} {password}"),
                    )),
                    Some(Auth::ApiKey {
                        key,
                        value,
                        location: ApiKeyLocation::Header,
                    }) => headers.push(Header::new(key.clone(), value.clone())),
                    Some(Auth::ApiKey {
                        key,
                        value,
                        location: ApiKeyLocation::Query,
                    }) => {
                        let sep = if url.contains('?') { '&' } else { '?' };
                        url = format!("{url}{sep}{key}={value}");
                    }
                    Some(Auth::None) | None => {}
                    Some(other) => warnings.push(format!(
                        "{}: {} auth cannot be written in a .http file; add it by hand",
                        entry.name,
                        other.label()
                    )),
                }
                out.push_str(&format!("\n### {}\n", entry.name));
                if !inherited.path.is_empty() {
                    out.push_str(&format!("# folder: {}\n", inherited.path.join(" / ")));
                }
                out.push_str(&format!("{} {url}\n", r.method));
                for h in &headers {
                    let prefix = if h.enabled { "" } else { "# " };
                    out.push_str(&format!("{prefix}{}: {}\n", h.name, h.value));
                }
                if let Some(form) = &r.form {
                    if form.kind == FormKind::Urlencoded {
                        if content_type(&headers).is_empty() {
                            out.push_str("Content-Type: application/x-www-form-urlencoded\n");
                        }
                        let body: Vec<String> = form
                            .fields
                            .iter()
                            .filter(|f| f.enabled)
                            .map(|f| format!("{}={}", f.name, f.value))
                            .collect();
                        out.push_str(&format!("\n{}\n", body.join("&")));
                    } else {
                        warnings.push(format!(
                            "{}: multipart bodies are not written to .http files; add it by hand",
                            entry.name
                        ));
                    }
                } else if let Some(file) = &r.body_file {
                    out.push_str(&format!("\n< ./{file}\n"));
                } else if let Some(body) = r.wire_body().filter(|b| !b.is_empty()) {
                    out.push_str(&format!("\n{}\n", body.trim_end()));
                }
                if !entry.scripts.is_empty() {
                    warnings.push(format!(
                        "{}: scripts are not exported to .http files",
                        entry.name
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{init_workspace, save_request, Scripts};
    use crate::import;
    use crate::HttpMethod;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        init_workspace(dir.path(), "Shop").unwrap();
        let folder = collection::create_folder(dir.path(), "", "Users").unwrap();
        assert_eq!(folder, "01-users");
        let mut list = HttpRequest::new(HttpMethod::Get, "{{base}}/users?page=1");
        list.auth = Some(Auth::Bearer {
            token: "{{token}}".into(),
        });
        list.headers.push(Header::new("X-Tenant", "t1"));
        save_request(
            dir.path(),
            &RequestEntry {
                id: "01-users/01-list.pulpo.yaml".into(),
                name: "List users".into(),
                protocol: crate::Protocol::Http,
                request: list,
                scripts: Scripts {
                    pre: String::new(),
                    test: "pm.test('ok', () => pm.response.to.have.status(200));".into(),
                },
            },
        )
        .unwrap();
        let mut create = HttpRequest::new(HttpMethod::Post, "{{base}}/users/:id");
        create.path_params.push(crate::http::Param::new("id", "7"));
        create
            .headers
            .push(Header::new("Content-Type", "application/json"));
        create.body = Some("{\"name\": \"Ana\"}".into());
        save_request(
            dir.path(),
            &RequestEntry {
                id: "01-users/02-create.pulpo.yaml".into(),
                name: "Create user".into(),
                protocol: crate::Protocol::Http,
                request: create,
                scripts: Scripts::default(),
            },
        )
        .unwrap();
        dir
    }

    #[test]
    fn exports_postman_that_imports_back() {
        let dir = workspace();
        let exported = export(dir.path(), "", ExportFormat::Postman).unwrap();
        assert_eq!(exported.requests, 2);
        assert_eq!(exported.file_name, "shop.postman_collection.json");
        let plan = import::parse(&exported.text).unwrap();
        assert_eq!(plan.format, import::ImportFormat::PostmanCollection);
        let import::ImportNode::Folder { children, .. } = &plan.nodes[0] else {
            panic!("a folder")
        };
        let import::ImportNode::Request {
            request, scripts, ..
        } = &children[0]
        else {
            panic!()
        };
        assert_eq!(request.url, "{{base}}/users?page=1");
        assert_eq!(
            request.auth,
            Some(Auth::Bearer {
                token: "{{token}}".into()
            })
        );
        assert!(scripts.test.contains("pm.test"));
        let import::ImportNode::Request { request, .. } = &children[1] else {
            panic!()
        };
        assert_eq!(request.body.as_deref(), Some("{\"name\": \"Ana\"}"));
        assert_eq!(request.path_params[0].value, "7");
    }

    #[test]
    fn exports_openapi_with_paths_params_and_security() {
        let dir = workspace();
        let exported = export(dir.path(), "", ExportFormat::OpenApi).unwrap();
        let doc: Value = serde_norway::from_str(&exported.text).unwrap();
        assert_eq!(doc["servers"][0]["url"], "{base}");
        let list = &doc["paths"]["/users"]["get"];
        assert_eq!(list["operationId"], "list_users");
        assert_eq!(list["tags"][0], "Users");
        assert_eq!(list["security"][0]["bearerAuth"], json!([]));
        assert!(list["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "page" && p["in"] == "query"));
        let create = &doc["paths"]["/users/{id}"]["post"];
        assert_eq!(create["parameters"][0]["in"], "path");
        assert_eq!(
            create["requestBody"]["content"]["application/json"]["example"]["name"],
            "Ana"
        );
        assert_eq!(
            doc["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
        // A nested variable in an environment gives the server's default.
        let env = crate::environment::create_environment(
            dir.path(),
            "Local",
            &crate::environment::MemoryStore::default(),
        )
        .unwrap();
        crate::environment::save_environment(
            dir.path(),
            &crate::environment::Environment {
                id: env.id,
                name: "Local".into(),
                variables: vec![
                    crate::environment::Variable {
                        name: "host".into(),
                        value: "https://api.shop".into(),
                        secret: false,
                        has_value: false,
                    },
                    crate::environment::Variable {
                        name: "base".into(),
                        value: "{{host}}/v1".into(),
                        secret: false,
                        has_value: false,
                    },
                ],
            },
            &crate::environment::MemoryStore::default(),
        )
        .unwrap();
        let again: Value =
            serde_norway::from_str(&export(dir.path(), "", ExportFormat::OpenApi).unwrap().text)
                .unwrap();
        assert_eq!(
            again["servers"][0]["variables"]["base"]["default"],
            "https://api.shop/v1"
        );
        // And Pulpo reads it back.
        assert_eq!(
            import::parse(&exported.text).unwrap().format,
            import::ImportFormat::OpenApi
        );
    }

    #[test]
    fn exports_an_http_file_that_imports_back() {
        let dir = workspace();
        let exported = export(dir.path(), "01-users", ExportFormat::Http).unwrap();
        assert!(
            exported
                .text
                .contains("### List users\nGET {{base}}/users?page=1\n"),
            "{}",
            exported.text
        );
        assert!(exported.text.contains("Authorization: Bearer {{token}}"));
        assert!(exported.text.contains("POST {{base}}/users/7\n"));
        assert!(exported.warnings.iter().any(|w| w.contains("scripts")));
        let plan = import::parse(&exported.text).unwrap();
        assert_eq!(plan.nodes.len(), 2);
    }
}
