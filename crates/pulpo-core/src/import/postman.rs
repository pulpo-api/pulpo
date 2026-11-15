//! Postman collection v2.0/v2.1 and Postman environment files.

use super::{workspace_path, ImportError, ImportFormat, ImportNode, ImportPlan, ImportedBody};
use crate::collection::Scripts;
use crate::environment::{Environment, Variable};
use crate::http::{ApiKeyLocation, Auth, Form, FormField, FormKind, Header, HttpRequest, Param};
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
        "oauth2" => {
            use crate::oauth::{ClientAuth, Grant, OAuth2};
            let get = |key: &str| kv(auth.get("oauth2"), key);
            let grant_type = get("grant_type");
            let grant = match grant_type.as_str() {
                "client_credentials" => Grant::ClientCredentials,
                "password_credentials" | "password" => Grant::Password,
                "authorization_code" | "authorization_code_with_pkce" | "" => {
                    Grant::AuthorizationCode
                }
                other => {
                    warnings.push(format!(
                        "{at}: OAuth 2.0 '{other}' grant is not supported; it inherits from its folder"
                    ));
                    return None;
                }
            };
            // Postman's own callback page cannot reach Pulpo: use the loopback default.
            let redirect_uri = get("redirect_uri");
            let redirect_uri = if redirect_uri.contains("oauth.pstmn.io") {
                String::new()
            } else {
                redirect_uri
            };
            let prefix = get("headerPrefix");
            Some(Auth::OAuth2(OAuth2 {
                grant,
                auth_url: get("authUrl"),
                token_url: get("accessTokenUrl"),
                client_id: get("clientId"),
                client_secret: get("clientSecret"),
                scope: get("scope"),
                username: get("username"),
                password: get("password"),
                redirect_uri,
                pkce: grant_type != "authorization_code",
                client_auth: if get("client_authentication") == "body" {
                    ClientAuth::Body
                } else {
                    ClientAuth::Header
                },
                header_prefix: if prefix.is_empty() {
                    "Bearer".into()
                } else {
                    prefix
                },
                revoke_url: String::new(),
            }))
        }
        "digest" => {
            let get = |key: &str| kv(auth.get("digest"), key);
            Some(Auth::Digest(crate::signing::DigestAuth {
                username: get("username"),
                password: get("password"),
            }))
        }
        "awsv4" => {
            let get = |key: &str| kv(auth.get("awsv4"), key);
            let service = get("service");
            Some(Auth::AwsV4(crate::signing::AwsV4 {
                access_key: get("accessKey"),
                secret_key: get("secretKey"),
                session_token: get("sessionToken"),
                region: get("region"),
                service: if service.is_empty() {
                    "execute-api".into()
                } else {
                    service
                },
            }))
        }
        "hawk" => {
            use crate::signing::{Hawk, HawkAlgorithm};
            let get = |key: &str| kv(auth.get("hawk"), key);
            Some(Auth::Hawk(Hawk {
                id: get("authId"),
                key: get("authKey"),
                algorithm: if get("algorithm").eq_ignore_ascii_case("sha1") {
                    HawkAlgorithm::Sha1
                } else {
                    HawkAlgorithm::Sha256
                },
                ext: get("extraData"),
                payload: get("includePayloadHash") == "true",
            }))
        }
        "ntlm" => {
            let get = |key: &str| kv(auth.get("ntlm"), key);
            Some(Auth::Ntlm(crate::signing::Ntlm {
                username: get("username"),
                password: get("password"),
                domain: get("domain"),
                workstation: get("workstation"),
            }))
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
) -> ImportedBody {
    let Some(body) = body else {
        return ImportedBody::default();
    };
    let raw = |text: String| ImportedBody {
        raw: (!text.is_empty()).then_some(text),
        ..Default::default()
    };
    let mode = body.get("mode").and_then(|m| m.as_str()).unwrap_or("raw");
    match mode {
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
            ImportedBody {
                raw: (!raw.is_empty()).then_some(raw),
                ..Default::default()
            }
        }
        "urlencoded" | "formdata" => {
            let urlencoded = mode == "urlencoded";
            let fields = body
                .get(mode)
                .and_then(|u| u.as_array())
                .map(|items| {
                    items
                        .iter()
                        .map(|i| {
                            let is_file = i.get("type").and_then(|t| t.as_str()) == Some("file");
                            let value = if is_file {
                                // `src` is a path (or a list of paths) on the author's machine.
                                let src = match i.get("src") {
                                    Some(Value::Array(list)) => text(list.first()),
                                    other => text(other),
                                };
                                workspace_path(&src, warnings, at)
                            } else {
                                text(i.get("value"))
                            };
                            FormField {
                                name: text(i.get("key")),
                                value,
                                file: is_file && !urlencoded,
                                enabled: !i
                                    .get("disabled")
                                    .and_then(|d| d.as_bool())
                                    .unwrap_or(false),
                            }
                        })
                        .filter(|f| !f.name.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            ImportedBody {
                form: Some(Form {
                    kind: if urlencoded {
                        FormKind::Urlencoded
                    } else {
                        FormKind::Multipart
                    },
                    fields,
                }),
                ..Default::default()
            }
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
            raw(serde_json::to_string_pretty(&payload).unwrap_or_default())
        }
        "file" => ImportedBody {
            file: Some(workspace_path(
                &text(body.pointer("/file/src")),
                warnings,
                at,
            )),
            ..Default::default()
        },
        _ => ImportedBody::default(),
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
        body: body.raw,
        form: body.form,
        body_file: body.file,
        settings: Default::default(),
        // Postman's per-request "Disable cookie jar" setting.
        cookies: !item
            .pointer("/protocolProfileBehavior/disableCookies")
            .and_then(|d| d.as_bool())
            .unwrap_or(false),
        graphql: None,
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
          {"name": "Upload", "request": {"method": "POST", "url": "{{base}}/files", "body": {"mode": "formdata", "formdata": [
            {"key": "title", "value": "x", "type": "text"},
            {"key": "photo", "src": "/Users/ana/photo.png", "type": "file"}]}}},
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
        assert_eq!(create.body, None);
        assert_eq!(
            create.form,
            Some(Form {
                kind: FormKind::Urlencoded,
                fields: vec![
                    FormField::text("a", "1"),
                    FormField {
                        enabled: false,
                        ..FormField::text("b", "2")
                    },
                ],
            })
        );

        let query = &all[2].1;
        let payload: Value = serde_json::from_str(query.body.as_deref().unwrap()).unwrap();
        assert_eq!(payload["query"], "{ orders { id } }");
        assert_eq!(payload["variables"]["n"], 1);

        assert_eq!(
            all[3].1.form.as_ref().unwrap().fields,
            vec![
                FormField::text("title", "x"),
                FormField::file("photo", "files/photo.png")
            ]
        );
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
        assert!(plan
            .warnings
            .iter()
            .any(|w| w.contains("copy it to files/photo.png")));
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
    fn keeps_the_disable_cookies_setting() {
        let plan = parse(
            r#"{"info": {"name": "x"}, "item": [
                {"name": "a", "request": {"method": "GET", "url": "https://x.test"},
                 "protocolProfileBehavior": {"disableCookies": true}},
                {"name": "b", "request": {"method": "GET", "url": "https://x.test"}}]}"#,
        )
        .unwrap();
        let all = requests(&plan.nodes);
        assert!(!all[0].1.cookies);
        assert!(all[1].1.cookies);
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

    #[test]
    fn imports_oauth2_auth() {
        let auth: Value = serde_json::from_str(
            r#"{"type": "oauth2", "oauth2": [
                {"key": "grant_type", "value": "authorization_code_with_pkce"},
                {"key": "authUrl", "value": "{{idp}}/authorize"},
                {"key": "accessTokenUrl", "value": "{{idp}}/token"},
                {"key": "clientId", "value": "{{clientId}}"},
                {"key": "scope", "value": "openid"},
                {"key": "redirect_uri", "value": "https://oauth.pstmn.io/v1/callback"},
                {"key": "client_authentication", "value": "body"}
            ]}"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let Some(Auth::OAuth2(config)) = parse_auth(Some(&auth), &mut warnings, "x") else {
            panic!("expected OAuth 2.0");
        };
        assert!(warnings.is_empty());
        assert_eq!(config.grant, crate::oauth::Grant::AuthorizationCode);
        assert_eq!(config.token_url, "{{idp}}/token");
        assert!(config.pkce);
        assert_eq!(
            config.redirect_uri, "",
            "Postman's callback cannot reach Pulpo"
        );
        assert_eq!(config.client_auth, crate::oauth::ClientAuth::Body);
        assert_eq!(config.header_prefix, "Bearer");
    }

    #[test]
    fn imports_digest_aws_hawk_and_ntlm_auth() {
        use crate::signing::{AwsV4, DigestAuth, Hawk, HawkAlgorithm, Ntlm};
        let parse = |json: &str| {
            let mut warnings = Vec::new();
            let auth = parse_auth(
                Some(&serde_json::from_str(json).unwrap()),
                &mut warnings,
                "x",
            );
            assert!(warnings.is_empty(), "{warnings:?}");
            auth.unwrap()
        };
        assert_eq!(
            parse(
                r#"{"type": "digest", "digest": [{"key": "username", "value": "ana"}, {"key": "password", "value": "{{pw}}"}]}"#
            ),
            Auth::Digest(DigestAuth {
                username: "ana".into(),
                password: "{{pw}}".into()
            })
        );
        assert_eq!(
            parse(
                r#"{"type": "awsv4", "awsv4": [{"key": "accessKey", "value": "AK"}, {"key": "secretKey", "value": "SK"}, {"key": "region", "value": "eu-west-1"}]}"#
            ),
            Auth::AwsV4(AwsV4 {
                access_key: "AK".into(),
                secret_key: "SK".into(),
                session_token: String::new(),
                region: "eu-west-1".into(),
                service: "execute-api".into(),
            })
        );
        assert_eq!(
            parse(
                r#"{"type": "hawk", "hawk": [{"key": "authId", "value": "id"}, {"key": "authKey", "value": "k"}, {"key": "algorithm", "value": "sha1"}, {"key": "includePayloadHash", "value": true}]}"#
            ),
            Auth::Hawk(Hawk {
                id: "id".into(),
                key: "k".into(),
                algorithm: HawkAlgorithm::Sha1,
                ext: String::new(),
                payload: true,
            })
        );
        assert_eq!(
            parse(
                r#"{"type": "ntlm", "ntlm": [{"key": "username", "value": "ana"}, {"key": "domain", "value": "CORP"}]}"#
            ),
            Auth::Ntlm(Ntlm {
                username: "ana".into(),
                password: String::new(),
                domain: "CORP".into(),
                workstation: String::new(),
            })
        );
    }
}
