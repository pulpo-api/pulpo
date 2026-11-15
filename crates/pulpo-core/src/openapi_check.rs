//! The OpenAPI editor's checks: a document (YAML or JSON) against the official
//! OpenAPI 3.0 / 3.1 JSON Schemas, plus what the schema cannot see (path parameters
//! that are not declared, repeated `operationId`s, local `$ref`s that point nowhere),
//! each problem with the line it is on, and a preview of the operations.

use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const OAS30: &str = include_str!("../schemas/openapi-3.0.json");
const OAS31: &str = include_str!("../schemas/openapi-3.1.json");
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];
const MAX_SCHEMA_PROBLEMS: usize = 30;
const MAX_MESSAGE: usize = 220;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    pub severity: Severity,
    /// 1-based; `None` when it could not be placed.
    pub line: Option<usize>,
    /// JSON pointer of the offending value (`/paths/~1orders/get`).
    pub pointer: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    pub name: String,
    /// `path`, `query`, `header` or `cookie`.
    pub location: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub status: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub method: String,
    pub path: String,
    pub summary: String,
    pub operation_id: String,
    pub tags: Vec<String>,
    pub deprecated: bool,
    pub parameters: Vec<Parameter>,
    /// Media types of the request body, empty when there is none.
    pub body: Vec<String>,
    pub body_required: bool,
    pub responses: Vec<Response>,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// `3.0.3`, `3.1.0`… empty when the document does not say.
    pub openapi: String,
    pub title: String,
    pub version: String,
    pub servers: Vec<String>,
    pub problems: Vec<Problem>,
    pub operations: Vec<Operation>,
}

impl Check {
    pub fn errors(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
            .count()
    }
}

/// Everything wrong with `text` and what it describes.
pub fn check(text: &str) -> Check {
    let mut check = Check {
        openapi: String::new(),
        title: String::new(),
        version: String::new(),
        servers: Vec::new(),
        problems: Vec::new(),
        operations: Vec::new(),
    };
    let doc = match parse(text) {
        Ok(doc) => doc,
        Err((line, message)) => {
            check.problems.push(Problem {
                severity: Severity::Error,
                line,
                pointer: String::new(),
                message,
            });
            return check;
        }
    };
    if !doc.is_object() {
        check.problems.push(problem(
            text,
            Severity::Error,
            "",
            "an OpenAPI document is an object",
        ));
        return check;
    }
    let text_of = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    check.openapi = text_of(doc.get("openapi"));
    check.title = text_of(doc.pointer("/info/title"));
    check.version = text_of(doc.pointer("/info/version"));
    check.servers = doc
        .get("servers")
        .and_then(Value::as_array)
        .map(|s| {
            s.iter()
                .filter_map(|s| s.get("url").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if doc.get("swagger").is_some() {
        check.problems.push(problem(
            text,
            Severity::Warning,
            "/swagger",
            "Swagger 2.0: only references and path parameters are checked; convert it to OpenAPI 3 for full validation",
        ));
    } else if check.openapi.starts_with("3.1") {
        schema_problems(text, &doc, oas31(), &mut check.problems);
    } else if check.openapi.starts_with("3.0") {
        schema_problems(text, &doc, oas30(), &mut check.problems);
    } else if check.openapi.is_empty() {
        check.problems.push(problem(
            text,
            Severity::Error,
            "",
            "missing \"openapi\": say which version this is (3.0.3 or 3.1.0)",
        ));
    } else {
        check.problems.push(problem(
            text,
            Severity::Error,
            "/openapi",
            &format!(
                "unknown OpenAPI version {}: use 3.0.x or 3.1.x",
                check.openapi
            ),
        ));
    }
    reference_problems(text, &doc, &mut check.problems);
    check.operations = operations(text, &doc, &mut check.problems);
    check.problems.sort_by_key(|p| p.line.unwrap_or(usize::MAX));
    check
}

fn parse(text: &str) -> Result<Value, (Option<usize>, String)> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Err((None, "the document is empty".into()));
    }
    if trimmed.starts_with('{') {
        serde_json::from_str(text).map_err(|e| (Some(e.line()), format!("JSON: {e}")))
    } else {
        serde_norway::from_str(text)
            .map_err(|e| (e.location().map(|l| l.line()), format!("YAML: {e}")))
    }
}

fn oas30() -> &'static jsonschema::Validator {
    static V: OnceLock<jsonschema::Validator> = OnceLock::new();
    V.get_or_init(|| compile(OAS30))
}

fn oas31() -> &'static jsonschema::Validator {
    static V: OnceLock<jsonschema::Validator> = OnceLock::new();
    V.get_or_init(|| compile(OAS31))
}

fn compile(schema: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(schema).expect("embedded OpenAPI schema is JSON");
    jsonschema::validator_for(&schema).expect("embedded OpenAPI schema compiles")
}

fn schema_problems(
    text: &str,
    doc: &Value,
    validator: &jsonschema::Validator,
    out: &mut Vec<Problem>,
) {
    let mut seen = HashSet::new();
    for error in validator.iter_errors(doc) {
        let mut found = Vec::new();
        leaves(&error, &mut found);
        for (pointer, message) in found {
            if seen.len() >= MAX_SCHEMA_PROBLEMS {
                return;
            }
            // Several branches can fail at one place; one problem per place is enough.
            if seen.insert(pointer.clone()) {
                out.push(problem(text, Severity::Error, &pointer, &message));
            }
        }
    }
}

/// A failed `oneOf`/`anyOf` says only that no branch fits; the branch that got
/// furthest into the document says what is actually wrong.
fn leaves(error: &jsonschema::ValidationError<'_>, out: &mut Vec<(String, String)>) {
    use jsonschema::error::ValidationErrorKind as Kind;
    let branches = match error.kind() {
        Kind::AnyOf { context } | Kind::OneOfNotValid { context } => Some(context),
        _ => None,
    };
    let depth =
        |e: &jsonschema::ValidationError<'_>| e.instance_path().to_string().matches('/').count();
    if let Some(best) = branches.and_then(|branches| {
        branches
            .iter()
            .filter(|b| !b.is_empty())
            .max_by_key(|b| (b.iter().map(depth).max(), std::cmp::Reverse(b.len())))
    }) {
        for e in best {
            leaves(e, out);
        }
        return;
    }
    let mut message = if error.instance().is_object() || error.instance().is_array() {
        error.masked().to_string()
    } else {
        error.to_string()
    };
    if message.chars().count() > MAX_MESSAGE {
        message = message.chars().take(MAX_MESSAGE).collect::<String>() + "…";
    }
    let mut pointer = error.instance_path().to_string();
    if let Kind::AdditionalProperties { unexpected } | Kind::UnevaluatedProperties { unexpected } =
        error.kind()
    {
        if let [key] = unexpected.as_slice() {
            pointer = format!("{pointer}/{}", escape(key));
        }
    }
    out.push((pointer, message));
}

/// Local `$ref`s that point nowhere, and components nobody uses.
fn reference_problems(text: &str, doc: &Value, out: &mut Vec<Problem>) {
    let mut refs = Vec::new();
    collect_refs(doc, String::new(), &mut refs);
    let mut used = HashSet::new();
    for (at, target) in &refs {
        let Some(local) = target.strip_prefix('#') else {
            continue;
        };
        used.insert(local.to_string());
        if doc.pointer(&decode_uri(local)).is_none() {
            out.push(problem(
                text,
                Severity::Error,
                at,
                &format!("$ref {target} points to nothing in this document"),
            ));
        }
    }
    let Some(components) = doc.get("components").and_then(Value::as_object) else {
        return;
    };
    for (kind, items) in components {
        let Some(items) = items.as_object() else {
            continue;
        };
        if kind == "securitySchemes" {
            continue;
        }
        for name in items.keys() {
            let pointer = format!("/components/{kind}/{}", escape(name));
            if !used
                .iter()
                .any(|u| u == &pointer || u.starts_with(&(pointer.clone() + "/")))
            {
                out.push(problem(
                    text,
                    Severity::Warning,
                    &pointer,
                    &format!("{kind}/{name} is never used"),
                ));
            }
        }
    }
}

fn collect_refs(value: &Value, at: String, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let here = format!("{at}/{}", escape(key));
                if key == "$ref" {
                    if let Some(target) = child.as_str() {
                        out.push((here, target.to_string()));
                    }
                } else {
                    collect_refs(child, here, out);
                }
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                collect_refs(child, format!("{at}/{i}"), out);
            }
        }
        _ => {}
    }
}

fn deref<'a>(doc: &'a Value, value: &'a Value) -> &'a Value {
    let mut value = value;
    for _ in 0..8 {
        match value
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|p| doc.pointer(&decode_uri(p)))
        {
            Some(target) => value = target,
            None => break,
        }
    }
    value
}

fn parameters(doc: &Value, list: Option<&Value>) -> Vec<Parameter> {
    list.and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|p| deref(doc, p))
                .filter_map(|p| {
                    Some(Parameter {
                        name: p.get("name")?.as_str()?.to_string(),
                        location: p.get("in")?.as_str()?.to_string(),
                        required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn operations(text: &str, doc: &Value, out: &mut Vec<Problem>) -> Vec<Operation> {
    let mut list = Vec::new();
    let mut ids: HashMap<String, String> = HashMap::new();
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return list;
    };
    for (path, item) in paths {
        let item = deref(doc, item);
        let base = format!("/paths/{}", escape(path));
        let template = template_params(path);
        let shared = parameters(doc, item.get("parameters"));
        for method in METHODS {
            let Some(op) = item.get(method) else {
                continue;
            };
            let at = format!("{base}/{method}");
            let mut params = shared.clone();
            for p in parameters(doc, op.get("parameters")) {
                params.retain(|q| !(q.name == p.name && q.location == p.location));
                params.push(p);
            }
            for name in &template {
                if !params
                    .iter()
                    .any(|p| p.location == "path" && &p.name == name)
                {
                    out.push(problem(
                        text,
                        Severity::Error,
                        &at,
                        &format!(
                            "{} {path}: path parameter {{{name}}} is not declared",
                            method.to_uppercase()
                        ),
                    ));
                }
            }
            for p in params.iter().filter(|p| p.location == "path") {
                if !template.contains(&p.name) {
                    out.push(problem(
                        text,
                        Severity::Error,
                        &at,
                        &format!(
                            "{} {path}: path parameter \"{}\" is not in the path",
                            method.to_uppercase(),
                            p.name
                        ),
                    ));
                }
            }
            let operation_id = op
                .get("operationId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !operation_id.is_empty() {
                let label = format!("{} {path}", method.to_uppercase());
                if let Some(first) = ids.get(&operation_id) {
                    out.push(problem(
                        text,
                        Severity::Error,
                        &format!("{at}/operationId"),
                        &format!("operationId \"{operation_id}\" is already used by {first}"),
                    ));
                } else {
                    ids.insert(operation_id.clone(), label);
                }
            }
            let body = op.get("requestBody").map(|b| deref(doc, b));
            list.push(Operation {
                method: method.to_uppercase(),
                path: path.clone(),
                summary: op
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                operation_id,
                tags: op
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|t| {
                        t.iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default(),
                deprecated: op
                    .get("deprecated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                parameters: params,
                body: body
                    .and_then(|b| b.get("content"))
                    .and_then(Value::as_object)
                    .map(|c| c.keys().cloned().collect())
                    .unwrap_or_default(),
                body_required: body
                    .and_then(|b| b.get("required"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                responses: op
                    .get("responses")
                    .and_then(Value::as_object)
                    .map(|r| {
                        r.iter()
                            .map(|(status, response)| Response {
                                status: status.clone(),
                                description: deref(doc, response)
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                line: line_of(text, &at),
            });
        }
    }
    list
}

fn template_params(path: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        names.push(rest[start + 1..start + len].to_string());
        rest = &rest[start + len + 1..];
    }
    names
}

fn problem(text: &str, severity: Severity, pointer: &str, message: &str) -> Problem {
    Problem {
        severity,
        line: line_of(text, pointer),
        pointer: pointer.to_string(),
        message: message.to_string(),
    }
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn unescape(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

/// `$ref`s are URI fragments: `%7B` and friends stand for the real characters.
fn decode_uri(pointer: &str) -> String {
    let bytes = pointer.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&pointer[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| pointer.to_string())
}

// ------------------------------------------------------------- line lookup

struct Line<'a> {
    indent: usize,
    /// After a leading `- `, where the key of a list item starts.
    key_indent: usize,
    item: bool,
    content: &'a str,
}

fn split(raw: &str) -> Option<Line<'_>> {
    let trimmed = raw.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let indent = raw.len() - trimmed.len();
    let (item, content, key_indent) = match trimmed.strip_prefix('-') {
        Some(rest) if rest.is_empty() || rest.starts_with(' ') => {
            let inner = rest.trim_start();
            (true, inner, indent + 1 + (rest.len() - inner.len()))
        }
        _ => (false, trimmed, indent),
    };
    Some(Line {
        indent,
        key_indent,
        item,
        content,
    })
}

fn has_key(content: &str, key: &str) -> bool {
    ["", "\"", "'"].iter().any(|quote| {
        content
            .strip_prefix(quote)
            .and_then(|r| r.strip_prefix(key))
            .and_then(|r| r.strip_prefix(quote))
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })
}

/// The 1-based line a JSON pointer lands on in YAML or pretty-printed JSON; the
/// deepest part found when the rest cannot be followed.
pub fn line_of(text: &str, pointer: &str) -> Option<usize> {
    let lines: Vec<Option<Line>> = text.lines().map(split).collect();
    let first = lines.iter().position(Option::is_some)?;
    let json = lines[first]
        .as_ref()
        .is_some_and(|l| l.content.starts_with('{'));
    // Children of the node at `at` sit deeper than `parent`; a YAML key may also hold
    // a list whose `- ` items sit at its own indent.
    let mut at = first;
    let mut parent: isize = if json { 0 } else { -1 };
    let mut parent_is_key = false;
    let mut inclusive = !json;
    let mut found = Some(first + 1);
    for segment in pointer.split('/').skip(1) {
        let key = unescape(segment);
        let start = if inclusive { at } else { at + 1 };
        let block = || {
            lines
                .iter()
                .enumerate()
                .skip(start)
                .filter_map(|(n, line)| line.as_ref().map(|line| (n, line)))
                .take_while(|(n, line)| {
                    let indent = line.indent as isize;
                    let compact = !json && parent_is_key && line.item && indent == parent;
                    (inclusive && *n == at) || indent > parent || (indent == parent && compact)
                })
        };
        let mut level = None;
        let mut hit = block()
            .find(|(_, line)| {
                *level.get_or_insert(line.key_indent) == line.key_indent
                    && has_key(line.content, &key)
            })
            .map(|(n, line)| (n, line.key_indent, true, false));
        if let (None, Ok(mut i)) = (hit, key.parse::<usize>()) {
            let mut level = None;
            hit = block()
                .find(|(_, line)| {
                    let starts = if json {
                        !line.content.starts_with('}') && !line.content.starts_with(']')
                    } else {
                        line.item
                    };
                    if *level.get_or_insert(line.indent) != line.indent || !starts {
                        return false;
                    }
                    if i == 0 {
                        return true;
                    }
                    i -= 1;
                    false
                })
                .map(|(n, line)| (n, line.indent, false, !json));
        }
        let Some((n, indent, is_key, own)) = hit else {
            break;
        };
        at = n;
        parent = indent as isize;
        parent_is_key = is_key;
        inclusive = own;
        found = Some(n + 1);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r##"openapi: 3.0.3
info:
  title: Shop
  version: "1.2"
servers:
  - url: https://shop.example.com
paths:
  /orders/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema: { type: integer }
    get:
      summary: Get an order
      operationId: getOrder
      tags: [orders]
      responses:
        "200":
          description: The order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema: { $ref: "#/components/schemas/Order" }
      responses:
        "201": { description: Created }
components:
  schemas:
    Order:
      type: object
      properties:
        id: { type: integer }
"##;

    #[test]
    fn a_good_document_has_no_problems_and_a_preview() {
        let check = check(GOOD);
        assert_eq!(check.problems, vec![], "{:#?}", check.problems);
        assert_eq!(
            (
                check.title.as_str(),
                check.version.as_str(),
                check.openapi.as_str()
            ),
            ("Shop", "1.2", "3.0.3")
        );
        assert_eq!(check.servers, vec!["https://shop.example.com"]);
        assert_eq!(check.operations.len(), 2);
        let get = &check.operations[0];
        assert_eq!(
            (get.method.as_str(), get.path.as_str()),
            ("GET", "/orders/{id}")
        );
        assert_eq!(get.summary, "Get an order");
        assert_eq!(get.tags, vec!["orders"]);
        assert_eq!(get.parameters[0].name, "id");
        assert!(get.parameters[0].required);
        assert_eq!(get.responses[0].status, "200");
        assert_eq!(get.line, Some(14));
        let post = &check.operations[1];
        assert_eq!(post.body, vec!["application/json"]);
        assert!(post.body_required);
        assert_eq!(post.line, Some(26));
    }

    #[test]
    fn schema_errors_are_placed_on_their_line() {
        let bad = GOOD.replace("      summary: Get an order\n", "      summary: 7\n");
        let check = check(&bad);
        assert_eq!(check.errors(), 1, "{:#?}", check.problems);
        let p = &check.problems[0];
        assert_eq!(p.pointer, "/paths/~1orders~1{id}/get/summary");
        assert_eq!(p.line, Some(15));

        let bad = GOOD.replace("  title: Shop\n", "");
        let check = super::check(&bad);
        assert!(check
            .problems
            .iter()
            .any(|p| p.pointer == "/info" && p.line == Some(2) && p.message.contains("title")));
    }

    #[test]
    fn explains_failed_branches_and_unknown_keys() {
        let bad = GOOD
            .replace(
                "      summary: Get an order
",
                "      sumary: Get an order
",
            )
            .replace("description: Created", "description: 5");
        let check = check(&bad);
        let found: Vec<_> = check
            .problems
            .iter()
            .map(|p| (p.line, p.message.as_str()))
            .collect();
        assert_eq!(
            found,
            vec![
                (
                    Some(15),
                    "Additional properties are not allowed ('sumary' was unexpected)"
                ),
                (Some(34), "5 is not of type \"string\"")
            ]
        );
    }

    #[test]
    fn finds_what_the_schema_cannot_see() {
        let bad = GOOD
            .replace("operationId: createOrder", "operationId: getOrder")
            .replace("  /orders:\n", "  /orders/{shop}:\n")
            .replace(
                "schema: { $ref: \"#/components/schemas/Order\" }",
                "schema: { $ref: \"#/components/schemas/Ordr\" }",
            );
        let check = check(&bad);
        let messages: Vec<_> = check
            .problems
            .iter()
            .map(|p| (p.line, p.message.as_str()))
            .collect();
        assert!(
            messages.contains(&(
                Some(26),
                "POST /orders/{shop}: path parameter {shop} is not declared"
            )),
            "{messages:#?}"
        );
        assert!(messages.contains(&(
            Some(27),
            "operationId \"getOrder\" is already used by GET /orders/{id}"
        )));
        assert!(messages.contains(&(
            Some(32),
            "$ref #/components/schemas/Ordr points to nothing in this document"
        )));
    }

    #[test]
    fn unused_components_are_warnings() {
        let doc = format!("{GOOD}    Unused:\n      type: string\n");
        let check = check(&doc);
        assert_eq!(check.errors(), 0);
        assert_eq!(check.problems.len(), 1);
        assert_eq!(check.problems[0].severity, Severity::Warning);
        assert_eq!(check.problems[0].message, "schemas/Unused is never used");
        assert_eq!(check.problems[0].line, Some(41));
    }

    #[test]
    fn checks_3_1_and_json() {
        let doc = serde_json::json!({
            "openapi": "3.1.0",
            "info": {"title": "T", "version": "1"},
            "paths": {"/a": {"get": {"responses": {"200": {"description": "ok"}}}}},
            "webhooks": {}
        });
        let text = serde_json::to_string_pretty(&doc).unwrap();
        assert_eq!(check(&text).problems, vec![]);

        let bad = text.replace("\"description\": \"ok\"", "\"description\": 5");
        let check = check(&bad);
        assert_eq!(check.errors(), 1, "{:#?}", check.problems);
        assert_eq!(
            check.problems[0].pointer,
            "/paths/~1a/get/responses/200/description"
        );
        assert_eq!(check.problems[0].line, Some(12));
    }

    #[test]
    fn syntax_errors_and_versions() {
        let check = super::check("openapi: 3.0.3\ninfo: [\n");
        assert_eq!(check.problems.len(), 1);
        assert!(check.problems[0].message.starts_with("YAML"));
        assert!(check.problems[0].line.is_some());
        assert!(super::check("info: {}\n").problems[0]
            .message
            .contains("missing \"openapi\""));
        assert!(super::check("openapi: 4.0.0\n").problems[0]
            .message
            .contains("unknown OpenAPI version"));
    }

    #[test]
    fn lines_of_list_items() {
        let text = "a:\n  - x: 1\n  - x: 2\n    y: 3\nb: 4\n";
        assert_eq!(line_of(text, "/a/1/y"), Some(4));
        assert_eq!(line_of(text, "/a/0/x"), Some(2));
        assert_eq!(line_of(text, "/b"), Some(5));
        assert_eq!(line_of(text, "/a/7"), Some(1));
        let compact = "a:
- x: 1
- x: 2
  y: 3
b: 4
";
        assert_eq!(line_of(compact, "/a/1/y"), Some(4));
        assert_eq!(line_of(compact, "/b"), Some(5));
    }
}
