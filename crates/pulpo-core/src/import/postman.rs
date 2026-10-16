//! Postman collection v2.0/v2.1 and Postman environment files.

use super::{ImportError, ImportFormat, ImportNode, ImportPlan};
use crate::collection::Scripts;
use crate::environment::{Environment, Variable};
use crate::http::{ApiKeyLocation, Auth, Header, HttpRequest, Param};
use crate::HttpMethod;
use serde_json::Value;

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn kv(list: Option<&Value>, key: &str) -> String {
    list.and_then(|l| l.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|i| i.get("key").and_then(|k| k.as_str()) == Some(key))
        })
        .map(|i| text(i.get("value")))
        .unwrap_or_default()
}

/// The auth a level of the collection sets. `None` = inherit from the parent.
fn parse_auth(auth: Option<&Value>, warnings: &mut Vec<String>, at: &str) -> Option<Auth> {
    let auth = auth?;
    let kind = auth
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("noauth");
    match kind {
        "noauth" => Some(Auth::None),
        "inherit" => None,
        "bearer" => Some(Auth::Bearer {
            token: kv(auth.get("bearer"), "token"),
        }),
        "basic" => Some(Auth::Basic {
            username: kv(auth.get("basic"), "username"),
            password: kv(auth.get("basic"), "password"),
        }),
        "apikey" => {
            let key = kv(auth.get("apikey"), "key");
            Some(Auth::ApiKey {
                key: if key.is_empty() {
                    "X-API-Key".into()
                } else {
                    key
                },
                value: kv(auth.get("apikey"), "value"),
                location: if kv(auth.get("apikey"), "in") == "query" {
                    ApiKeyLocation::Query
                } else {
                    ApiKeyLocation::Header
                },
            })
        }
        other => {
            warnings.push(format!(
                "{at}: '{other}' auth is not supported yet; it inherits from its folder"
            ));
            None
        }
    }
}

fn url_of(url: Option<&Value>) -> String {
    match url {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => {
            if let Some(raw) = o.get("raw").and_then(|r| r.as_str()) {
                return raw.to_string();
            }
            let protocol = o
                .get("protocol")
                .and_then(|p| p.as_str())
                .map(|p| format!("{p}://"))
                .unwrap_or_default();
            let host = match o.get("host") {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .map(|p| text(Some(p)))
                    .collect::<Vec<_>>()
                    .join("."),
                other => text(other),
            };
            let path = match o.get("path") {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .map(|p| text(Some(p)))
                    .collect::<Vec<_>>()
                    .join("/"),
                other => text(other),
            };
            format!(
                "{protocol}{host}{}{path}",
                if path.is_empty() { "" } else { "/" }
            )
        }
        _ => String::new(),
    }
}

fn has_header(headers: &[Header], name: &str) -> bool {
    headers.iter().any(|h| h.name.eq_ignore_ascii_case(name))
}

fn body_of(
    body: Option<&Value>,
    headers: &mut Vec<Header>,
    warnings: &mut Vec<String>,
    at: &str,
) -> Option<String> {
    let body = body?;
    match body.get("mode").and_then(|m| m.as_str()).unwrap_or("raw") {
        "raw" => {
            let raw = text(body.get("raw"));
            let language = body
                .pointer("/options/raw/language")
                .and_then(|l| l.as_str())
                .unwrap_or("");
            let content_type = match language {
                "json" => Some("application/json"),
                "xml" => Some("application/xml"),
                "html" => Some("text/html"),
                "javascript" => Some("application/javascript"),
                _ => None,
            };
            if let Some(ct) = content_type {
                if !raw.is_empty() && !has_header(headers, "content-type") {
                    headers.push(Header::new("Content-Type", ct));
                }
            }
            (!raw.is_empty()).then_some(raw)
        }
        "urlencoded" => {
            let pairs: Vec<String> = body
                .get("urlencoded")
                .and_then(|u| u.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| !i.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false))
                        .map(|i| format!("{}={}", text(i.get("key")), text(i.get("value"))))
                        .collect()
                })
                .unwrap_or_default();
            if !has_header(headers, "content-type") {
                headers.push(Header::new(
                    "Content-Type",
                    "application/x-www-form-urlencoded",
                ));
            }
            Some(pairs.join("&"))
        }
        "graphql" => {
            let query = text(body.pointer("/graphql/query"));
            let variables = text(body.pointer("/graphql/variables"));
            let variables: Value =
                serde_json::from_str(&variables).unwrap_or(Value::Object(Default::default()));
            if !has_header(headers, "content-type") {
                headers.push(Header::new("Content-Type", "application/json"));
            }
            let payload = serde_json::json!({ "query": query, "variables": variables });
            Some(serde_json::to_string_pretty(&payload).unwrap_or_default())
        }
        "formdata" => {
            warnings.push(format!(
                "{at}: multipart form-data bodies are not supported yet; body skipped"
            ));
            None
        }
        "file" => {
            warnings.push(format!(
                "{at}: file bodies are not supported yet; body skipped"
            ));
            None
        }
        _ => None,
    }
}

fn request_of(item: &Value, warnings: &mut Vec<String>, at: &str) -> Option<HttpRequest> {
    let request = item.get("request")?;
    if let Value::String(url) = request {
        return Some(HttpRequest::new(HttpMethod::Get, url.clone()));
    }
    let method_name = request
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("GET");
    let Ok(method) = method_name.parse::<HttpMethod>() else {
        warnings.push(format!(
            "{at}: method {method_name} is not supported; request skipped"
        ));
        return None;
    };
    let mut headers: Vec<Header> = request
        .get("header")
        .and_then(|h| h.as_array())
        .map(|list| {
            list.iter()
                .map(|h| Header {
                    name: text(h.get("key")),
                    value: text(h.get("value")),
                    enabled: !h.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false),
                })
                .filter(|h| !h.name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let auth = parse_auth(request.get("auth"), warnings, at);
    let body = body_of(request.get("body"), &mut headers, warnings, at);
    let url = request.get("url");
    // Postman keeps disabled query params out of `raw`, in `query` with `disabled: true`,
    // and the values of `:name` path segments in `variable`.
    let params_of = |key: &str, keep: fn(&Value) -> bool| -> Vec<Param> {
        url.and_then(|u| u.get(key))
            .and_then(|list| list.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|i| keep(i))
                    .map(|i| Param::new(text(i.get("key")), text(i.get("value"))))
                    .filter(|p| !p.name.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(HttpRequest {
        method,
        url: url_of(url),
        headers,
        body,
        timeout_ms: None,
        disabled_params: params_of("query", |i| {
            i.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false)
        }),
        path_params: params_of("variable", |_| true),
        auth,
    })
}

/// Joins the `exec` lines of an item's `prerequest` and `test` events.
fn scripts_of(item: &Value) -> Scripts {
    let mut scripts = Scripts::default();
    for event in item
        .get("event")
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
    {
        let source = match event.pointer("/script/exec") {
            Some(Value::Array(lines)) => lines
                .iter()
                .map(|l| text(Some(l)))
                .collect::<Vec<_>>()
                .join("\n"),
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        if source.trim().is_empty() {
            continue;
        }
        let target = match event.get("listen").and_then(|l| l.as_str()) {
            Some("prerequest") => &mut scripts.pre,
            Some("test") => &mut scripts.test,
            _ => continue,
        };
        if !target.is_empty() {
            target.push('\n');
        }
        target.push_str(&source);
        if !target.ends_with('\n') {
            target.push('\n');
        }
    }
    scripts
}

fn items(list: &Value, path: &str, warnings: &mut Vec<String>) -> Vec<ImportNode> {
    let mut nodes = Vec::new();
    for item in list.as_array().into_iter().flatten() {
        let name = text(item.get("name"));
        let name = if name.trim().is_empty() {
            "Untitled".to_string()
        } else {
            name
        };
        let at = if path.is_empty() {
            name.clone()
        } else {
            format!("{path} / {name}")
        };
        let own_scripts = scripts_of(item);
        if let Some(children) = item.get("item") {
            let auth = parse_auth(item.get("auth"), warnings, &at);
            nodes.push(ImportNode::Folder {
                name,
                children: items(children, &at, warnings),
                scripts: own_scripts,
                auth,
            });
        } else if let Some(request) = request_of(item, warnings, &at) {
            nodes.push(ImportNode::Request {
                name,
                request,
                scripts: own_scripts,
            });
        }
    }
    nodes
}

pub(super) fn collection(value: &Value) -> Result<ImportPlan, ImportError> {
    let item = value
        .get("item")
        .ok_or_else(|| ImportError::Invalid("Postman collection without items".into()))?;
    let name = text(value.pointer("/info/name"));
    let name = if name.trim().is_empty() {
        "Imported collection".to_string()
    } else {
        name
    };
    let mut warnings = Vec::new();
    let folder_auth = parse_auth(value.get("auth"), &mut warnings, &name);
    let folder_scripts = scripts_of(value);
    let nodes = items(item, "", &mut warnings);

    let variables: Vec<Variable> = value
        .get("variable")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter(|v| !v.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false))
                .map(|v| Variable {
                    name: text(v.get("key")),
                    value: text(v.get("value")),
                    secret: v.get("type").and_then(|t| t.as_str()) == Some("secret"),
                    has_value: false,
                })
                .filter(|v| !v.name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let environments = if variables.is_empty() {
        Vec::new()
    } else {
        vec![Environment {
            id: String::new(),
            name: format!("{name} variables"),
            variables,
        }]
    };

    Ok(ImportPlan {
        format: ImportFormat::PostmanCollection,
        folder: Some(name),
        folder_scripts,
        folder_auth,
        nodes,
        environments,
        warnings,
    })
}

pub(super) fn environment(value: &Value) -> Result<ImportPlan, ImportError> {
    let name = text(value.get("name"));
    let name = if name.trim().is_empty() {
        "Imported environment".to_string()
    } else {
        name
    };
    let mut warnings = Vec::new();
    let mut variables: Vec<Variable> = Vec::new();
    for v in value
        .get("values")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let key = text(v.get("key"));
        if key.is_empty() {
            continue;
        }
        if variables.iter().any(|existing| existing.name == key) {
            warnings.push(format!("{name}: duplicated variable '{key}' skipped"));
            continue;
        }
        variables.push(Variable {
            name: key,
            value: text(v.get("value")),
            secret: v.get("type").and_then(|t| t.as_str()) == Some("secret"),
            has_value: false,
        });
    }
    Ok(ImportPlan {
        format: ImportFormat::PostmanEnvironment,
        folder: None,
        folder_scripts: Default::default(),
        folder_auth: None,
        nodes: Vec::new(),
        environments: vec![Environment {
            id: String::new(),
            name,
            variables,
        }],
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    const COLLECTION: &str = r#"{
      "info": {"name": "Store API", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
      "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{token}}", "type": "string"}]},
      "event": [{"listen": "prerequest", "script": {"exec": ["pm.environment.set('a', 1);"]}}],
      "item": [
        {"name": "Auth", "auth": {"type": "noauth"}, "item": [
          {"name": "Login", "request": {
            "method": "POST",
            "header": [{"key": "Accept", "value": "application/json"}, {"key": "X-Old", "value": "1", "disabled": true}],
            "url": {"raw": "{{base}}/login", "host": ["{{base}}"], "path": ["login"]},
            "body": {"mode": "raw", "raw": "{\"user\": \"{{user}}\"}", "options": {"raw": {"language": "json"}}}
          }, "event": [{"listen": "test", "script": {"exec": ["pm.test('ok', () => {});"]}}]}
        ]},
        {"name": "Orders", "item": [
          {"name": "Create", "request": {"method": "POST", "url": "{{base}}/orders",
            "body": {"mode": "urlencoded", "urlencoded": [{"key": "a", "value": "1"}, {"key": "b", "value": "2", "disabled": true}]}}},
          {"name": "Query", "request": {"method": "POST", "url": "{{base}}/graphql",
            "body": {"mode": "graphql", "graphql": {"query": "{ orders { id } }", "variables": "{\"n\": 1}"}}}},
          {"name": "Upload", "request": {"method": "POST", "url": "{{base}}/files", "body": {"mode": "formdata", "formdata": []}}},
          {"name": "Weird", "request": {"method": "PURGE", "url": "{{base}}/cache"}},
          {"name": "Api key", "request": {"method": "GET", "url": "{{base}}/k",
            "auth": {"type": "apikey", "apikey": [{"key": "key", "value": "X-Key"}, {"key": "value", "value": "{{key}}"}]}}}
        ]}
      ],
      "variable": [{"key": "base", "value": "https://shop.test"}, {"key": "token", "value": "abc", "type": "secret"}]
    }"#;

    fn requests(nodes: &[ImportNode]) -> Vec<(String, HttpRequest)> {
        let mut out = Vec::new();
        for node in nodes {
            match node {
                ImportNode::Folder { children, .. } => out.extend(requests(children)),
                ImportNode::Request { name, request, .. } => {
                    out.push((name.clone(), request.clone()))
                }
            }
        }
        out
    }

    #[test]
    fn imports_a_collection() {
        let plan = parse(COLLECTION).unwrap();
        assert_eq!(plan.format, ImportFormat::PostmanCollection);
        assert_eq!(plan.folder.as_deref(), Some("Store API"));
        let all = requests(&plan.nodes);
        let names: Vec<_> = all.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Login", "Create", "Query", "Upload", "Api key"]);

        let login = &all[0].1;
        assert_eq!(login.method, HttpMethod::Post);
        assert_eq!(login.url, "{{base}}/login");
        assert_eq!(login.auth, None, "inherits the folder's");
        assert!(login
            .headers
            .iter()
            .any(|h| h.name == "X-Old" && !h.enabled));
        assert!(login
            .headers
            .iter()
            .any(|h| h.name == "Content-Type" && h.value == "application/json"));
        assert_eq!(login.body.as_deref(), Some(r#"{"user": "{{user}}"}"#));

        assert_eq!(
            plan.folder_auth,
            Some(Auth::Bearer {
                token: "{{token}}".into()
            })
        );
        let create = &all[1].1;
        assert_eq!(create.auth, None);
        assert!(!create.headers.iter().any(|h| h.name == "Authorization"));
        assert_eq!(create.body.as_deref(), Some("a=1"));

        let query = &all[2].1;
        let payload: Value = serde_json::from_str(query.body.as_deref().unwrap()).unwrap();
        assert_eq!(payload["query"], "{ orders { id } }");
        assert_eq!(payload["variables"]["n"], 1);

        assert_eq!(all[3].1.body, None);
        assert_eq!(
            all[4].1.auth,
            Some(Auth::ApiKey {
                key: "X-Key".into(),
                value: "{{key}}".into(),
                location: ApiKeyLocation::Header
            })
        );

        assert_eq!(plan.folder_scripts.pre, "pm.environment.set('a', 1);\n");
        let ImportNode::Folder { children, auth, .. } = &plan.nodes[0] else {
            panic!()
        };
        assert_eq!(auth, &Some(Auth::None), "noauth folder");
        let ImportNode::Request { scripts, .. } = &children[0] else {
            panic!()
        };
        assert_eq!(scripts.test, "pm.test('ok', () => {});\n");
        assert!(plan.warnings.iter().any(|w| w.contains("form-data")));
        assert!(plan.warnings.iter().any(|w| w.contains("PURGE")));

        let env = &plan.environments[0];
        assert_eq!(env.name, "Store API variables");
        assert!(env
            .variables
            .iter()
            .any(|v| v.name == "token" && v.secret && v.value == "abc"));
    }

    #[test]
    fn keeps_disabled_query_params_and_path_variables() {
        let plan = parse(
            r#"{"info": {"name": "x"}, "item": [{"name": "a", "request": {"method": "GET",
                "url": {"raw": "{{base}}/orders/:orderId?page=1",
                        "query": [{"key": "page", "value": "1"}, {"key": "debug", "value": "true", "disabled": true}],
                        "variable": [{"key": "orderId", "value": "{{orderId}}"}]}}}]}"#,
        )
        .unwrap();
        let (_, request) = &requests(&plan.nodes)[0];
        assert_eq!(request.url, "{{base}}/orders/:orderId?page=1");
        assert_eq!(request.disabled_params, vec![Param::new("debug", "true")]);
        assert_eq!(
            request.path_params,
            vec![Param::new("orderId", "{{orderId}}")]
        );
    }

    #[test]
    fn imports_an_environment_with_secrets() {
        let plan = parse(
            r#"{"id": "1", "name": "Local", "_postman_variable_scope": "environment", "values": [
                {"key": "base", "value": "http://localhost", "type": "default", "enabled": true},
                {"key": "pwd", "value": "s3cr3t", "type": "secret", "enabled": true},
                {"key": "base", "value": "dup", "type": "default", "enabled": true}
            ]}"#,
        )
        .unwrap();
        assert_eq!(plan.format, ImportFormat::PostmanEnvironment);
        let env = &plan.environments[0];
        assert_eq!(env.name, "Local");
        assert_eq!(env.variables.len(), 2);
        assert!(env.variables[1].secret);
        assert!(plan.warnings[0].contains("duplicated"));
    }

    #[test]
    fn builds_urls_from_parts_and_encodes_basic_auth() {
        let plan = parse(
            r#"{"info": {"name": "x"}, "item": [{"name": "a", "request": {"method": "GET",
                "url": {"protocol": "https", "host": ["api", "example", "com"], "path": ["v1", "users"]},
                "auth": {"type": "basic", "basic": [{"key": "username", "value": "ana"}, {"key": "password", "value": "pw"}]}}}]}"#,
        )
        .unwrap();
        let (_, request) = &requests(&plan.nodes)[0];
        assert_eq!(request.url, "https://api.example.com/v1/users");
        assert_eq!(
            request.auth,
            Some(Auth::Basic {
                username: "ana".into(),
                password: "pw".into()
            })
        );
    }
}
