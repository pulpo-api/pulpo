//! Pieces the importers of other tools share.

use crate::http::{Form, FormField, FormKind, Header, HttpRequest, Param};
use crate::HttpMethod;
use serde_json::Value;
use std::str::FromStr;

/// A JSON value as text: strings as they are, numbers and booleans written out.
pub(super) fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

pub(super) fn flag(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

pub(super) fn list(value: Option<&Value>) -> &[Value] {
    value
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The method, or GET with a warning when Pulpo does not know it.
pub(super) fn method_of(name: &str, warnings: &mut Vec<String>, at: &str) -> HttpMethod {
    let name = name.trim();
    if name.is_empty() {
        return HttpMethod::Get;
    }
    HttpMethod::from_str(name).unwrap_or_else(|_| {
        warnings.push(format!(
            "{at}: method {name} is not supported; imported as GET"
        ));
        HttpMethod::Get
    })
}

/// Percent-encodes a query part but leaves `{{variables}}` as they are.
pub(super) fn encode_keeping_vars(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else {
            break;
        };
        out.push_str(&crate::http::encode_query(&rest[..start]));
        out.push_str(&rest[start..start + len + 2]);
        rest = &rest[start + len + 2..];
    }
    out.push_str(&crate::http::encode_query(rest));
    out
}

/// Adds query params: the enabled ones to the URL, the others to `disabled_params`.
pub(super) fn add_query(request: &mut HttpRequest, params: Vec<(String, String, bool)>) {
    let mut pairs = Vec::new();
    for (name, value, enabled) in params {
        if name.is_empty() {
            continue;
        }
        if enabled {
            pairs.push(format!(
                "{}={}",
                encode_keeping_vars(&name),
                encode_keeping_vars(&value)
            ));
        } else {
            request.disabled_params.push(Param::new(name, value));
        }
    }
    if pairs.is_empty() {
        return;
    }
    let (base, hash) = match request.url.find('#') {
        Some(i) => (request.url[..i].to_string(), request.url[i..].to_string()),
        None => (request.url.clone(), String::new()),
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    request.url = format!("{base}{sep}{}{hash}", pairs.join("&"));
}

pub(super) fn has_header(headers: &[Header], name: &str) -> bool {
    headers.iter().any(|h| h.name.eq_ignore_ascii_case(name))
}

/// Sets the raw body and, when missing, its Content-Type.
pub(super) fn raw_body(request: &mut HttpRequest, body: String, content_type: Option<&str>) {
    if body.is_empty() {
        return;
    }
    if let Some(content_type) = content_type.filter(|c| !c.is_empty()) {
        if !has_header(&request.headers, "content-type") {
            request
                .headers
                .push(Header::new("Content-Type", content_type));
        }
    }
    request.body = Some(body);
}

/// A GraphQL query as the JSON body servers expect.
pub(super) fn graphql_body(request: &mut HttpRequest, query: &str, variables: &str) {
    let variables: Value =
        serde_json::from_str(variables).unwrap_or(Value::Object(Default::default()));
    let payload = serde_json::json!({ "query": query, "variables": variables });
    raw_body(
        request,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
        Some("application/json"),
    );
}

/// A form body from `(name, value, is_file, enabled)` fields.
pub(super) fn form_body(
    request: &mut HttpRequest,
    multipart: bool,
    fields: Vec<(String, String, bool, bool)>,
    warnings: &mut Vec<String>,
    at: &str,
) {
    let fields = fields
        .into_iter()
        .filter(|(name, ..)| !name.is_empty())
        .map(|(name, value, file, enabled)| FormField {
            name,
            value: if file && multipart {
                super::workspace_path(&value, warnings, at)
            } else {
                value
            },
            file: file && multipart,
            enabled,
        })
        .collect();
    request.form = Some(Form {
        kind: if multipart {
            FormKind::Multipart
        } else {
            FormKind::Urlencoded
        },
        fields,
    });
}

/// A script written for another tool's API, kept as a comment so nothing is lost and
/// nothing breaks when it runs.
pub(super) fn foreign_script(tool: &str, api: &str, code: &str) -> String {
    if code.trim().is_empty() {
        return String::new();
    }
    let body = code.replace("*/", "* /");
    format!(
        "// Imported from {tool}. It uses the {api} API, not pm.*: rewrite it, then remove the comment.\n/*\n{}\n*/\n",
        body.trim_end()
    )
}

/// `{{ _.name }}` (Insomnia) and `<<name>>` (Hoppscotch) as `{{name}}`.
pub(super) fn pulpo_vars(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    loop {
        let next_curly = rest.find("{{");
        let next_angle = rest.find("<<");
        let (start, close) = match (next_curly, next_angle) {
            (Some(c), Some(a)) if a < c => (a, ">>"),
            (Some(c), _) => (c, "}}"),
            (None, Some(a)) => (a, ">>"),
            (None, None) => break,
        };
        let Some(len) = rest[start + 2..].find(close) else {
            break;
        };
        let inner = rest[start + 2..start + 2 + len].trim();
        let name = inner.strip_prefix("_.").unwrap_or(inner).trim();
        out.push_str(&rest[..start]);
        let simple = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || "_-.$".contains(c));
        if simple {
            out.push_str(&format!("{{{{{name}}}}}"));
        } else {
            out.push_str(&rest[start..start + 2 + len + 2]);
        }
        rest = &rest[start + 2 + len + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_variables_of_other_tools() {
        assert_eq!(
            pulpo_vars("{{ _.base }}/users/<<id>>?q={{token}}"),
            "{{base}}/users/{{id}}?q={{token}}"
        );
        assert_eq!(pulpo_vars("a << b and {% uuid %}"), "a << b and {% uuid %}");
    }

    #[test]
    fn adds_query_params_keeping_variables() {
        let mut request = HttpRequest::new(HttpMethod::Get, "https://x/a#top");
        add_query(
            &mut request,
            vec![
                ("q".into(), "a b".into(), true),
                ("id".into(), "{{id}}".into(), true),
                ("off".into(), "1".into(), false),
            ],
        );
        assert_eq!(request.url, "https://x/a?q=a%20b&id={{id}}#top");
        assert_eq!(request.disabled_params, vec![Param::new("off", "1")]);
    }
}
