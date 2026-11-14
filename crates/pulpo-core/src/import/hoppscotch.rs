//! Hoppscotch collections (JSON: one collection or a list of them) and HAR files.

use super::common::{
    add_query, flag, foreign_script, form_body, list, method_of, pulpo_vars, raw_body, text,
};
use super::{ImportError, ImportFormat, ImportNode, ImportPlan};
use crate::collection::Scripts;
use crate::http::{ApiKeyLocation, Auth, Header, HttpRequest};
use crate::signing::{AwsV4, DigestAuth};
use serde_json::Value;

fn is_collection(value: &Value) -> bool {
    value.get("requests").is_some_and(Value::is_array)
        && value.get("folders").is_some_and(Value::is_array)
}

pub(super) fn is_hoppscotch(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.first().is_some_and(is_collection),
        other => is_collection(other),
    }
}

fn vars(value: Option<&Value>) -> String {
    pulpo_vars(&text(value))
}

fn auth_of(auth: Option<&Value>, warnings: &mut Vec<String>, at: &str) -> Option<Auth> {
    let auth = auth?;
    let kind = text(auth.get("authType"));
    if kind == "inherit" || kind.is_empty() {
        return None;
    }
    if kind == "none"
        || auth
            .get("authActive")
            .is_some_and(|a| a == &Value::Bool(false))
    {
        return Some(Auth::None);
    }
    let get = |key: &str| vars(auth.get(key));
    Some(match kind.as_str() {
        "bearer" => Auth::Bearer {
            token: get("token"),
        },
        "basic" => Auth::Basic {
            username: get("username"),
            password: get("password"),
        },
        "api-key" => Auth::ApiKey {
            key: get("key"),
            value: get("value"),
            location: if get("addTo") == "QUERY_PARAMS" {
                ApiKeyLocation::Query
            } else {
                ApiKeyLocation::Header
            },
        },
        "digest" => Auth::Digest(DigestAuth {
            username: get("username"),
            password: get("password"),
        }),
        "aws-signature" => Auth::AwsV4(AwsV4 {
            access_key: get("accessKey"),
            secret_key: get("secretKey"),
            session_token: get("serviceToken"),
            region: get("region"),
            service: get("serviceName"),
        }),
        other => {
            warnings.push(format!(
                "{at}: '{other}' auth is not supported yet; it inherits from its folder"
            ));
            return None;
        }
    })
}

fn scripts_of(item: &Value) -> Scripts {
    Scripts {
        pre: foreign_script("Hoppscotch", "pw.*", &text(item.get("preRequestScript"))),
        test: foreign_script("Hoppscotch", "pw.*", &text(item.get("testScript"))),
    }
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
        vars(item.get("endpoint")),
    );
    for header in list(item.get("headers")) {
        let key = vars(header.get("key"));
        if key.is_empty() {
            continue;
        }
        let mut h = Header::new(key, vars(header.get("value")));
        h.enabled = header.get("active").is_none_or(|a| flag(Some(a)));
        request.headers.push(h);
    }
    let params = list(item.get("params"))
        .iter()
        .map(|p| {
            (
                vars(p.get("key")),
                vars(p.get("value")),
                p.get("active").is_none_or(|a| flag(Some(a))),
            )
        })
        .collect();
    add_query(&mut request, params);
    if let Some(body) = item.get("body") {
        let content_type = text(body.get("contentType"));
        match content_type.as_str() {
            "application/x-www-form-urlencoded" | "multipart/form-data" => {
                let multipart = content_type == "multipart/form-data";
                let fields = match body.get("body") {
                    Some(Value::Array(fields)) => fields
                        .iter()
                        .map(|f| {
                            (
                                vars(f.get("key")),
                                vars(f.get("value")),
                                flag(f.get("isFile")),
                                f.get("active").is_none_or(|a| flag(Some(a))),
                            )
                        })
                        .collect(),
                    // urlencoded bodies come as "a: 1\nb: 2".
                    Some(Value::String(lines)) => lines
                        .lines()
                        .filter_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            let disabled = k.trim_start().starts_with('#');
                            Some((
                                pulpo_vars(k.trim().trim_start_matches('#').trim()),
                                pulpo_vars(v.trim()),
                                false,
                                !disabled,
                            ))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                form_body(&mut request, multipart, fields, warnings, &at);
            }
            "" => {}
            other => raw_body(&mut request, vars(body.get("body")), Some(other)),
        }
    }
    request.auth = auth_of(item.get("auth"), warnings, &at);
    ImportNode::Request {
        name,
        request,
        scripts: scripts_of(item),
    }
}

fn folder_of(item: &Value, warnings: &mut Vec<String>) -> ImportNode {
    let name = text(item.get("name"));
    let name = if name.trim().is_empty() {
        "Folder".to_string()
    } else {
        name
    };
    let mut children: Vec<ImportNode> = list(item.get("folders"))
        .iter()
        .map(|f| folder_of(f, warnings))
        .collect();
    children.extend(
        list(item.get("requests"))
            .iter()
            .map(|r| request_of(r, warnings)),
    );
    if !list(item.get("headers")).is_empty() {
        warnings.push(format!(
            "{name}: collection headers are not imported; add them to the folder"
        ));
    }
    ImportNode::Folder {
        auth: auth_of(item.get("auth"), warnings, &name),
        scripts: scripts_of(item),
        name,
        children,
    }
}

pub(super) fn plan(value: &Value) -> Result<ImportPlan, ImportError> {
    let mut warnings = Vec::new();
    let collections: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut plan = if collections.len() == 1 {
        let ImportNode::Folder {
            name,
            children,
            scripts,
            auth,
        } = folder_of(collections[0], &mut warnings)
        else {
            unreachable!("a collection is a folder");
        };
        ImportPlan {
            format: ImportFormat::Hoppscotch,
            folder: Some(name),
            folder_scripts: scripts,
            folder_auth: auth,
            nodes: children,
            environments: Vec::new(),
            warnings: Vec::new(),
        }
    } else {
        ImportPlan {
            format: ImportFormat::Hoppscotch,
            folder: Some("Hoppscotch".into()),
            folder_scripts: Scripts::default(),
            folder_auth: None,
            nodes: collections
                .iter()
                .map(|c| folder_of(c, &mut warnings))
                .collect(),
            environments: Vec::new(),
            warnings: Vec::new(),
        }
    };
    plan.warnings = warnings;
    Ok(plan)
}

// ------------------------------------------------------------------------------- HAR

pub(super) fn is_har(value: &Value) -> bool {
    value.pointer("/log/entries").is_some_and(Value::is_array)
}

/// Headers the browser or the client sets on its own.
const SKIPPED_HEADERS: [&str; 6] = [
    "host",
    "content-length",
    "connection",
    "cookie",
    "accept-encoding",
    "upgrade-insecure-requests",
];

pub(super) fn har(value: &Value) -> Result<ImportPlan, ImportError> {
    let mut warnings = Vec::new();
    let mut hosts: Vec<(String, Vec<ImportNode>)> = Vec::new();
    let mut skipped_assets = 0;
    let mut cookies = false;
    for entry in list(value.pointer("/log/entries")) {
        // Browser HARs mark what each request was for: keep API calls and pages.
        if let Some(kind) = entry.get("_resourceType").and_then(|k| k.as_str()) {
            if !matches!(kind, "xhr" | "fetch" | "document") {
                skipped_assets += 1;
                continue;
            }
        }
        let Some(req) = entry.get("request") else {
            continue;
        };
        let url = text(req.get("url"));
        let Ok(parsed) = reqwest::Url::parse(&url) else {
            warnings.push(format!("skipped an entry with an invalid URL: {url}"));
            continue;
        };
        let at = format!("{} {}", text(req.get("method")), parsed.path());
        let mut request =
            HttpRequest::new(method_of(&text(req.get("method")), &mut warnings, &at), url);
        for header in list(req.get("headers")) {
            let name = text(header.get("name"));
            let lower = name.to_ascii_lowercase();
            if name.starts_with(':') || SKIPPED_HEADERS.contains(&lower.as_str()) {
                cookies |= lower == "cookie";
                continue;
            }
            request
                .headers
                .push(Header::new(name, text(header.get("value"))));
        }
        if let Some(post) = req.get("postData") {
            let mime = text(post.get("mimeType"));
            let mime_base = mime
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let params = list(post.get("params"));
            if !params.is_empty()
                && matches!(
                    mime_base.as_str(),
                    "application/x-www-form-urlencoded" | "multipart/form-data"
                )
            {
                let multipart = mime_base == "multipart/form-data";
                if multipart {
                    request
                        .headers
                        .retain(|h| !h.name.eq_ignore_ascii_case("content-type"));
                }
                let fields = params
                    .iter()
                    .map(|p| {
                        let file = p.get("fileName").is_some();
                        let value = if file {
                            text(p.get("fileName"))
                        } else {
                            text(p.get("value"))
                        };
                        (text(p.get("name")), value, file, true)
                    })
                    .collect();
                form_body(&mut request, multipart, fields, &mut warnings, &at);
            } else {
                raw_body(&mut request, text(post.get("text")), Some(&mime_base));
            }
        }
        let host = parsed.host_str().unwrap_or("requests").to_string();
        let node = ImportNode::Request {
            name: at,
            request,
            scripts: Scripts::default(),
        };
        match hosts.iter_mut().find(|(h, _)| *h == host) {
            Some((_, nodes)) => nodes.push(node),
            None => hosts.push((host, vec![node])),
        }
    }
    if skipped_assets > 0 {
        warnings.push(format!(
            "skipped {skipped_assets} entries for scripts, styles, images and fonts"
        ));
    }
    if cookies {
        warnings.push(
            "Cookie headers were left out: Pulpo's cookie jar sends them after a login request"
                .into(),
        );
    }
    if hosts.is_empty() {
        return Err(ImportError::Invalid("the HAR file has no requests".into()));
    }
    let nodes = if hosts.len() == 1 {
        hosts.remove(0).1
    } else {
        hosts
            .into_iter()
            .map(|(host, children)| ImportNode::Folder {
                name: host,
                children,
                scripts: Scripts::default(),
                auth: None,
            })
            .collect()
    };
    Ok(ImportPlan {
        format: ImportFormat::Har,
        folder: Some("HAR import".into()),
        folder_scripts: Scripts::default(),
        folder_auth: None,
        nodes,
        environments: Vec::new(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    #[test]
    fn imports_a_hoppscotch_collection() {
        let export = r#"[{"v":2,"name":"Shop","folders":[{"v":2,"name":"Users","folders":[],"requests":[
            {"v":"1","name":"Create","method":"POST","endpoint":"<<base>>/users",
             "headers":[{"key":"X-Trace","value":"1","active":false}],
             "params":[{"key":"dry","value":"true","active":true}],
             "body":{"contentType":"application/json","body":"{\"a\":1}"},
             "auth":{"authType":"bearer","authActive":true,"token":"<<token>>"},
             "preRequestScript":"pw.env.set('x','1');","testScript":""}]}],
            "requests":[{"v":"1","name":"Health","method":"GET","endpoint":"https://x/health","headers":[],"params":[],
             "body":{"contentType":null,"body":null},"auth":{"authType":"inherit","authActive":true}}]}]"#;
        let plan = parse(export).unwrap();
        assert_eq!(plan.format, ImportFormat::Hoppscotch);
        assert_eq!(plan.folder.as_deref(), Some("Shop"));
        assert_eq!(plan.nodes.len(), 2);
        let ImportNode::Folder { children, .. } = &plan.nodes[0] else {
            panic!()
        };
        let ImportNode::Request {
            request, scripts, ..
        } = &children[0]
        else {
            panic!()
        };
        assert_eq!(request.url, "{{base}}/users?dry=true");
        assert_eq!(
            request.auth,
            Some(Auth::Bearer {
                token: "{{token}}".into()
            })
        );
        assert!(request
            .headers
            .iter()
            .any(|h| h.name == "X-Trace" && !h.enabled));
        assert!(scripts.pre.contains("pw.env.set"), "{}", scripts.pre);
        assert!(scripts.pre.starts_with("// Imported from Hoppscotch"));
    }

    #[test]
    fn imports_api_calls_from_a_har_file() {
        let har = r#"{"log":{"version":"1.2","entries":[
          {"_resourceType":"script","request":{"method":"GET","url":"https://shop.example/app.js","headers":[]}},
          {"_resourceType":"fetch","request":{"method":"POST","url":"https://api.example/orders?x=1",
            "headers":[{"name":":authority","value":"api.example"},{"name":"Cookie","value":"s=1"},
                       {"name":"Content-Type","value":"application/json"},{"name":"Authorization","value":"Bearer t"}],
            "postData":{"mimeType":"application/json","text":"{\"id\":1}"}}},
          {"_resourceType":"xhr","request":{"method":"GET","url":"https://api.example/orders/1","headers":[]}}
        ]}}"#;
        let plan = parse(har).unwrap();
        assert_eq!(plan.format, ImportFormat::Har);
        assert_eq!(plan.nodes.len(), 2, "one host: no folders");
        let ImportNode::Request { name, request, .. } = &plan.nodes[0] else {
            panic!()
        };
        assert_eq!(name, "POST /orders");
        assert_eq!(request.url, "https://api.example/orders?x=1");
        assert_eq!(request.headers.len(), 2, "{:?}", request.headers);
        assert_eq!(request.body.as_deref(), Some("{\"id\":1}"));
        assert!(plan
            .warnings
            .iter()
            .any(|w| w.contains("skipped 1 entries")));
        assert!(plan.warnings.iter().any(|w| w.contains("Cookie")));
    }
}
