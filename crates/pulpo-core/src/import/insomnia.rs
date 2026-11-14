//! Insomnia exports: v4 (JSON, a flat list of resources) and v5 (YAML, a tree).

use super::common::{
    add_query, flag, form_body, graphql_body, list, method_of, pulpo_vars, raw_body, text,
};
use super::{ImportError, ImportFormat, ImportNode, ImportPlan};
use crate::collection::Scripts;
use crate::environment::{Environment, Variable};
use crate::http::{ApiKeyLocation, Auth, Header, HttpRequest};
use crate::oauth::{Grant, OAuth2};
use crate::signing::{AwsV4, DigestAuth, Hawk, HawkAlgorithm, Ntlm};
use serde_json::Value;

pub(super) fn is_insomnia(value: &Value) -> bool {
    value.get("_type").and_then(|t| t.as_str()) == Some("export")
        || value
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.starts_with("collection.insomnia.rest"))
}

fn vars(value: Option<&Value>) -> String {
    pulpo_vars(&text(value))
}

/// Insomnia scripts use `insomnia.*`, which mirrors Postman's `pm.*`.
fn script(code: &str) -> String {
    if code.trim().is_empty() {
        String::new()
    } else {
        code.replace("insomnia.", "pm.")
    }
}

fn auth_of(auth: Option<&Value>, warnings: &mut Vec<String>, at: &str) -> Option<Auth> {
    let auth = auth.filter(|a| a.as_object().is_some_and(|o| !o.is_empty()))?;
    if flag(auth.get("disabled")) {
        return Some(Auth::None);
    }
    let get = |key: &str| vars(auth.get(key));
    let kind = text(auth.get("type"));
    Some(match kind.as_str() {
        "none" => Auth::None,
        "inherit" => return None,
        "bearer" => Auth::Bearer {
            token: get("token"),
        },
        "basic" => Auth::Basic {
            username: get("username"),
            password: get("password"),
        },
        "apikey" => Auth::ApiKey {
            key: get("key"),
            value: get("value"),
            location: if text(auth.get("addTo")) == "queryParams" {
                ApiKeyLocation::Query
            } else {
                ApiKeyLocation::Header
            },
        },
        "digest" => Auth::Digest(DigestAuth {
            username: get("username"),
            password: get("password"),
        }),
        "ntlm" => Auth::Ntlm(Ntlm {
            username: get("username"),
            password: get("password"),
            ..Default::default()
        }),
        "iam" => Auth::AwsV4(AwsV4 {
            access_key: get("accessKeyId"),
            secret_key: get("secretAccessKey"),
            session_token: get("sessionToken"),
            region: get("region"),
            service: get("service"),
        }),
        "hawk" => Auth::Hawk(Hawk {
            id: get("id"),
            key: get("key"),
            algorithm: if get("algorithm") == "sha1" {
                HawkAlgorithm::Sha1
            } else {
                HawkAlgorithm::Sha256
            },
            ext: get("ext"),
            payload: flag(auth.get("validatePayload")),
        }),
        "oauth2" => {
            let grant = match get("grantType").as_str() {
                "client_credentials" => Grant::ClientCredentials,
                "password" => Grant::Password,
                "authorization_code" | "" => Grant::AuthorizationCode,
                other => {
                    warnings.push(format!(
                        "{at}: OAuth 2.0 '{other}' grant is not supported; it inherits from its folder"
                    ));
                    return None;
                }
            };
            Auth::OAuth2(OAuth2 {
                grant,
                auth_url: get("authorizationUrl"),
                token_url: get("accessTokenUrl"),
                client_id: get("clientId"),
                client_secret: get("clientSecret"),
                scope: get("scope"),
                username: get("username"),
                password: get("password"),
                pkce: flag(auth.get("usePkce")) || grant != Grant::AuthorizationCode,
                ..Default::default()
            })
        }
        other => {
            warnings.push(format!(
                "{at}: '{other}' auth is not supported yet; it inherits from its folder"
            ));
            return None;
        }
    })
}

fn request_of(item: &Value, warnings: &mut Vec<String>) -> ImportNode {
    let name = text(item.get("name"));
    let name = if name.trim().is_empty() {
        "Request".to_string()
    } else {
        name
    };
    let at = name.clone();
    let mut request = HttpRequest::new(
        method_of(&text(item.get("method")), warnings, &at),
        vars(item.get("url")),
    );
    for header in list(item.get("headers")) {
        let name = vars(header.get("name"));
        if name.is_empty() {
            continue;
        }
        let mut h = Header::new(name, vars(header.get("value")));
        h.enabled = !flag(header.get("disabled"));
        request.headers.push(h);
    }
    let params = list(item.get("parameters"))
        .iter()
        .map(|p| {
            (
                vars(p.get("name")),
                vars(p.get("value")),
                !flag(p.get("disabled")),
            )
        })
        .collect();
    add_query(&mut request, params);

    if let Some(body) = item.get("body") {
        let mime = text(body.get("mimeType"));
        let mime_base = mime.split(';').next().unwrap_or_default().trim();
        match mime_base {
            "application/x-www-form-urlencoded" | "multipart/form-data" => {
                let fields = list(body.get("params"))
                    .iter()
                    .map(|p| {
                        let file = text(p.get("type")) == "file";
                        let value = if file {
                            text(p.get("fileName"))
                        } else {
                            vars(p.get("value"))
                        };
                        (vars(p.get("name")), value, file, !flag(p.get("disabled")))
                    })
                    .collect();
                form_body(
                    &mut request,
                    mime_base == "multipart/form-data",
                    fields,
                    warnings,
                    &at,
                );
            }
            "application/graphql" => {
                let payload: Value =
                    serde_json::from_str(&text(body.get("text"))).unwrap_or(Value::Null);
                let variables = payload
                    .get("variables")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                graphql_body(&mut request, &text(payload.get("query")), &variables);
            }
            "application/octet-stream" => {
                let file = text(body.get("fileName"));
                if !file.is_empty() {
                    request.body_file = Some(super::workspace_path(&file, warnings, &at));
                }
            }
            _ => raw_body(&mut request, vars(body.get("text")), Some(mime_base)),
        }
    }
    request.auth = auth_of(item.get("authentication"), warnings, &at);
    let (pre, test) = match item.get("scripts") {
        // v5
        Some(s) => (text(s.get("preRequest")), text(s.get("afterResponse"))),
        // v4
        None => (
            text(item.get("preRequestScript")),
            text(item.get("afterResponseScript")),
        ),
    };
    ImportNode::Request {
        name,
        request,
        scripts: Scripts {
            pre: script(&pre),
            test: script(&test),
        },
    }
}

fn folder_of(item: &Value, children: Vec<ImportNode>, warnings: &mut Vec<String>) -> ImportNode {
    let name = text(item.get("name"));
    let name = if name.trim().is_empty() {
        "Folder".to_string()
    } else {
        name
    };
    let auth = auth_of(item.get("authentication"), warnings, &name);
    let scripts = match item.get("scripts") {
        Some(s) => Scripts {
            pre: script(&text(s.get("preRequest"))),
            test: script(&text(s.get("afterResponse"))),
        },
        None => Scripts {
            pre: script(&text(item.get("preRequestScript"))),
            test: script(&text(item.get("afterResponseScript"))),
        },
    };
    let has_vars = item
        .get("environment")
        .and_then(|e| e.as_object())
        .is_some_and(|o| !o.is_empty());
    if has_vars {
        warnings.push(format!(
            "{name}: folder variables are not supported; add them to an environment"
        ));
    }
    ImportNode::Folder {
        name,
        children,
        scripts,
        auth,
    }
}

fn environment_of(name: &str, data: Option<&Value>, base: &[Variable]) -> Environment {
    let mut variables: Vec<Variable> = base.to_vec();
    if let Some(data) = data.and_then(|d| d.as_object()) {
        for (key, value) in data {
            let value = match value {
                Value::String(s) => pulpo_vars(s),
                Value::Null => String::new(),
                // Nested objects are reached as `_.a.b` in Insomnia; keep them as JSON.
                other => other.to_string(),
            };
            match variables.iter_mut().find(|v| &v.name == key) {
                Some(existing) => existing.value = value,
                None => variables.push(Variable {
                    name: key.clone(),
                    value,
                    secret: false,
                    has_value: false,
                }),
            }
        }
    }
    Environment {
        id: String::new(),
        name: name.to_string(),
        variables,
    }
}

/// Base environment plus each sub-environment (which overrides the base).
fn environments(
    base_name: &str,
    base: Option<&Value>,
    subs: Vec<(String, Option<&Value>)>,
) -> Vec<Environment> {
    let base_env = environment_of(base_name, base, &[]);
    if subs.is_empty() {
        return if base_env.variables.is_empty() {
            Vec::new()
        } else {
            vec![base_env]
        };
    }
    subs.into_iter()
        .map(|(name, data)| environment_of(&name, data, &base_env.variables))
        .collect()
}

fn v4(value: &Value) -> Result<ImportPlan, ImportError> {
    let resources = list(value.get("resources"));
    let mut warnings = Vec::new();
    let workspace = resources
        .iter()
        .find(|r| text(r.get("_type")) == "workspace");
    let workspace_id = workspace.map(|w| text(w.get("_id"))).unwrap_or_default();
    let name = workspace
        .map(|w| text(w.get("name")))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Insomnia".into());

    fn children_of(
        parent: &str,
        resources: &[Value],
        warnings: &mut Vec<String>,
    ) -> Vec<ImportNode> {
        let mut items: Vec<&Value> = resources
            .iter()
            .filter(|r| text(r.get("parentId")) == parent)
            .collect();
        items.sort_by(|a, b| {
            let key = |v: &Value| v.get("metaSortKey").and_then(|k| k.as_f64()).unwrap_or(0.0);
            key(a).total_cmp(&key(b))
        });
        let mut nodes = Vec::new();
        for item in items {
            match text(item.get("_type")).as_str() {
                "request" => nodes.push(request_of(item, warnings)),
                "request_group" => {
                    let children = children_of(&text(item.get("_id")), resources, warnings);
                    nodes.push(folder_of(item, children, warnings));
                }
                kind @ ("grpc_request" | "websocket_request" | "graphql_request") => {
                    warnings.push(format!(
                        "{}: {kind} is not supported yet; skipped",
                        text(item.get("name"))
                    ))
                }
                _ => {}
            }
        }
        nodes
    }

    let nodes = children_of(&workspace_id, resources, &mut warnings);
    let base = resources
        .iter()
        .find(|r| text(r.get("_type")) == "environment" && text(r.get("parentId")) == workspace_id);
    let subs = base
        .map(|b| text(b.get("_id")))
        .map(|base_id| {
            resources
                .iter()
                .filter(|r| {
                    text(r.get("_type")) == "environment" && text(r.get("parentId")) == base_id
                })
                .map(|r| (text(r.get("name")), r.get("data")))
                .collect()
        })
        .unwrap_or_default();
    Ok(ImportPlan {
        format: ImportFormat::Insomnia,
        folder: Some(name.clone()),
        folder_scripts: Scripts::default(),
        folder_auth: None,
        nodes,
        environments: environments(
            &format!("{name} base"),
            base.and_then(|b| b.get("data")),
            subs,
        ),
        warnings,
    })
}

fn v5(value: &Value) -> Result<ImportPlan, ImportError> {
    let mut warnings = Vec::new();
    let name = text(value.get("name"));
    let name = if name.trim().is_empty() {
        "Insomnia".to_string()
    } else {
        name
    };
    fn items(list_value: Option<&Value>, warnings: &mut Vec<String>) -> Vec<ImportNode> {
        list(list_value)
            .iter()
            .map(|item| {
                if item.get("children").is_some() {
                    let children = items(item.get("children"), warnings);
                    folder_of(item, children, warnings)
                } else {
                    request_of(item, warnings)
                }
            })
            .collect()
    }
    let nodes = items(value.get("collection"), &mut warnings);
    let envs = value.get("environments");
    let subs = list(envs.and_then(|e| e.get("subEnvironments")))
        .iter()
        .map(|s| (text(s.get("name")), s.get("data")))
        .collect();
    let base_name = envs
        .map(|e| text(e.get("name")))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| format!("{name} base"));
    let folder_auth = auth_of(value.get("authentication"), &mut warnings, &name);
    Ok(ImportPlan {
        format: ImportFormat::Insomnia,
        folder: Some(name),
        folder_scripts: Scripts::default(),
        folder_auth,
        nodes,
        environments: environments(&base_name, envs.and_then(|e| e.get("data")), subs),
        warnings,
    })
}

pub(super) fn plan(value: &Value) -> Result<ImportPlan, ImportError> {
    let mut plan = if value.get("resources").is_some() {
        v4(value)?
    } else {
        v5(value)?
    };
    if plan.nodes.is_empty() && plan.environments.is_empty() {
        return Err(ImportError::Invalid(
            "the Insomnia export has no requests or environments".into(),
        ));
    }
    plan.warnings.sort();
    plan.warnings.dedup();
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    #[test]
    fn imports_a_v4_export() {
        let export = r#"{"_type":"export","__export_format":4,"resources":[
          {"_id":"wrk_1","_type":"workspace","name":"Shop"},
          {"_id":"fld_1","_type":"request_group","parentId":"wrk_1","name":"Users","metaSortKey":1,
           "authentication":{"type":"bearer","token":"{{ _.token }}"}},
          {"_id":"req_2","_type":"request","parentId":"fld_1","name":"Create","method":"POST","metaSortKey":2,
           "url":"{{ _.base }}/users","body":{"mimeType":"application/json","text":"{\"a\":1}"},
           "headers":[{"name":"X-Off","value":"1","disabled":true}],
           "parameters":[{"name":"dry","value":"true"}],"authentication":{},
           "afterResponseScript":"insomnia.test('ok', () => insomnia.expect(insomnia.response.code).to.eql(201));"},
          {"_id":"req_1","_type":"request","parentId":"fld_1","name":"List","method":"GET","metaSortKey":1,
           "url":"{{ _.base }}/users","authentication":{"type":"none"}},
          {"_id":"env_1","_type":"environment","parentId":"wrk_1","name":"Base","data":{"base":"http://localhost"}},
          {"_id":"env_2","_type":"environment","parentId":"env_1","name":"Prod","data":{"base":"https://api.shop","token":"t"}}
        ]}"#;
        let plan = parse(export).unwrap();
        assert_eq!(plan.format, ImportFormat::Insomnia);
        assert_eq!(plan.folder.as_deref(), Some("Shop"));
        let ImportNode::Folder {
            name,
            auth,
            children,
            ..
        } = &plan.nodes[0]
        else {
            panic!("a folder")
        };
        assert_eq!(name, "Users");
        assert_eq!(
            auth,
            &Some(Auth::Bearer {
                token: "{{token}}".into()
            })
        );
        let ImportNode::Request { name, request, .. } = &children[0] else {
            panic!()
        };
        assert_eq!(name, "List", "sorted by metaSortKey");
        assert_eq!(request.auth, Some(Auth::None));
        let ImportNode::Request {
            request, scripts, ..
        } = &children[1]
        else {
            panic!()
        };
        assert_eq!(request.url, "{{base}}/users?dry=true");
        assert_eq!(request.body.as_deref(), Some("{\"a\":1}"));
        assert!(request.headers.iter().any(|h| h.name == "Content-Type"));
        assert!(request
            .headers
            .iter()
            .any(|h| h.name == "X-Off" && !h.enabled));
        assert_eq!(request.auth, None, "an empty auth inherits");
        assert!(scripts.test.starts_with("pm.test("), "{}", scripts.test);
        let prod = &plan.environments[0];
        assert_eq!(prod.name, "Prod");
        let base = prod.variables.iter().find(|v| v.name == "base").unwrap();
        assert_eq!(base.value, "https://api.shop");
        assert_eq!(prod.variables.len(), 2);
    }

    #[test]
    fn imports_a_v5_yaml_export() {
        let export = "type: collection.insomnia.rest/5.0\nname: Shop\ncollection:\n  - name: Users\n    children:\n      - url: \"{{ _.base }}/users\"\n        name: List\n        method: GET\n        authentication:\n          type: basic\n          username: ana\n          password: \"{{ _.pw }}\"\n  - url: https://x/health\n    name: Health\n    method: GET\nenvironments:\n  name: Base Environment\n  data:\n    base: http://localhost\n";
        let plan = parse(export).unwrap();
        assert_eq!(plan.format, ImportFormat::Insomnia);
        assert_eq!(plan.nodes.len(), 2);
        let ImportNode::Folder { children, .. } = &plan.nodes[0] else {
            panic!()
        };
        let ImportNode::Request { request, .. } = &children[0] else {
            panic!()
        };
        assert_eq!(
            request.auth,
            Some(Auth::Basic {
                username: "ana".into(),
                password: "{{pw}}".into()
            })
        );
        assert_eq!(plan.environments[0].name, "Base Environment");
    }
}
