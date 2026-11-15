//! Automatic chaining: which response feeds which request. For the requests of a
//! folder, in order, every `{{variable}}` a request uses is looked for in the responses
//! of the requests before it (`orderId` ← `id` of `POST /orders`, `token` ←
//! `access_token`), and the result becomes a flow whose steps `save` those values.
//!
//! Responses come from the saved examples, the folder's OpenAPI contract or, with
//! `run`, from calling the API step by step.

use crate::collection::{self, RequestEntry};
use crate::environment::SecretStore;
use crate::flow::{Expect, Flow, Step};
use crate::run::{self, RunOptions, Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub variable: String,
    /// JSONPath into the producer's response body.
    pub path: String,
    /// Request id that produces it.
    pub from: String,
    /// `example`, `contract` or `response`.
    pub evidence: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainStep {
    pub id: String,
    pub name: String,
    pub method: String,
    pub url: String,
    /// Variables the request uses.
    pub uses: Vec<String>,
    /// Variables an earlier response provides.
    pub fed: Vec<Link>,
    /// Variables its own scripts set (already chained by hand).
    pub set_by_script: Vec<String>,
    /// Status of the real call (`run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chain {
    pub folder: String,
    pub steps: Vec<ChainStep>,
    /// `(request id, variable)` used and provided by nothing before it nor by the
    /// environment.
    pub unresolved: Vec<(String, String)>,
    /// Unresolved variables a script of a request outside the folder sets:
    /// `(variable, that request's name)`. Run it first (or reuse it as a subflow).
    #[serde(default)]
    pub set_elsewhere: Vec<(String, String)>,
    /// Variables the scripts of this folder, its parents or subfolders set (they run
    /// around every request inside).
    #[serde(default)]
    pub set_by_folders: Vec<String>,
}

impl Chain {
    pub fn links(&self) -> usize {
        self.steps.iter().map(|s| s.fed.len()).sum()
    }
}

/// `{{name}}` occurrences, without dynamic ones (`{{$guid}}`).
fn variables_in(text: &str, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else {
            break;
        };
        let name = rest[start + 2..start + 2 + len].trim();
        if !name.is_empty() && !name.starts_with('$') && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &rest[start + 2 + len + 2..];
    }
}

fn uses(entry: &RequestEntry) -> Vec<String> {
    let r = &entry.request;
    let mut out = Vec::new();
    variables_in(&r.url, &mut out);
    for h in r.headers.iter().filter(|h| h.enabled) {
        variables_in(&h.value, &mut out);
    }
    for p in &r.path_params {
        variables_in(&p.value, &mut out);
    }
    if let Some(body) = &r.body {
        variables_in(body, &mut out);
    }
    if let Some(g) = &r.graphql {
        variables_in(&g.query, &mut out);
        variables_in(&g.variables, &mut out);
    }
    if let Some(form) = &r.form {
        for f in form.fields.iter().filter(|f| f.enabled) {
            variables_in(&f.value, &mut out);
        }
    }
    if let Some(auth) = &r.auth {
        if let Ok(text) = serde_json::to_string(auth) {
            variables_in(&text, &mut out);
        }
    }
    out
}

/// Names a script sets with `pm.environment.set('x'` (or collection/local variables).
fn set_by_scripts(entry: &RequestEntry) -> Vec<String> {
    names_set_in(&format!("{}\n{}", entry.scripts.pre, entry.scripts.test))
}

/// Folder ids of a tree (`""` first), depth first.
fn folder_ids(nodes: &[crate::collection::TreeNode], out: &mut Vec<String>) {
    for n in nodes {
        if let crate::collection::TreeNode::Folder { id, children, .. } = n {
            out.push(id.clone());
            folder_ids(children, out);
        }
    }
}

fn folder_sets(root: &Path, folder_id: &str) -> Vec<String> {
    collection::folder_scripts(root, folder_id)
        .map(|s| names_set_in(&format!("{}\n{}", s.pre, s.test)))
        .unwrap_or_default()
}

fn names_set_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in [
        ".environment.set(",
        ".collectionVariables.set(",
        ".variables.set(",
        ".globals.set(",
    ] {
        let mut rest = text;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let quoted = rest.trim_start();
            if let Some(q) = quoted
                .chars()
                .next()
                .filter(|c| matches!(c, '\'' | '"' | '`'))
            {
                if let Some(end) = quoted[1..].find(q) {
                    let name = &quoted[1..1 + end];
                    if !out.iter().any(|n| n == name) {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    out
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Scalar leaves of a JSON value with their JSONPath (`$.data.id`, `$.items[0].id`).
fn leaves(value: &Value) -> Vec<(String, String)> {
    fn walk(value: &Value, path: String, out: &mut Vec<(String, String)>, depth: usize) {
        if depth > 8 || out.len() > 500 {
            return;
        }
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    let simple = k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                    let next = if simple {
                        format!("{path}.{k}")
                    } else {
                        format!("{path}['{}']", k.replace('\'', "\\'"))
                    };
                    match v {
                        Value::Object(_) | Value::Array(_) => walk(v, next, out, depth + 1),
                        _ => out.push((next, k.clone())),
                    }
                }
            }
            Value::Array(items) => {
                if let Some(first) = items.first() {
                    walk(first, format!("{path}[0]"), out, depth + 1);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, "$".into(), &mut out, 0);
    out
}

const TOKEN_VARS: [&str; 6] = [
    "token",
    "accesstoken",
    "authtoken",
    "jwt",
    "bearertoken",
    "bearer",
];
const TOKEN_KEYS: [&str; 5] = ["accesstoken", "token", "idtoken", "jwt", "authtoken"];

/// The last literal path segment, singular: `POST /api/orders` → `order`.
fn resource_of(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let segment = path
        .split('/')
        .rev()
        .find(|s| !s.is_empty() && !s.contains("{{") && !s.starts_with(':') && !s.starts_with('{'))
        .unwrap_or_default();
    let s = normalize(segment);
    if let Some(stem) = s.strip_suffix("ies") {
        format!("{stem}y")
    } else if s.ends_with("ses") {
        s[..s.len() - 2].to_string()
    } else {
        s.strip_suffix('s').map(str::to_string).unwrap_or(s)
    }
}

/// How well a response field fits a variable: 3 same name, 2 `id` of the resource the
/// producer created or a token, 0 no fit. Shallower paths win ties.
fn fit(variable: &str, key: &str, producer_url: &str) -> u8 {
    let (v, k) = (normalize(variable), normalize(key));
    if v == k {
        return 3;
    }
    if k == "id" && v == format!("{}id", resource_of(producer_url)) && v.len() > 2 {
        return 2;
    }
    if TOKEN_VARS.contains(&v.as_str()) && TOKEN_KEYS.contains(&k.as_str()) {
        return 2;
    }
    // `sellerToken`, `tokenSeller`: still a token when the producer is a login.
    if v.contains("token") && TOKEN_KEYS.contains(&k.as_str()) && looks_like_login(producer_url) {
        return 1;
    }
    0
}

fn looks_like_login(url: &str) -> bool {
    let u = url.to_lowercase();
    ["login", "token", "auth", "signin", "session"]
        .iter()
        .any(|w| u.contains(w))
}

/// The sample response of each request: a saved example, else the folder's contract.
fn samples(
    root: &Path,
    folder_id: &str,
    entries: &[RequestEntry],
) -> HashMap<String, (Value, &'static str)> {
    let mut out = HashMap::new();
    for e in entries {
        if let Ok(examples) = crate::examples::list_examples(root, &e.id) {
            let best = examples
                .iter()
                .filter(|x| (200..300).contains(&x.status))
                .find_map(|x| serde_json::from_str::<Value>(&x.body).ok());
            if let Some(body) = best {
                out.insert(e.id.clone(), (body, "example"));
            }
        }
    }
    // The contract of the folder or of a folder above it.
    let source = collection::ancestor_folders(&format!("{folder_id}/x"))
        .into_iter()
        .rev()
        .find_map(|f| {
            collection::folder_meta(root, &f)
                .ok()
                .and_then(|m| m.source)
        });
    if let Some(source) = source {
        if let Ok(responses) = crate::import::openapi_responses(&root.join(&source)) {
            let ops: Vec<((String, Vec<String>), Value)> = responses
                .into_iter()
                .map(|(method, path, body)| {
                    let method = method.parse().unwrap_or(crate::HttpMethod::Get);
                    (
                        crate::sync::key_of(&crate::http::HttpRequest::new(method, path)),
                        body,
                    )
                })
                .collect();
            for e in entries {
                if out.contains_key(&e.id) {
                    continue;
                }
                let key = crate::sync::key_of(&e.request);
                if let Some((_, body)) = ops
                    .iter()
                    .filter(|(k, _)| crate::sync::matches(&key, k))
                    .max_by_key(|(k, _)| k.1.len())
                {
                    out.insert(e.id.clone(), (body.clone(), "contract"));
                }
            }
        }
    }
    out
}

fn find_link(variable: &str, before: &[(String, String, Value, &'static str)]) -> Option<Link> {
    // Nearest earlier response first; the best fit inside it.
    for (id, url, body, evidence) in before.iter().rev() {
        let best = leaves(body)
            .into_iter()
            .map(|(path, key)| (fit(variable, &key, url), path))
            .filter(|(score, _)| *score > 0)
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.len().cmp(&a.1.len())));
        if let Some((_, path)) = best {
            return Some(Link {
                variable: variable.to_string(),
                path,
                from: id.clone(),
                evidence: evidence.to_string(),
            });
        }
    }
    None
}

/// Finds the chain of a folder. With `run`, each request is also sent in order (values
/// found so far passed along) and its real response joins the evidence.
pub async fn chain(
    root: &Path,
    folder_id: &str,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    run_requests: bool,
) -> Result<Chain, String> {
    let workspace = collection::load_workspace(root).map_err(|e| e.to_string())?;
    let entries = run::requests_under(&workspace.tree, folder_id);
    let everywhere = run::requests_under(&workspace.tree, "");
    if entries.is_empty() {
        return Err(format!("no requests in {folder_id}"));
    }
    let known = samples(root, folder_id, &entries);
    let env: Vec<String> = match environment_id {
        Some(id) => crate::environment::load_environment(root, id, store, false)
            .map(|e| e.variables.into_iter().map(|v| v.name).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let mut session = if run_requests {
        Some(Session::open(root, environment_id, store, false)?.detached())
    } else {
        None
    };
    let mut values: crate::data::Row = HashMap::new();
    let mut before: Vec<(String, String, Value, &'static str)> = Vec::new();
    // Folder scripts of the folder, the ones above it and the ones inside it.
    let mut all_folders = vec![String::new()];
    folder_ids(&workspace.tree, &mut all_folders);
    let inside = |f: &str| {
        folder_id.is_empty()
            || f == folder_id
            || f.starts_with(&format!("{folder_id}/"))
            || folder_id.starts_with(&format!("{f}/"))
            || f.is_empty()
    };
    let mut scripted: Vec<String> = Vec::new();
    for f in all_folders.iter().filter(|f| inside(f)) {
        for name in folder_sets(root, f) {
            if !scripted.contains(&name) {
                scripted.push(name);
            }
        }
    }
    let mut out = Chain {
        folder: folder_id.to_string(),
        set_by_folders: scripted.clone(),
        ..Default::default()
    };
    for entry in &entries {
        let mut step = ChainStep {
            id: entry.id.clone(),
            name: entry.name.clone(),
            method: entry.request.method.to_string(),
            url: entry.request.url.clone(),
            uses: uses(entry),
            set_by_script: set_by_scripts(entry),
            ..Default::default()
        };
        for variable in &step.uses {
            if scripted.contains(variable) {
                continue;
            }
            match find_link(variable, &before) {
                Some(link) => step.fed.push(link),
                None if !env.contains(variable)
                    && !out.unresolved.iter().any(|(_, v)| v == variable) =>
                {
                    out.unresolved.push((entry.id.clone(), variable.clone()))
                }
                None => {}
            }
        }
        scripted.extend(step.set_by_script.iter().cloned());
        // The values found for this step go into its real call.
        for link in &step.fed {
            if let Some((_, _, body, _)) = before.iter().find(|b| b.0 == link.from) {
                if let Ok(found) = crate::query::json_path(&body.to_string(), &link.path) {
                    if let Some(v) = found.first() {
                        let text = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        values.insert(link.variable.clone(), text);
                    }
                }
            }
        }
        let mut response: Option<(Value, &'static str)> = known.get(&entry.id).cloned();
        if let Some(session) = session.as_mut() {
            let report = run::run_in(
                root,
                entry,
                session,
                &RunOptions {
                    data: values.clone(),
                    ..Default::default()
                },
            )
            .await;
            if let Some(r) = &report.response {
                step.status = Some(r.status);
                if let Ok(body) = serde_json::from_str::<Value>(&r.body) {
                    response = Some((body, "response"));
                }
            }
        }
        if let Some((body, evidence)) = response {
            before.push((entry.id.clone(), entry.request.url.clone(), body, evidence));
        }
        out.steps.push(step);
    }
    for (_, variable) in &out.unresolved {
        let setter = everywhere
            .iter()
            .filter(|e| !entries.iter().any(|x| x.id == e.id))
            .find(|e| set_by_scripts(e).contains(variable))
            .map(|e| e.name.clone())
            .or_else(|| {
                all_folders
                    .iter()
                    .filter(|f| !inside(f))
                    .find(|f| folder_sets(root, f).contains(variable))
                    .map(|f| format!("folder {f}"))
            });
        if let Some(by) = setter {
            out.set_elsewhere.push((variable.clone(), by));
        }
    }
    Ok(out)
}

/// The flow that runs the folder in order, each producer saving what later steps use.
pub fn to_flow(chain: &Chain, name: &str) -> Flow {
    let mut saves: HashMap<String, BTreeMap<String, String>> = HashMap::new();
    for step in &chain.steps {
        for link in &step.fed {
            saves
                .entry(link.from.clone())
                .or_default()
                .insert(link.variable.clone(), link.path.clone());
        }
    }
    Flow {
        id: String::new(),
        version: 1,
        name: name.to_string(),
        description: format!(
            "Made from {} by pulpo chain: {} values passed between requests",
            if chain.folder.is_empty() {
                "the workspace"
            } else {
                &chain.folder
            },
            chain.links()
        ),
        steps: chain
            .steps
            .iter()
            .map(|s| Step::Request {
                request: s.id.clone(),
                name: None,
                save: saves.remove(&s.id).unwrap_or_default(),
                expect: s
                    .status
                    .filter(|c| (200..300).contains(c))
                    .map(|c| Expect { status: Some(c) }),
                retry: None,
                control: Default::default(),
            })
            .collect(),
    }
}

/// Text for the terminal.
pub fn to_text(chain: &Chain) -> String {
    let mut out = format!(
        "{} requests, {} values passed between them\n",
        chain.steps.len(),
        chain.links()
    );
    for (i, s) in chain.steps.iter().enumerate() {
        out.push_str(&format!(
            "{:>2}. {:<7} {}{}\n",
            i + 1,
            s.method,
            s.name,
            s.status.map(|c| format!("  → {c}")).unwrap_or_default()
        ));
        for l in &s.fed {
            let from = chain
                .steps
                .iter()
                .position(|p| p.id == l.from)
                .map(|p| format!("{}", p + 1))
                .unwrap_or_default();
            out.push_str(&format!(
                "      {{{{{}}}}} ← step {from} {}  ({})\n",
                l.variable, l.path, l.evidence
            ));
        }
        if !s.set_by_script.is_empty() {
            out.push_str(&format!(
                "      its scripts set {}\n",
                s.set_by_script.join(", ")
            ));
        }
    }
    if !chain.set_by_folders.is_empty() {
        out.push_str(&format!(
            "Folder scripts set {}\n",
            chain.set_by_folders.join(", ")
        ));
    }
    if !chain.unresolved.is_empty() {
        out.push_str("Not provided by any earlier response nor the environment:\n");
        for (id, var) in &chain.unresolved {
            let n = chain
                .steps
                .iter()
                .position(|s| &s.id == id)
                .map(|p| p + 1)
                .unwrap_or(0);
            let elsewhere = chain
                .set_elsewhere
                .iter()
                .find(|(v, _)| v == var)
                .map(|(_, by)| format!("  (set by “{by}” in another folder: run it first)"))
                .unwrap_or_default();
            out.push_str(&format!("  step {n}: {{{{{var}}}}}{elsewhere}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{create_folder, init_workspace, save_request, Scripts};
    use crate::environment::MemoryStore;
    use crate::http::{Header, HttpRequest};
    use crate::HttpMethod;
    use std::fs;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn save(
        root: &Path,
        id: &str,
        name: &str,
        method: HttpMethod,
        url: &str,
        auth: bool,
        test: &str,
    ) {
        let mut request = HttpRequest::new(method, url);
        if auth {
            request
                .headers
                .push(Header::new("Authorization", "Bearer {{token}}"));
        }
        save_request(
            root,
            &RequestEntry {
                id: id.into(),
                name: name.into(),
                protocol: crate::Protocol::Http,
                request,
                scripts: Scripts {
                    pre: String::new(),
                    test: test.into(),
                },
            },
        )
        .unwrap();
    }

    #[test]
    fn matches_fields_to_variables() {
        assert_eq!(fit("orderId", "id", "{{base}}/orders"), 2);
        assert_eq!(fit("orderId", "orderId", "x"), 3);
        assert_eq!(fit("order_id", "orderId", "x"), 3);
        assert_eq!(fit("token", "access_token", "/auth/token"), 2);
        assert_eq!(fit("tokenSeller", "access_token", "/auth/login"), 1);
        assert_eq!(fit("customerId", "id", "{{base}}/orders"), 0);
        assert_eq!(resource_of("{{b}}/api/categories"), "category");
        assert_eq!(resource_of("{{b}}/api/addresses/{{id}}"), "address");
        let paths = leaves(&serde_json::json!({ "data": { "items": [{ "id": 7, "x-y": 1 }] } }));
        assert!(
            paths.contains(&("$.data.items[0].id".into(), "id".into())),
            "{paths:?}"
        );
        assert!(paths.iter().any(|(p, _)| p == "$.data.items[0]['x-y']"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn chains_a_folder_from_examples_contracts_and_real_responses() {
        let dir = crate::git::tests::repo();
        let repo = dir.path();
        fs::create_dir_all(repo.join("api")).unwrap();
        fs::write(
            repo.join("api/shop.yaml"),
            r#"openapi: 3.0.3
info: { title: Shop, version: "1" }
paths:
  /orders:
    post:
      responses:
        "201":
          content:
            application/json:
              schema:
                type: object
                properties:
                  id: { type: string, readOnly: true, example: "o-1" }
                  status: { type: string }
  /orders/{orderId}/items:
    post:
      responses:
        "201": { content: { application/json: { schema: { type: object, properties: { itemId: { type: integer } } } } } }
  /orders/{orderId}/items/{itemId}:
    get: { responses: { "200": { description: ok } } }
"#,
        )
        .unwrap();
        let ws = repo.join("collection");
        fs::create_dir_all(&ws).unwrap();
        init_workspace(&ws, "Shop").unwrap();
        let f = create_folder(&ws, "", "Orders").unwrap();
        collection::set_folder_source(&ws, &f, Some("../api/shop.yaml")).unwrap();
        save(
            &ws,
            &format!("{f}/01-login.pulpo.yaml"),
            "Login",
            HttpMethod::Post,
            "{{base}}/auth/token",
            false,
            "",
        );
        save(
            &ws,
            &format!("{f}/02-order.pulpo.yaml"),
            "Create order",
            HttpMethod::Post,
            "{{base}}/orders",
            true,
            "",
        );
        save(
            &ws,
            &format!("{f}/03-item.pulpo.yaml"),
            "Add item",
            HttpMethod::Post,
            "{{base}}/orders/{{orderId}}/items",
            true,
            "pm.environment.set('lastItem', 1);",
        );
        save(
            &ws,
            &format!("{f}/04-get.pulpo.yaml"),
            "Get item",
            HttpMethod::Get,
            "{{base}}/orders/{{orderId}}/items/{{itemId}}?c={{lastItem}}&x={{nowhere}}",
            true,
            "",
        );
        // The login only has a saved example.
        crate::examples::save_example(
            &ws,
            &format!("{f}/01-login.pulpo.yaml"),
            &crate::examples::Example {
                id: String::new(),
                name: "ok".into(),
                status: 200,
                headers: Vec::new(),
                body: r#"{"access_token":"t","expires_in":3600}"#.into(),
            },
        )
        .unwrap();

        let store = MemoryStore::default();
        let c = chain(&ws, &f, None, &store, false).await.unwrap();
        let text = to_text(&c);
        assert_eq!(c.links(), 6, "token feeds three steps: {text}");
        let link = |step: usize, var: &str| {
            c.steps[step]
                .fed
                .iter()
                .find(|l| l.variable == var)
                .cloned()
        };
        assert_eq!(link(1, "token").unwrap().path, "$.access_token");
        assert_eq!(link(1, "token").unwrap().evidence, "example");
        assert_eq!(link(2, "orderId").unwrap().path, "$.id");
        assert_eq!(link(2, "orderId").unwrap().evidence, "contract");
        assert_eq!(link(3, "itemId").unwrap().path, "$.itemId");
        assert!(link(3, "lastItem").is_none(), "set by a script");
        let unresolved: Vec<&str> = c.unresolved.iter().map(|(_, v)| v.as_str()).collect();
        assert_eq!(unresolved, vec!["base", "nowhere"], "each once");
        assert!(c.unresolved.iter().any(|(_, v)| v == "nowhere"));
        assert!(
            text.contains("{{orderId}} ← step 2 $.id  (contract)"),
            "{text}"
        );

        let flow = to_flow(&c, "Order an item");
        let crate::flow::Step::Request { save, .. } = &flow.steps[1] else {
            panic!()
        };
        assert_eq!(save["orderId"], "$.id");
        let crate::flow::Step::Request { save, .. } = &flow.steps[0] else {
            panic!()
        };
        assert_eq!(save["token"], "$.access_token");
        crate::flow::save_flow(&ws, &flow).unwrap();
        assert_eq!(crate::flow::plan(&ws, "order-an-item").unwrap().len(), 4);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_responses_find_what_nothing_documents() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let (status, body) = if text.starts_with("POST /carts") {
                    ("201 Created", r#"{"cart":{"cartId":"c-9"}}"#)
                } else if text.starts_with("GET /carts/c-9 ") {
                    ("200 OK", r#"{"ok":true}"#)
                } else {
                    ("404 Not Found", "{}")
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        init_workspace(ws, "Shop").unwrap();
        save(
            ws,
            "01-cart.pulpo.yaml",
            "New cart",
            HttpMethod::Post,
            &format!("{base}/carts"),
            false,
            "",
        );
        save(
            ws,
            "02-get.pulpo.yaml",
            "See cart",
            HttpMethod::Get,
            &format!("{base}/carts/{{{{cartId}}}}"),
            false,
            "",
        );
        let store = MemoryStore::default();
        let blind = chain(ws, "", None, &store, false).await.unwrap();
        assert_eq!(blind.links(), 0);
        let c = chain(ws, "", None, &store, true).await.unwrap();
        assert_eq!(c.steps[1].fed[0].path, "$.cart.cartId");
        assert_eq!(c.steps[1].fed[0].evidence, "response");
        assert_eq!(
            c.steps[1].status,
            Some(200),
            "the found value was used: {}",
            to_text(&c)
        );
        let flow = to_flow(&c, "Cart");
        let crate::flow::Step::Request { expect, .. } = &flow.steps[1] else {
            panic!()
        };
        assert_eq!(expect.as_ref().and_then(|e| e.status), Some(200));
    }
}
