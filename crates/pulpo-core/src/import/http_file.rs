//! `.http` / `.rest` files (VS Code REST Client and JetBrains HTTP Client).

use super::common::{method_of, raw_body};
use super::{ImportError, ImportFormat, ImportNode, ImportPlan};
use crate::collection::Scripts;
use crate::environment::{Environment, Variable};
use crate::http::{Auth, Header, HttpRequest};
use crate::HttpMethod;
use std::str::FromStr;

/// `GET https://…` (or a bare URL) starts a request.
fn request_line(line: &str) -> Option<(String, String)> {
    let mut parts = line.split_whitespace();
    let first = parts.next()?;
    if HttpMethod::from_str(first).is_ok() || matches!(first, "TRACE" | "CONNECT") {
        let url = parts.next()?.to_string();
        return Some((first.to_string(), url));
    }
    let looks_like_url =
        first.starts_with("http://") || first.starts_with("https://") || first.starts_with("{{");
    looks_like_url.then(|| ("GET".to_string(), first.to_string()))
}

pub(super) fn looks_like_http_file(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("###")
            || line
                .split_whitespace()
                .next()
                .is_some_and(|m| HttpMethod::from_str(m).is_ok())
                && line.split_whitespace().nth(1).is_some_and(|u| {
                    u.starts_with("http") || u.starts_with("{{") || u.starts_with('/')
                })
    })
}

/// `Authorization: Basic user pass` (or `user:pass`, not encoded) is a REST Client
/// shorthand: it becomes the Basic auth.
fn basic_shorthand(value: &str) -> Option<Auth> {
    let rest = value.strip_prefix("Basic ")?.trim();
    let (user, pass) = match rest.split_once(' ') {
        Some((u, p)) => (u, p),
        None => rest.split_once(':')?,
    };
    Some(Auth::Basic {
        username: user.trim().to_string(),
        password: pass.trim().to_string(),
    })
}

pub(super) fn plan(text: &str) -> Result<ImportPlan, ImportError> {
    let mut warnings = Vec::new();
    let mut variables: Vec<Variable> = Vec::new();
    let mut nodes = Vec::new();
    let mut handlers = 0;

    let mut sections: Vec<(String, Vec<&str>)> = vec![(String::new(), Vec::new())];
    for line in text.trim_start_matches('\u{feff}').lines() {
        if let Some(name) = line.trim_start().strip_prefix("###") {
            sections.push((name.trim().to_string(), Vec::new()));
        } else if let Some((_, lines)) = sections.last_mut() {
            lines.push(line);
        }
    }

    for (index, (section_name, lines)) in sections.into_iter().enumerate() {
        let mut name = section_name;
        let mut no_redirect = false;
        let mut no_cookies = false;
        let mut lines = lines.into_iter().peekable();
        // Comments, variables and directives before the request line.
        let mut start = None;
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(comment) = trimmed
                .strip_prefix('#')
                .or_else(|| trimmed.strip_prefix("//"))
            {
                let comment = comment.trim();
                if let Some(n) = comment.strip_prefix("@name") {
                    name = n.trim().trim_start_matches('=').trim().to_string();
                } else if comment.starts_with("@no-redirect") {
                    no_redirect = true;
                } else if comment.starts_with("@no-cookie-jar") {
                    no_cookies = true;
                }
                continue;
            }
            if let Some(var) = trimmed.strip_prefix('@') {
                if let Some((key, value)) = var.split_once('=') {
                    let key = key.trim().to_string();
                    let value = value.trim().to_string();
                    match variables.iter_mut().find(|v| v.name == key) {
                        Some(existing) => existing.value = value,
                        None => variables.push(Variable {
                            name: key,
                            value,
                            secret: false,
                            has_value: false,
                        }),
                    }
                }
                continue;
            }
            start = Some(trimmed.to_string());
            break;
        }
        let Some(first) = start else {
            continue;
        };
        let Some((method, mut url)) = request_line(&first) else {
            warnings.push(format!(
                "request {}: cannot read '{first}'; skipped",
                index.max(1)
            ));
            continue;
        };
        // Query lines (`?a=1`, `&b=2`) may continue the URL.
        while let Some(next) = lines.peek() {
            let t = next.trim();
            if t.starts_with('?') || t.starts_with('&') {
                url.push_str(t);
                lines.next();
            } else {
                break;
            }
        }
        let at = if name.is_empty() {
            format!("{method} {url}")
        } else {
            name.clone()
        };
        let mut request = HttpRequest::new(method_of(&method, &mut warnings, &at), url.clone());
        for line in lines.by_ref() {
            let t = line.trim();
            if t.is_empty() {
                break;
            }
            if t.starts_with('#') || t.starts_with("//") {
                continue;
            }
            if let Some((key, value)) = t.split_once(':') {
                let value = value.trim();
                if key.eq_ignore_ascii_case("authorization") {
                    // Base64 has neither spaces nor colons, so encoded values stay a header.
                    if let Some(auth) = basic_shorthand(value) {
                        request.auth = Some(auth);
                        continue;
                    }
                }
                request.headers.push(Header::new(key.trim(), value));
            }
        }
        let mut body: Vec<&str> = Vec::new();
        for line in lines {
            let t = line.trim_start();
            if t.starts_with("> ") || t.starts_with(">>") {
                handlers += 1;
                continue;
            }
            body.push(line);
        }
        // A `> {% … %}` handler spans lines: drop everything from its start.
        if let Some(i) = body.iter().position(|l| l.trim_start().starts_with("{%")) {
            body.truncate(i);
        }
        let body = body.join("\n").trim_end().to_string();
        if let Some(file) = body
            .trim()
            .strip_prefix("< ")
            .filter(|_| !body.trim().contains('\n'))
        {
            request.body_file = Some(super::workspace_path(file.trim(), &mut warnings, &at));
        } else {
            let content_type = request
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-type"))
                .map(|h| h.value.clone());
            raw_body(&mut request, body, content_type.as_deref());
        }
        if no_redirect {
            request.settings.follow_redirects = Some(false);
        }
        if no_cookies {
            request.cookies = false;
        }
        if url.contains(".response.") {
            warnings.push(format!(
                "{at}: request variables ({{{{name.response…}}}}) are not supported; use a test script with pm.environment.set"
            ));
        }
        nodes.push(ImportNode::Request {
            name: if name.is_empty() {
                format!("{method} {}", url.split('?').next().unwrap_or(&url))
            } else {
                name
            },
            request,
            scripts: Scripts::default(),
        });
    }
    if handlers > 0 {
        warnings.push(format!(
            "{handlers} response handler(s) (> …) were left out; write them as test scripts"
        ));
    }
    if nodes.is_empty() {
        return Err(ImportError::Invalid(
            "no requests found in the .http file".into(),
        ));
    }
    let environments = if variables.is_empty() {
        Vec::new()
    } else {
        vec![Environment {
            id: String::new(),
            name: "http file variables".into(),
            variables,
        }]
    };
    Ok(ImportPlan {
        format: ImportFormat::HttpFile,
        folder: Some("HTTP file".into()),
        folder_scripts: Scripts::default(),
        folder_auth: None,
        nodes,
        environments,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "@base = https://api.example.com\n@token = abc\n\n### List users\nGET {{base}}/users\n    ?page=1\n    &size=20\nAccept: application/json\n\n### \n# @name createUser\n# @no-redirect\nPOST {{base}}/users HTTP/1.1\nContent-Type: application/json\nAuthorization: Basic ana s3cret\n\n{\n  \"name\": \"Ana\"\n}\n\n> {% client.global.set(\"id\", response.body.id); %}\n\n###\nPUT {{base}}/avatar\nContent-Type: image/png\n\n< ./avatar.png\n";

    #[test]
    fn reads_requests_variables_and_bodies() {
        let plan = plan(FILE).unwrap();
        assert_eq!(plan.format, ImportFormat::HttpFile);
        assert_eq!(plan.nodes.len(), 3);
        let vars = &plan.environments[0].variables;
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].value, "https://api.example.com");

        let ImportNode::Request { name, request, .. } = &plan.nodes[0] else {
            panic!()
        };
        assert_eq!(name, "List users");
        assert_eq!(request.url, "{{base}}/users?page=1&size=20");
        assert_eq!(request.headers.len(), 1);

        let ImportNode::Request { name, request, .. } = &plan.nodes[1] else {
            panic!()
        };
        assert_eq!(name, "createUser");
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.body.as_deref(), Some("{\n  \"name\": \"Ana\"\n}"));
        assert_eq!(request.settings.follow_redirects, Some(false));
        assert_eq!(
            request.auth,
            Some(Auth::Basic {
                username: "ana".into(),
                password: "s3cret".into()
            })
        );

        let ImportNode::Request { name, request, .. } = &plan.nodes[2] else {
            panic!()
        };
        assert_eq!(name, "PUT {{base}}/avatar");
        assert_eq!(request.body_file.as_deref(), Some("avatar.png"));
        assert!(
            plan.warnings.iter().any(|w| w.contains("response handler")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn detects_http_files() {
        assert!(looks_like_http_file("GET https://x/y\n"));
        assert!(looks_like_http_file("### a\nPOST /x\n"));
        assert!(!looks_like_http_file("name: x\nurl: y\n"));
    }
}
