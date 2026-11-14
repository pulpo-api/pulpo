//! Bruno: a collection folder of `.bru` files (pick its `bruno.json`) or a JSON export.

use super::common::{
    add_query, flag, foreign_script, form_body, graphql_body, list, method_of, raw_body, text,
};
use super::{ImportError, ImportFormat, ImportNode, ImportPlan};
use crate::collection::Scripts;
use crate::environment::{Environment, Variable};
use crate::http::{ApiKeyLocation, Auth, Header, HttpRequest, Param};
use crate::signing::{AwsV4, DigestAuth, Ntlm};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// What a Bruno request says, from a `.bru` file or the JSON export.
#[derive(Default)]
struct BruRequest {
    name: String,
    seq: f64,
    method: String,
    url: String,
    headers: Vec<(String, String, bool)>,
    query: Vec<(String, String, bool)>,
    path: Vec<(String, String)>,
    /// json, text, xml, formUrlEncoded, multipartForm, graphql or none.
    body_mode: String,
    body_text: String,
    graphql_vars: String,
    /// (name, value, is_file, enabled)
    form: Vec<(String, String, bool, bool)>,
    auth_mode: String,
    auth: BTreeMap<String, String>,
    pre: String,
    post: String,
    tests: String,
}

fn auth_of(
    mode: &str,
    a: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
    at: &str,
) -> Option<Auth> {
    let get = |key: &str| a.get(key).cloned().unwrap_or_default();
    Some(match mode {
        "" | "inherit" => return None,
        "none" => Auth::None,
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
            location: if get("placement").eq_ignore_ascii_case("queryparams") {
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
            domain: get("domain"),
            workstation: String::new(),
        }),
        "awsv4" => Auth::AwsV4(AwsV4 {
            access_key: get("accessKeyId"),
            secret_key: get("secretAccessKey"),
            session_token: get("sessionToken"),
            region: get("region"),
            service: get("service"),
        }),
        other => {
            warnings.push(format!(
                "{at}: '{other}' auth is not supported yet; it inherits from its folder"
            ));
            return None;
        }
    })
}

fn scripts(pre: &str, post: &str, tests: &str) -> Scripts {
    let test = [post, tests]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    Scripts {
        pre: foreign_script("Bruno", "bru/req/res", pre),
        test: foreign_script("Bruno", "bru/req/res", &test),
    }
}

fn node_of(bru: BruRequest, warnings: &mut Vec<String>) -> ImportNode {
    let name = if bru.name.trim().is_empty() {
        "Request".to_string()
    } else {
        bru.name.clone()
    };
    let at = name.clone();
    // The URL already holds the enabled query params; keep only the disabled ones.
    let mut request = HttpRequest::new(method_of(&bru.method, warnings, &at), bru.url.clone());
    for (name, value, enabled) in bru.headers {
        let mut h = Header::new(name, value);
        h.enabled = enabled;
        request.headers.push(h);
    }
    let disabled: Vec<_> = bru.query.into_iter().filter(|(_, _, on)| !on).collect();
    add_query(&mut request, disabled);
    request.path_params = bru
        .path
        .into_iter()
        .map(|(n, v)| Param::new(n, v))
        .collect();
    match bru.body_mode.as_str() {
        "json" => raw_body(&mut request, bru.body_text, Some("application/json")),
        "xml" => raw_body(&mut request, bru.body_text, Some("application/xml")),
        "text" => raw_body(&mut request, bru.body_text, Some("text/plain")),
        "sparql" => raw_body(
            &mut request,
            bru.body_text,
            Some("application/sparql-query"),
        ),
        "graphql" => graphql_body(&mut request, &bru.body_text, &bru.graphql_vars),
        "formUrlEncoded" | "multipartForm" => form_body(
            &mut request,
            bru.body_mode == "multipartForm",
            bru.form,
            warnings,
            &at,
        ),
        _ => {}
    }
    request.auth = auth_of(&bru.auth_mode, &bru.auth, warnings, &at);
    if request.url.contains("{{process.env.") {
        warnings.push(format!(
            "{at}: {{{{process.env.*}}}} variables come from a .env file; add them to an environment"
        ));
    }
    ImportNode::Request {
        name,
        request,
        scripts: scripts(&bru.pre, &bru.post, &bru.tests),
    }
}

// --------------------------------------------------------------------- .bru files

/// The blocks of a `.bru` file: `name {` … `}` (or `[` … `]` for lists), raw content.
fn blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_end();
        let (name, close) = if let Some(name) = trimmed.strip_suffix('{') {
            (name.trim(), "}")
        } else if let Some(name) = trimmed.strip_suffix('[') {
            (name.trim(), "]")
        } else {
            continue;
        };
        if name.is_empty() || line.starts_with(char::is_whitespace) {
            continue;
        }
        let mut content = Vec::new();
        for inner in lines.by_ref() {
            if inner.trim_end() == close {
                break;
            }
            content.push(inner.strip_prefix("  ").unwrap_or(inner));
        }
        out.push((name.to_string(), content.join("\n")));
    }
    out
}

/// `key: value` lines; a `~` before the key disables it.
fn dict(content: &str) -> Vec<(String, String, bool)> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            let (key, enabled) = match key.trim().strip_prefix('~') {
                Some(k) => (k.trim(), false),
                None => (key.trim(), true),
            };
            Some((key.to_string(), value.trim().to_string(), enabled))
        })
        .collect()
}

fn get<'a>(entries: &'a [(String, String, bool)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _, _)| k == key)
        .map(|(_, v, _)| v.as_str())
}

const METHODS: [&str; 9] = [
    "get", "post", "put", "patch", "delete", "head", "options", "connect", "trace",
];

fn parse_bru(text: &str) -> BruRequest {
    let mut bru = BruRequest::default();
    for (name, content) in blocks(text) {
        match name.as_str() {
            "meta" => {
                let meta = dict(&content);
                bru.name = get(&meta, "name").unwrap_or_default().to_string();
                bru.seq = get(&meta, "seq")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
            }
            m if METHODS.contains(&m) => {
                let fields = dict(&content);
                bru.method = m.to_uppercase();
                bru.url = get(&fields, "url").unwrap_or_default().to_string();
                bru.body_mode = match get(&fields, "body").unwrap_or("none") {
                    "formUrlEncoded" | "form-urlencoded" => "formUrlEncoded".into(),
                    "multipartForm" | "multipart-form" => "multipartForm".into(),
                    other => other.to_string(),
                };
                bru.auth_mode = get(&fields, "auth").unwrap_or_default().to_string();
            }
            "headers" => bru.headers = dict(&content),
            "params:query" | "query" => bru.query = dict(&content),
            "params:path" => {
                bru.path = dict(&content).into_iter().map(|(k, v, _)| (k, v)).collect()
            }
            "body:json" | "body:text" | "body:xml" | "body:sparql" | "body:graphql" => {
                bru.body_text = content
            }
            "body:graphql:vars" => bru.graphql_vars = content,
            "body:form-urlencoded" => {
                bru.form = dict(&content)
                    .into_iter()
                    .map(|(k, v, on)| (k, v, false, on))
                    .collect()
            }
            "body:multipart-form" => {
                bru.form = dict(&content)
                    .into_iter()
                    .map(|(k, v, on)| {
                        match v.strip_prefix("@file(").and_then(|f| f.strip_suffix(')')) {
                            Some(file) => (
                                k,
                                file.split('|').next().unwrap_or_default().to_string(),
                                true,
                                on,
                            ),
                            None => (k, v, false, on),
                        }
                    })
                    .collect()
            }
            "script:pre-request" => bru.pre = content,
            "script:post-response" => bru.post = content,
            "tests" => bru.tests = content,
            auth if auth.starts_with("auth:") => {
                if bru.auth_mode.is_empty() {
                    bru.auth_mode = auth[5..].to_string();
                }
                bru.auth = dict(&content).into_iter().map(|(k, v, _)| (k, v)).collect();
            }
            _ => {}
        }
    }
    bru
}

/// A folder's own settings, from its `folder.bru` (or the collection's `collection.bru`).
fn folder_settings(
    dir: &Path,
    file: &str,
    warnings: &mut Vec<String>,
) -> (String, f64, Option<Auth>, Scripts) {
    let fallback = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Ok(text) = fs::read_to_string(dir.join(file)) else {
        return (fallback, f64::MAX, None, Scripts::default());
    };
    let bru = parse_bru(&text);
    let name = if bru.name.is_empty() {
        fallback
    } else {
        bru.name.clone()
    };
    let mode = blocks(&text)
        .into_iter()
        .find(|(n, _)| n == "auth")
        .and_then(|(_, c)| get(&dict(&c), "mode").map(str::to_string))
        .unwrap_or(bru.auth_mode.clone());
    let auth = auth_of(&mode, &bru.auth, warnings, &name);
    if !bru.headers.is_empty() {
        warnings.push(format!(
            "{name}: folder headers are not imported; add them to the folder"
        ));
    }
    let seq = if bru.seq == 0.0 { f64::MAX } else { bru.seq };
    (name, seq, auth, scripts(&bru.pre, &bru.post, &bru.tests))
}

fn read_dir(dir: &Path, warnings: &mut Vec<String>) -> Vec<(f64, String, ImportNode)> {
    let mut nodes = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return nodes;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            if matches!(file_name.as_str(), "environments" | "node_modules" | ".git") {
                continue;
            }
            let (name, seq, auth, scripts) = folder_settings(&path, "folder.bru", warnings);
            let mut children = read_dir(&path, warnings);
            // A folder with no requests and no folder.bru is not part of the collection.
            if children.is_empty() && !path.join("folder.bru").is_file() {
                continue;
            }
            children.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            nodes.push((
                seq,
                name.clone(),
                ImportNode::Folder {
                    name,
                    children: children.into_iter().map(|(_, _, n)| n).collect(),
                    scripts,
                    auth,
                },
            ));
        } else if file_name.ends_with(".bru")
            && !matches!(file_name.as_str(), "folder.bru" | "collection.bru")
        {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let mut bru = parse_bru(&text);
            if bru.method.is_empty() {
                continue;
            }
            if bru.name.is_empty() {
                bru.name = file_name.trim_end_matches(".bru").to_string();
            }
            let (seq, name) = (bru.seq, bru.name.clone());
            nodes.push((seq, name, node_of(bru, warnings)));
        }
    }
    nodes
}

fn bru_environment(name: &str, text: &str) -> Environment {
    let mut variables = Vec::new();
    for (block, content) in blocks(text) {
        match block.as_str() {
            "vars" => {
                for (key, value, enabled) in dict(&content) {
                    if enabled {
                        variables.push(Variable {
                            name: key,
                            value,
                            secret: false,
                            has_value: false,
                        });
                    }
                }
            }
            "vars:secret" => {
                for key in content.split([',', '\n']) {
                    let key = key.trim().trim_start_matches('~');
                    if !key.is_empty() {
                        variables.push(Variable {
                            name: key.to_string(),
                            value: String::new(),
                            secret: true,
                            has_value: false,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Environment {
        id: String::new(),
        name: name.to_string(),
        variables,
    }
}

/// Imports a Bruno collection folder (the one holding `bruno.json`).
pub(super) fn folder(dir: &Path) -> Result<ImportPlan, ImportError> {
    let config: Value = fs::read_to_string(dir.join("bruno.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| ImportError::Invalid(format!("{}: no bruno.json", dir.display())))?;
    let mut warnings = Vec::new();
    let (_, _, folder_auth, folder_scripts) = folder_settings(dir, "collection.bru", &mut warnings);
    let mut nodes = read_dir(dir, &mut warnings);
    nodes.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut environments = Vec::new();
    if let Ok(entries) = fs::read_dir(dir.join("environments")) {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().is_some_and(|e| e == "bru") {
                if let Ok(text) = fs::read_to_string(&path) {
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    environments.push(bru_environment(&name, &text));
                }
            }
        }
    }
    if environments
        .iter()
        .any(|e| e.variables.iter().any(|v| v.secret))
    {
        warnings.push(
            "secret variables come without their values (Bruno keeps them on each machine); set them in the environment"
                .into(),
        );
    }
    let name = text(config.get("name"));
    Ok(ImportPlan {
        format: ImportFormat::Bruno,
        folder: Some(if name.trim().is_empty() {
            "Bruno".into()
        } else {
            name
        }),
        folder_scripts,
        folder_auth,
        nodes: nodes.into_iter().map(|(_, _, n)| n).collect(),
        environments,
        warnings,
    })
}

// --------------------------------------------------------------------- JSON export

pub(super) fn is_bruno_json(value: &Value) -> bool {
    value.get("items").is_some_and(Value::is_array)
        && (value.get("brunoConfig").is_some()
            || list(value.get("items")).iter().any(|i| {
                matches!(
                    i.get("type").and_then(|t| t.as_str()),
                    Some("http-request" | "graphql-request" | "folder")
                )
            }))
}

fn json_request(item: &Value) -> BruRequest {
    let req = item.get("request").cloned().unwrap_or_default();
    let body = req.get("body").cloned().unwrap_or_default();
    let mode = text(body.get("mode"));
    let auth = req.get("auth").cloned().unwrap_or_default();
    let auth_mode = text(auth.get("mode"));
    let auth_fields = auth
        .get(auth_mode.as_str())
        .and_then(|a| a.as_object())
        .map(|o| o.iter().map(|(k, v)| (k.clone(), text(Some(v)))).collect())
        .unwrap_or_default();
    let params = list(req.get("params"));
    let kv = |p: &Value| {
        (
            text(p.get("name")),
            text(p.get("value")),
            p.get("enabled").is_none_or(|e| flag(Some(e))),
        )
    };
    BruRequest {
        name: text(item.get("name")),
        seq: item.get("seq").and_then(|s| s.as_f64()).unwrap_or(0.0),
        method: text(req.get("method")),
        url: text(req.get("url")),
        headers: list(req.get("headers")).iter().map(kv).collect(),
        query: params
            .iter()
            .filter(|p| text(p.get("type")) != "path")
            .map(kv)
            .collect(),
        path: params
            .iter()
            .filter(|p| text(p.get("type")) == "path")
            .map(|p| (text(p.get("name")), text(p.get("value"))))
            .collect(),
        body_text: match mode.as_str() {
            "graphql" => text(body.pointer("/graphql/query")),
            other => text(body.get(other)),
        },
        graphql_vars: text(body.pointer("/graphql/variables")),
        form: list(body.get(mode.as_str()))
            .iter()
            .map(|f| {
                let file = text(f.get("type")) == "file";
                let value = match f.get("value") {
                    Some(Value::Array(files)) if file => text(files.first()),
                    other => text(other),
                };
                (
                    text(f.get("name")),
                    value,
                    file,
                    f.get("enabled").is_none_or(|e| flag(Some(e))),
                )
            })
            .collect(),
        body_mode: mode,
        auth_mode,
        auth: auth_fields,
        pre: text(item.pointer("/request/script/req")),
        post: text(item.pointer("/request/script/res")),
        tests: text(item.pointer("/request/tests")),
    }
}

fn json_items(items: &[Value], warnings: &mut Vec<String>) -> Vec<ImportNode> {
    let mut sorted: Vec<&Value> = items.iter().collect();
    sorted.sort_by(|a, b| {
        let seq = |v: &Value| v.get("seq").and_then(|s| s.as_f64()).unwrap_or(f64::MAX);
        seq(a).total_cmp(&seq(b))
    });
    sorted
        .into_iter()
        .filter_map(|item| match text(item.get("type")).as_str() {
            "folder" => {
                let name = text(item.get("name"));
                let auth = item.pointer("/root/request/auth");
                let auth_mode = auth.map(|a| text(a.get("mode"))).unwrap_or_default();
                let fields = auth
                    .and_then(|a| a.get(auth_mode.as_str()))
                    .and_then(|a| a.as_object())
                    .map(|o| o.iter().map(|(k, v)| (k.clone(), text(Some(v)))).collect())
                    .unwrap_or_default();
                Some(ImportNode::Folder {
                    auth: auth_of(&auth_mode, &fields, warnings, &name),
                    children: json_items(list(item.get("items")), warnings),
                    scripts: Scripts::default(),
                    name,
                })
            }
            "http-request" | "graphql-request" => Some(node_of(json_request(item), warnings)),
            other => {
                warnings.push(format!(
                    "{}: '{other}' items are not supported yet; skipped",
                    text(item.get("name"))
                ));
                None
            }
        })
        .collect()
}

pub(super) fn json(value: &Value) -> Result<ImportPlan, ImportError> {
    let mut warnings = Vec::new();
    let nodes = json_items(list(value.get("items")), &mut warnings);
    let environments = list(value.get("environments"))
        .iter()
        .map(|env| Environment {
            id: String::new(),
            name: text(env.get("name")),
            variables: list(env.get("variables"))
                .iter()
                .filter(|v| v.get("enabled").is_none_or(|e| flag(Some(e))))
                .map(|v| Variable {
                    name: text(v.get("name")),
                    value: text(v.get("value")),
                    secret: flag(v.get("secret")),
                    has_value: false,
                })
                .collect(),
        })
        .collect();
    let name = text(value.get("name"));
    let auth = value.pointer("/root/request/auth");
    let auth_mode = auth.map(|a| text(a.get("mode"))).unwrap_or_default();
    let fields = auth
        .and_then(|a| a.get(auth_mode.as_str()))
        .and_then(|a| a.as_object())
        .map(|o| o.iter().map(|(k, v)| (k.clone(), text(Some(v)))).collect())
        .unwrap_or_default();
    Ok(ImportPlan {
        format: ImportFormat::Bruno,
        folder_auth: auth_of(&auth_mode, &fields, &mut warnings, &name),
        folder: Some(if name.trim().is_empty() {
            "Bruno".into()
        } else {
            name
        }),
        folder_scripts: Scripts::default(),
        nodes,
        environments,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRU: &str = "meta {\n  name: Create user\n  type: http\n  seq: 2\n}\n\npost {\n  url: {{base}}/users?page=1\n  body: json\n  auth: bearer\n}\n\nparams:query {\n  page: 1\n  ~debug: true\n}\n\nheaders {\n  Accept: application/json\n  ~X-Off: 1\n}\n\nauth:bearer {\n  token: {{token}}\n}\n\nbody:json {\n  {\n    \"name\": \"Ana\"\n  }\n}\n\ntests {\n  test(\"ok\", () => expect(res.status).to.equal(201));\n}\n";

    #[test]
    fn reads_a_bru_file() {
        let bru = parse_bru(BRU);
        assert_eq!(bru.name, "Create user");
        assert_eq!(bru.seq, 2.0);
        let mut warnings = Vec::new();
        let ImportNode::Request {
            name,
            request,
            scripts,
        } = node_of(bru, &mut warnings)
        else {
            panic!("a request");
        };
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(name, "Create user");
        assert_eq!(request.method, crate::HttpMethod::Post);
        assert_eq!(request.url, "{{base}}/users?page=1");
        assert_eq!(request.disabled_params, vec![Param::new("debug", "true")]);
        assert_eq!(request.body.as_deref(), Some("{\n  \"name\": \"Ana\"\n}"));
        assert!(request
            .headers
            .iter()
            .any(|h| h.name == "X-Off" && !h.enabled));
        assert_eq!(
            request.auth,
            Some(Auth::Bearer {
                token: "{{token}}".into()
            })
        );
        assert!(
            scripts.test.starts_with("// Imported from Bruno"),
            "{}",
            scripts.test
        );
    }

    #[test]
    fn imports_a_collection_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("bruno.json"),
            r#"{"version":"1","name":"Shop","type":"collection"}"#,
        )
        .unwrap();
        fs::write(root.join("b.bru"), BRU).unwrap();
        fs::write(
            root.join("a.bru"),
            "meta {\n  name: List\n  seq: 1\n}\n\nget {\n  url: {{base}}/users\n  body: none\n  auth: inherit\n}\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("orders")).unwrap();
        fs::write(
            root.join("orders/folder.bru"),
            "meta {\n  name: Orders\n  seq: 3\n}\n\nauth {\n  mode: basic\n}\n\nauth:basic {\n  username: ana\n  password: {{pw}}\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("orders/get.bru"),
            "meta {\n  name: Get order\n  seq: 1\n}\n\nget {\n  url: {{base}}/orders/:id\n}\n\nparams:path {\n  id: 7\n}\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("environments")).unwrap();
        fs::write(
            root.join("environments/Local.bru"),
            "vars {\n  base: http://localhost:8080\n}\nvars:secret [\n  token,\n  pw\n]\n",
        )
        .unwrap();
        let plan = super::super::parse_file(&root.join("bruno.json")).unwrap();
        assert_eq!(plan.format, ImportFormat::Bruno);
        assert_eq!(plan.folder.as_deref(), Some("Shop"));
        let names: Vec<&str> = plan
            .nodes
            .iter()
            .map(|n| match n {
                ImportNode::Folder { name, .. } | ImportNode::Request { name, .. } => name.as_str(),
            })
            .collect();
        assert_eq!(names, vec!["List", "Create user", "Orders"]);
        let ImportNode::Folder { auth, children, .. } = &plan.nodes[2] else {
            panic!("a folder");
        };
        assert!(matches!(auth, Some(Auth::Basic { username, .. }) if username == "ana"));
        let ImportNode::Request { request, .. } = &children[0] else {
            panic!("a request");
        };
        assert_eq!(request.path_params, vec![Param::new("id", "7")]);
        let env = &plan.environments[0];
        assert_eq!(env.name, "Local");
        assert_eq!(env.variables.len(), 3);
        assert!(env.variables.iter().any(|v| v.name == "token" && v.secret));
    }
}
