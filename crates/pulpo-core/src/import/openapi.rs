//! OpenAPI 3.x documents (JSON or YAML): one folder per tag, one request per operation.

use super::{ImportError, ImportFormat, ImportNode, ImportPlan};
use crate::environment::{Environment, Variable};
use crate::http::{ApiKeyLocation, Auth, Header, HttpRequest};
use crate::HttpMethod;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const METHODS: [(&str, HttpMethod); 7] = [
    ("get", HttpMethod::Get),
    ("post", HttpMethod::Post),
    ("put", HttpMethod::Put),
    ("patch", HttpMethod::Patch),
    ("delete", HttpMethod::Delete),
    ("head", HttpMethod::Head),
    ("options", HttpMethod::Options),
];

/// Follows a local `$ref` like `#/components/schemas/Order`.
fn deref<'a>(doc: &'a Value, value: &'a Value) -> &'a Value {
    let mut current = value;
    for _ in 0..16 {
        match current.get("$ref").and_then(|r| r.as_str()) {
            Some(reference) if reference.starts_with("#/") => match doc.pointer(&reference[1..]) {
                Some(target) => current = target,
                None => return current,
            },
            _ => return current,
        }
    }
    current
}

/// Builds an example value from a schema: example, default, enum, then a placeholder by type.
fn example(doc: &Value, schema: &Value, depth: usize) -> Value {
    let schema = deref(doc, schema);
    if let Some(v) = schema.get("example") {
        return v.clone();
    }
    if let Some(v) = schema.get("default") {
        return v.clone();
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(|e| e.as_array())
        .and_then(|e| e.first())
    {
        return first.clone();
    }
    if depth > 6 {
        return Value::Null;
    }
    for combinator in ["allOf", "oneOf", "anyOf"] {
        if let Some(parts) = schema.get(combinator).and_then(|p| p.as_array()) {
            if combinator == "allOf" {
                let mut merged = Map::new();
                for part in parts {
                    if let Value::Object(o) = example(doc, part, depth + 1) {
                        merged.extend(o);
                    }
                }
                return Value::Object(merged);
            }
            if let Some(first) = parts.first() {
                return example(doc, first, depth + 1);
            }
        }
    }
    let kind = schema
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| {
            if schema.get("properties").is_some() {
                "object"
            } else {
                ""
            }
        });
    match kind {
        "object" => {
            let mut out = Map::new();
            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                for (name, prop) in props {
                    if deref(doc, prop).get("readOnly").and_then(|r| r.as_bool()) == Some(true) {
                        continue;
                    }
                    out.insert(name.clone(), example(doc, prop, depth + 1));
                }
            }
            Value::Object(out)
        }
        "array" => match schema.get("items") {
            Some(items) => json!([example(doc, items, depth + 1)]),
            None => json!([]),
        },
        "integer" => json!(0),
        "number" => json!(0.0),
        "boolean" => json!(false),
        "string" => match schema.get("format").and_then(|f| f.as_str()) {
            Some("date-time") => json!("2026-01-01T00:00:00Z"),
            Some("date") => json!("2026-01-01"),
            Some("uuid") => json!("{{$guid}}"),
            Some("email") => json!("user@example.com"),
            _ => json!("string"),
        },
        _ => Value::Null,
    }
}

fn request_body(doc: &Value, operation: &Value, headers: &mut Vec<Header>) -> Option<String> {
    let body = deref(doc, operation.get("requestBody")?);
    let content = body.get("content")?.as_object()?;
    let (media, spec) = content
        .iter()
        .find(|(m, _)| m.contains("json"))
        .or_else(|| content.iter().next())?;
    headers.push(Header::new("Content-Type", media.clone()));
    let value = if let Some(v) = spec.get("example") {
        v.clone()
    } else if let Some(first) = spec
        .get("examples")
        .and_then(|e| e.as_object())
        .and_then(|e| e.values().next())
    {
        deref(doc, first)
            .get("value")
            .cloned()
            .unwrap_or(Value::Null)
    } else if let Some(schema) = spec.get("schema") {
        example(doc, schema, 0)
    } else {
        Value::Null
    };
    if media.contains("json") {
        Some(serde_json::to_string_pretty(&value).unwrap_or_default())
    } else {
        match value {
            Value::String(s) => Some(s),
            Value::Null => Some(String::new()),
            other => Some(other.to_string()),
        }
    }
}

/// `{orderId}` -> `{{orderId}}`.
fn path_template(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        match rest[start..].find('}') {
            Some(end) => {
                out.push_str("{{");
                out.push_str(&rest[start + 1..start + end]);
                out.push_str("}}");
                rest = &rest[start + end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The security schemes Pulpo can express, by name, as auth that uses variables.
fn auth_schemes(doc: &Value) -> Vec<(String, Auth)> {
    let mut schemes = Vec::new();
    if let Some(defs) = doc
        .pointer("/components/securitySchemes")
        .and_then(|s| s.as_object())
    {
        for (name, def) in defs {
            let def = deref(doc, def);
            let kind = def.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let scheme = def
                .get("scheme")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let auth = if (kind == "http" && scheme == "bearer")
                || kind == "oauth2"
                || kind == "openIdConnect"
            {
                Auth::Bearer {
                    token: "{{token}}".into(),
                }
            } else if kind == "http" && scheme == "basic" {
                Auth::Basic {
                    username: "{{username}}".into(),
                    password: "{{password}}".into(),
                }
            } else if kind == "apiKey" {
                let location = match def.get("in").and_then(|i| i.as_str()) {
                    Some("header") => ApiKeyLocation::Header,
                    Some("query") => ApiKeyLocation::Query,
                    _ => continue,
                };
                Auth::ApiKey {
                    key: def
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("X-API-Key")
                        .to_string(),
                    value: "{{apiKey}}".into(),
                    location,
                }
            } else {
                continue;
            };
            schemes.push((name.clone(), auth));
        }
    }
    schemes
}

/// The auth of a `security` requirement list: `[]` means none; otherwise the first
/// scheme of the first requirement that Pulpo knows. `None` when not given.
fn security_auth(requirements: Option<&Value>, schemes: &[(String, Auth)]) -> Option<Auth> {
    let list = requirements?.as_array()?;
    let Some(first) = list.first().and_then(|r| r.as_object()) else {
        return Some(Auth::None);
    };
    first.keys().find_map(|name| {
        schemes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, auth)| auth.clone())
    })
}

/// Replaces `$ref`s that point to other files (`common.yml#/components/schemas/X`)
/// with their content, so the rest of the importer only deals with local refs.
/// `local` is the document that `#/...` refs belong to while inlining another file.
pub(super) fn inline_external_refs(
    value: &mut Value,
    base: &Path,
    local: Option<&Value>,
    cache: &mut HashMap<PathBuf, Value>,
    warnings: &mut Vec<String>,
    depth: usize,
) {
    if depth > 24 {
        return;
    }
    if let Some(reference) = value
        .get("$ref")
        .and_then(|r| r.as_str())
        .map(str::to_string)
    {
        let (file, pointer) = reference
            .split_once('#')
            .unwrap_or((reference.as_str(), ""));
        if file.is_empty() {
            if let Some(doc) = local {
                if let Some(target) = doc.pointer(pointer) {
                    let mut target = target.clone();
                    let owned = doc.clone();
                    inline_external_refs(
                        &mut target,
                        base,
                        Some(&owned),
                        cache,
                        warnings,
                        depth + 1,
                    );
                    *value = target;
                }
            }
            return;
        }
        if file.starts_with("http://") || file.starts_with("https://") {
            warnings.push(format!("remote $ref not followed: {reference}"));
            return;
        }
        let path = base.join(file);
        if !cache.contains_key(&path) {
            let loaded = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_norway::from_str::<Value>(&text).map_err(|e| e.to_string()));
            match loaded {
                Ok(doc) => {
                    cache.insert(path.clone(), doc);
                }
                Err(e) => {
                    warnings.push(format!("could not read {}: {e}", path.display()));
                    return;
                }
            }
        }
        let doc = cache[&path].clone();
        if let Some(target) = doc.pointer(pointer) {
            let mut target = target.clone();
            let next_base = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| base.to_path_buf());
            inline_external_refs(
                &mut target,
                &next_base,
                Some(&doc),
                cache,
                warnings,
                depth + 1,
            );
            *value = target;
        } else {
            warnings.push(format!("$ref target not found: {reference}"));
        }
        return;
    }
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                inline_external_refs(child, base, local, cache, warnings, depth + 1);
            }
        }
        Value::Array(items) => {
            for child in items {
                inline_external_refs(child, base, local, cache, warnings, depth + 1);
            }
        }
        _ => {}
    }
}

pub(super) fn document(doc: &Value) -> Result<ImportPlan, ImportError> {
    let paths = doc
        .get("paths")
        .and_then(|p| p.as_object())
        .ok_or_else(|| ImportError::Invalid("OpenAPI document without paths".into()))?;
    let title = doc
        .pointer("/info/title")
        .and_then(|t| t.as_str())
        .unwrap_or("Imported API")
        .to_string();
    let mut warnings = Vec::new();
    let schemes = auth_schemes(doc);
    let folder_auth = security_auth(doc.get("security"), &schemes);
    let mut used_auth: Vec<Auth> = folder_auth.iter().cloned().collect();

    let mut folders: Vec<(String, Vec<ImportNode>)> = Vec::new();
    let mut add =
        |folder: String, node: ImportNode| match folders.iter_mut().find(|(n, _)| *n == folder) {
            Some((_, nodes)) => nodes.push(node),
            None => folders.push((folder, vec![node])),
        };

    for (path, item) in paths {
        let item = deref(doc, item);
        let shared_params: Vec<Value> = item
            .get("parameters")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        for (key, method) in METHODS {
            let Some(operation) = item.get(key) else {
                continue;
            };
            let mut headers = Vec::new();
            let mut query = Vec::new();
            let params = shared_params.iter().chain(
                operation
                    .get("parameters")
                    .and_then(|p| p.as_array())
                    .into_iter()
                    .flatten(),
            );
            for param in params {
                let param = deref(doc, param);
                let name = param
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default();
                let required = param
                    .get("required")
                    .and_then(|r| r.as_bool())
                    .unwrap_or(false);
                match param.get("in").and_then(|i| i.as_str()) {
                    Some("query") if required => query.push(format!("{name}={{{{{name}}}}}")),
                    Some("header") if !headers.iter().any(|h: &Header| h.name == name) => headers
                        .push(Header {
                            name: name.to_string(),
                            value: format!("{{{{{name}}}}}"),
                            enabled: required,
                        }),
                    _ => {}
                }
            }
            // Only an operation that differs from the document's security gets its own auth.
            let auth = security_auth(operation.get("security"), &schemes)
                .filter(|auth| Some(auth) != folder_auth.as_ref());
            if let Some(auth) = &auth {
                if !used_auth.contains(auth) {
                    used_auth.push(auth.clone());
                }
            }
            let body = request_body(doc, operation, &mut headers);
            let mut url = format!("{{{{baseUrl}}}}{}", path_template(path));
            if !query.is_empty() {
                url.push('?');
                url.push_str(&query.join("&"));
            }
            let name = operation
                .get("summary")
                .and_then(|s| s.as_str())
                .or_else(|| operation.get("operationId").and_then(|s| s.as_str()))
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {path}", key.to_uppercase()));
            let folder = operation
                .get("tags")
                .and_then(|t| t.as_array())
                .and_then(|t| t.first())
                .and_then(|t| t.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    path.trim_start_matches('/')
                        .split('/')
                        .next()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("root")
                        .to_string()
                });
            add(
                folder,
                ImportNode::Request {
                    name,
                    request: HttpRequest {
                        method,
                        url,
                        headers,
                        body,
                        timeout_ms: None,
                        disabled_params: Vec::new(),
                        path_params: Vec::new(),
                        auth,
                    },
                    scripts: Default::default(),
                },
            );
        }
        if item.get("trace").is_some() {
            warnings.push(format!(
                "{path}: TRACE operations are not supported; skipped"
            ));
        }
    }

    let base_url = doc
        .pointer("/servers/0/url")
        .and_then(|u| u.as_str())
        .unwrap_or("http://localhost")
        .trim_end_matches('/')
        .to_string();
    let base_url = match doc
        .pointer("/servers/0/variables")
        .and_then(|v| v.as_object())
    {
        Some(vars) => vars.iter().fold(base_url, |url, (name, spec)| {
            url.replace(
                &format!("{{{name}}}"),
                spec.get("default").and_then(|d| d.as_str()).unwrap_or(""),
            )
        }),
        None => base_url,
    };
    let mut variables = vec![Variable {
        name: "baseUrl".into(),
        value: base_url,
        secret: false,
        has_value: false,
    }];
    let used: Vec<&str> = used_auth
        .iter()
        .flat_map(|auth| match auth {
            Auth::Bearer { .. } => vec!["token"],
            Auth::Basic { .. } => vec!["username", "password"],
            Auth::ApiKey { .. } => vec!["apiKey"],
            Auth::None => vec![],
        })
        .collect();
    for name in ["token", "apiKey", "username", "password"] {
        if used.contains(&name) {
            variables.push(Variable {
                name: name.into(),
                value: String::new(),
                secret: name != "username",
                has_value: false,
            });
        }
    }

    Ok(ImportPlan {
        format: ImportFormat::OpenApi,
        folder: Some(title.clone()),
        folder_scripts: Default::default(),
        folder_auth,
        nodes: folders
            .into_iter()
            .map(|(name, children)| ImportNode::Folder {
                name,
                children,
                scripts: Default::default(),
                auth: None,
            })
            .collect(),
        environments: vec![Environment {
            id: String::new(),
            name: title,
            variables,
        }],
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    const SPEC: &str = r#"
openapi: 3.0.3
info:
  title: Orders API
  version: "1"
servers:
  - url: https://{env}.shop.test/api/
    variables:
      env:
        default: dev
security:
  - bearerAuth: []
paths:
  /orders:
    get:
      tags: [Orders]
      summary: List orders
      parameters:
        - name: status
          in: query
          required: true
          schema: { type: string }
        - name: page
          in: query
          schema: { type: integer }
        - $ref: '#/components/parameters/Tenant'
    post:
      tags: [Orders]
      operationId: createOrder
      requestBody:
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/NewOrder'
  /orders/{orderId}:
    parameters:
      - name: orderId
        in: path
        required: true
        schema: { type: string }
    delete:
      tags: [Orders]
      security: []
  /health:
    get: {}
components:
  parameters:
    Tenant:
      name: X-Tenant-ID
      in: header
      required: true
      schema: { type: string }
  securitySchemes:
    bearerAuth:
      type: http
      scheme: bearer
  schemas:
    NewOrder:
      type: object
      properties:
        id: { type: string, format: uuid, readOnly: true }
        storeId: { type: string, format: uuid }
        items:
          type: array
          items:
            allOf:
              - type: object
                properties:
                  sku: { type: string, example: "SKU-1" }
              - type: object
                properties:
                  quantity: { type: integer, default: 1 }
        mode: { type: string, enum: [PICKUP, DELIVERY] }
"#;

    #[test]
    fn imports_operations_grouped_by_tag() {
        let plan = parse(SPEC).unwrap();
        assert_eq!(plan.format, ImportFormat::OpenApi);
        assert_eq!(plan.folder.as_deref(), Some("Orders API"));
        let names: Vec<_> = plan
            .nodes
            .iter()
            .map(|n| match n {
                ImportNode::Folder { name, children, .. } => format!("{name}:{}", children.len()),
                ImportNode::Request { name, .. } => name.clone(),
            })
            .collect();
        assert_eq!(names, ["Orders:3", "health:1"], "document order is kept");

        let ImportNode::Folder { children, .. } = &plan.nodes[0] else {
            panic!()
        };
        let ImportNode::Request { name, request, .. } = &children[0] else {
            panic!()
        };
        assert_eq!(name, "List orders");
        assert_eq!(request.url, "{{baseUrl}}/orders?status={{status}}");
        assert_eq!(
            plan.folder_auth,
            Some(Auth::Bearer {
                token: "{{token}}".into()
            })
        );
        assert_eq!(request.auth, None, "inherits the document's security");
        assert!(request
            .headers
            .iter()
            .any(|h| h.name == "X-Tenant-ID" && h.value == "{{X-Tenant-ID}}"));

        let ImportNode::Request { name, request, .. } = &children[1] else {
            panic!()
        };
        assert_eq!(name, "createOrder");
        let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"storeId": "{{$guid}}", "items": [{"sku": "SKU-1", "quantity": 1}], "mode": "PICKUP"})
        );

        let ImportNode::Request { name, request, .. } = &children[2] else {
            panic!()
        };
        assert_eq!(name, "DELETE /orders/{orderId}");
        assert_eq!(request.url, "{{baseUrl}}/orders/{{orderId}}");
        assert_eq!(request.auth, Some(Auth::None), "security: [] disables auth");

        let env = &plan.environments[0];
        assert_eq!(env.variables[0].value, "https://dev.shop.test/api");
        assert!(env.variables.iter().any(|v| v.name == "token" && v.secret));
    }

    #[test]
    fn inlines_refs_to_other_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("common.yml"),
            "components:\n  schemas:\n    Money:\n      type: object\n      properties:\n        amount: { type: number, example: 9.5 }\n        currency: { $ref: '#/components/schemas/Currency' }\n    Currency: { type: string, example: EUR }\n",
        )
        .unwrap();
        let spec = "openapi: 3.0.0\ninfo: { title: Pay }\npaths:\n  /pay:\n    post:\n      requestBody:\n        content:\n          application/json:\n            schema: { $ref: 'common.yml#/components/schemas/Money' }\n  /missing:\n    post:\n      requestBody:\n        content:\n          application/json:\n            schema: { $ref: 'nope.yml#/x' }\n";
        let path = dir.path().join("pay.yml");
        std::fs::write(&path, spec).unwrap();

        let plan = super::super::parse_file(&path).unwrap();

        let ImportNode::Folder { children, .. } = &plan.nodes[0] else {
            panic!()
        };
        let ImportNode::Request { request, .. } = &children[0] else {
            panic!()
        };
        let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body, json!({"amount": 9.5, "currency": "EUR"}));
        assert!(plan.warnings.iter().any(|w| w.contains("nope.yml")));
    }

    #[test]
    fn converts_path_templates() {
        assert_eq!(path_template("/a/{b}/c/{d}"), "/a/{{b}}/c/{{d}}");
        assert_eq!(path_template("/open/{x"), "/open/{x");
    }
}
