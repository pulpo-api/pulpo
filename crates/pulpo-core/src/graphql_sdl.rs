//! GraphQL schema files (SDL: `type Query { … }`) into requests: one per root field, with
//! its operation, variables and the scalar fields of what it returns.

use crate::http::{Graphql, HttpRequest};
use crate::import::{ImportFormat, ImportNode, ImportPlan};
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

const SCALARS: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub args: Vec<(String, String)>,
    /// As written: `[Order!]!`.
    pub ty: String,
}

#[derive(Debug, Default)]
pub struct Sdl {
    /// type / input / interface name → fields.
    pub types: HashMap<String, Vec<Field>>,
    pub enums: HashMap<String, Vec<String>>,
    pub scalars: Vec<String>,
    /// `query`, `mutation`, `subscription` → root type name.
    pub roots: Vec<(String, String)>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

/// The type without `[]` and `!`: `[Order!]!` → `Order`.
pub fn named(ty: &str) -> &str {
    ty.trim_matches(|c| c == '[' || c == ']' || c == '!')
}

/// Whether a text is a schema (not a query document).
pub fn is_sdl(text: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"(?m)^\s*(extend\s+)?(type|schema)\s+(Query|Mutation|Subscription|\{)",
    )
    .is_match(text)
        || re(&R, r"(?m)^\s*schema\s*\{").is_match(text)
}

pub fn parse(text: &str) -> Sdl {
    static STRINGS: OnceLock<Regex> = OnceLock::new();
    static COMMENTS: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static FIELD: OnceLock<Regex> = OnceLock::new();
    static ARG: OnceLock<Regex> = OnceLock::new();
    static SCALAR: OnceLock<Regex> = OnceLock::new();
    static SCHEMA: OnceLock<Regex> = OnceLock::new();
    // Descriptions and comments say nothing about the shape.
    let text = re(&STRINGS, r#"(?s)""".*?"""|"[^"\n]*""#).replace_all(text, " ");
    let text = re(&COMMENTS, r"#[^\n]*").replace_all(&text, " ");
    let mut sdl = Sdl::default();
    for s in re(&SCALAR, r"\bscalar\s+(\w+)").captures_iter(&text) {
        sdl.scalars.push(s[1].to_string());
    }
    for b in re(
        &BLOCK,
        r"(?s)\b(?:extend\s+)?(type|input|interface|enum)\s+(\w+)[^{]*\{(.*?)\}",
    )
    .captures_iter(&text)
    {
        let (kind, name, body) = (&b[1], b[2].to_string(), &b[3]);
        if kind == "enum" {
            let values = body
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|v| !v.is_empty() && !v.starts_with('@'))
                .map(str::to_string)
                .collect();
            sdl.enums.entry(name).or_insert(values);
            continue;
        }
        let fields: Vec<Field> = re(&FIELD, r"(\w+)\s*(\(([^)]*)\))?\s*:\s*([\[\]\w!]+)")
            .captures_iter(body)
            .map(|f| Field {
                name: f[1].to_string(),
                args: f
                    .get(3)
                    .map(|a| {
                        re(&ARG, r"(\w+)\s*:\s*([\[\]\w!]+)")
                            .captures_iter(a.as_str())
                            .map(|x| (x[1].to_string(), x[2].to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                ty: f[4].to_string(),
            })
            .collect();
        sdl.types.entry(name).or_default().extend(fields);
    }
    for kind in ["query", "mutation", "subscription"] {
        let custom = re(&SCHEMA, r"(?s)\bschema\s*\{(.*?)\}")
            .captures(&text)
            .and_then(|s| {
                Regex::new(&format!(r"\b{kind}\s*:\s*(\w+)"))
                    .ok()?
                    .captures(&s[1])
                    .map(|c| c[1].to_string())
            });
        let mut default = kind.to_string();
        default[..1].make_ascii_uppercase();
        let root = custom.unwrap_or(default);
        if sdl.types.contains_key(&root) {
            sdl.roots.push((kind.to_string(), root));
        }
    }
    sdl
}

impl Sdl {
    fn is_leaf(&self, ty: &str) -> bool {
        let n = named(ty);
        SCALARS.contains(&n) || self.scalars.iter().any(|s| s == n) || self.enums.contains_key(n)
    }

    /// The fields to ask for: scalars of the returned type (`__typename` when it has none).
    fn selection(&self, ty: &str) -> Option<String> {
        if self.is_leaf(ty) {
            return None;
        }
        let fields: Vec<&str> = self
            .types
            .get(named(ty))
            .map(|f| {
                f.iter()
                    .filter(|f| self.is_leaf(&f.ty) && f.args.is_empty())
                    .map(|f| f.name.as_str())
                    .collect()
            })
            .unwrap_or_default();
        Some(if fields.is_empty() {
            "__typename".to_string()
        } else {
            fields.join("\n    ")
        })
    }

    fn sample(&self, ty: &str, depth: usize) -> serde_json::Value {
        use serde_json::json;
        if ty.trim_end_matches('!').starts_with('[') {
            let inner = ty.trim_end_matches('!');
            return json!([self.sample(&inner[1..inner.len() - 1], depth)]);
        }
        match named(ty) {
            "Int" => json!(0),
            "Float" => json!(0.0),
            "Boolean" => json!(false),
            "String" | "ID" => json!(""),
            n if self.enums.contains_key(n) => {
                json!(self.enums[n].first().cloned().unwrap_or_default())
            }
            n if depth < 3 && self.types.contains_key(n) => {
                let mut out = serde_json::Map::new();
                for f in &self.types[n] {
                    out.insert(f.name.clone(), self.sample(&f.ty, depth + 1));
                }
                serde_json::Value::Object(out)
            }
            _ => json!(null),
        }
    }

    /// The operation for a root field, and its variables.
    pub fn operation(&self, kind: &str, field: &Field) -> (String, String) {
        let mut name = field.name.clone();
        name[..1].make_ascii_uppercase();
        let decls: Vec<String> = field
            .args
            .iter()
            .map(|(a, t)| format!("${a}: {t}"))
            .collect();
        let passes: Vec<String> = field
            .args
            .iter()
            .map(|(a, _)| format!("{a}: ${a}"))
            .collect();
        let head = if decls.is_empty() {
            format!("{kind} {name}")
        } else {
            format!("{kind} {name}({})", decls.join(", "))
        };
        let call = if passes.is_empty() {
            field.name.clone()
        } else {
            format!("{}({})", field.name, passes.join(", "))
        };
        let query = match self.selection(&field.ty) {
            Some(sel) => format!("{head} {{\n  {call} {{\n    {sel}\n  }}\n}}\n"),
            None => format!("{head} {{\n  {call}\n}}\n"),
        };
        let variables = if field.args.is_empty() {
            String::new()
        } else {
            let mut vars = serde_json::Map::new();
            for (a, t) in &field.args {
                vars.insert(a.clone(), self.sample(t, 0));
            }
            serde_json::to_string_pretty(&serde_json::Value::Object(vars)).unwrap_or_default()
        };
        (query, variables)
    }

    /// `(kind, field)` of every root field.
    pub fn root_fields(&self) -> Vec<(String, Field)> {
        let mut out = Vec::new();
        for (kind, root) in &self.roots {
            for f in self.types.get(root).into_iter().flatten() {
                out.push((kind.clone(), f.clone()));
            }
        }
        out
    }
}

/// An import plan: a folder per operation type, a request per root field, sent to
/// `{{baseUrl}}` (an environment with `http://localhost:4000/graphql`).
pub fn plan(text: &str, file_name: &str) -> Option<ImportPlan> {
    let sdl = parse(text);
    let fields = sdl.root_fields();
    if fields.is_empty() {
        return None;
    }
    let mut folders: Vec<(String, Vec<ImportNode>)> = Vec::new();
    for (kind, field) in fields {
        let (query, variables) = sdl.operation(&kind, &field);
        let mut request = HttpRequest::new(crate::HttpMethod::Post, "{{baseUrl}}");
        request.graphql = Some(Graphql {
            query,
            variables,
            operation_name: None,
        });
        let node = ImportNode::Request {
            name: field.name.clone(),
            request,
            scripts: Default::default(),
        };
        let folder = match kind.as_str() {
            "mutation" => "Mutations",
            "subscription" => "Subscriptions",
            _ => "Queries",
        };
        match folders.iter_mut().find(|(f, _)| f == folder) {
            Some((_, list)) => list.push(node),
            None => folders.push((folder.to_string(), vec![node])),
        }
    }
    let stem = std::path::Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "GraphQL".into());
    Some(ImportPlan {
        format: ImportFormat::GraphqlSchema,
        folder: Some(stem.clone()),
        folder_scripts: Default::default(),
        folder_auth: None,
        nodes: folders
            .into_iter()
            .map(|(name, children)| ImportNode::Folder {
                name,
                children,
                scripts: Default::default(),
                auth: None,
            })
            .collect(),
        environments: vec![crate::environment::Environment {
            id: String::new(),
            name: format!("{stem} local"),
            variables: vec![crate::environment::Variable {
                name: "baseUrl".into(),
                value: "http://localhost:4000/graphql".into(),
                secret: false,
                has_value: true,
            }],
        }],
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
"""The shop."""
scalar DateTime
enum Status { PENDING PAID }
type Order { id: ID! total: Float status: Status createdAt: DateTime items(first: Int): [Item!]! customer: Customer }
type Item { sku: String }
type Customer { name: String }
input OrderFilter { status: Status, from: DateTime }
type Query {
  "One order"
  order(id: ID!): Order
  orders(filter: OrderFilter, first: Int = 10): [Order!]!
  ping: String
}
type Mutation { pay(id: ID!): Order }
type Subscription { orderPaid: Order }
"#;

    #[test]
    fn writes_an_operation_per_root_field() {
        assert!(is_sdl(SCHEMA));
        assert!(!is_sdl("query Me { me { id } }"));
        let sdl = parse(SCHEMA);
        let fields = sdl.root_fields();
        let names: Vec<String> = fields
            .iter()
            .map(|(k, f)| format!("{k} {}", f.name))
            .collect();
        assert_eq!(
            names,
            [
                "query order",
                "query orders",
                "query ping",
                "mutation pay",
                "subscription orderPaid"
            ]
        );
        let (query, variables) = sdl.operation("query", &fields[0].1);
        assert_eq!(query, "query Order($id: ID!) {\n  order(id: $id) {\n    id\n    total\n    status\n    createdAt\n  }\n}\n");
        assert_eq!(variables, "{\n  \"id\": \"\"\n}");
        let (_, variables) = sdl.operation("query", &fields[1].1);
        let v: serde_json::Value = serde_json::from_str(&variables).unwrap();
        assert_eq!(v["filter"]["status"], "PENDING");
        assert_eq!(v["first"], 0);
        let (ping, none) = sdl.operation("query", &fields[2].1);
        assert_eq!(ping, "query Ping {\n  ping\n}\n");
        assert!(none.is_empty());
        let plan = plan(SCHEMA, "shop.graphql").unwrap();
        assert_eq!(plan.folder.as_deref(), Some("shop"));
        assert_eq!(plan.nodes.len(), 3);
        assert_eq!(
            plan.environments[0].variables[0].value,
            "http://localhost:4000/graphql"
        );
    }

    #[test]
    fn follows_a_custom_schema_block() {
        let sdl = parse("schema { query: Root }\ntype Root { hello: String }\n");
        assert_eq!(sdl.roots, vec![("query".to_string(), "Root".to_string())]);
    }
}
