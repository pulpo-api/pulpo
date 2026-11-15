//! The collection comes from the code: find the API contracts of the repository
//! (`scan`) and keep the folders generated from an OpenAPI document in step with it
//! (`drift` and `apply`): new operations, operations that are gone, and requests that
//! miss something the contract now asks for.
//!
//! A folder remembers its contract in `_folder.yaml` (`source:`). Requests are matched
//! by method and path, so a collection imported from Postman can be linked too.

use crate::collection::{self, CollectionError, RequestEntry, TreeNode};
use crate::http::{Header, HttpRequest};
use crate::import::{self, ImportFormat, ImportNode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

const SKIPPED_DIRS: [&str; 14] = [
    ".git",
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "bin",
    "obj",
    "vendor",
    ".gradle",
    ".idea",
    ".pulpo",
    ".venv",
    "__pycache__",
];
const MAX_DEPTH: usize = 10;
const MAX_FILE: u64 = 5 * 1024 * 1024;

// --------------------------------------------------------------------- scan

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contract {
    /// Relative to the workspace, with `/` (it may start with `../`).
    pub path: String,
    /// `openapi`, `swagger`, `proto`, `wsdl` or `graphql`.
    pub kind: String,
    pub title: String,
    /// Operations (OpenAPI), RPCs (proto), operations (WSDL) or types (GraphQL).
    pub operations: usize,
    /// Pulpo can import and sync it today.
    pub supported: bool,
    /// The folder already generated from it, if any.
    pub folder: Option<String>,
    /// Why it is not supported, or that it only holds shared components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `to` relative to the directory `from`, with `/`.
pub fn relative_path(from: &Path, to: &Path) -> String {
    let from = fs::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
    let to = fs::canonicalize(to).unwrap_or_else(|_| to.to_path_buf());
    let a: Vec<Component> = from.components().collect();
    let b: Vec<Component> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return to.to_string_lossy().replace('\\', "/");
    }
    let mut parts: Vec<String> = vec!["..".to_string(); a.len() - common];
    parts.extend(
        b[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Where to look: the git repository the workspace is in, or the workspace itself.
pub fn scan_root(root: &Path) -> PathBuf {
    crate::git::git(root, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|top| PathBuf::from(top.trim()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| root.to_path_buf())
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                walk(&path, depth + 1, out);
            }
        } else {
            out.push(path);
        }
    }
}

fn openapi_operations(doc: &serde_json::Value) -> usize {
    const METHODS: [&str; 8] = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];
    doc.get("paths")
        .and_then(|p| p.as_object())
        .map(|paths| {
            paths
                .values()
                .map(|item| METHODS.iter().filter(|m| item.get(**m).is_some()).count())
                .sum()
        })
        .unwrap_or(0)
}

fn inspect(path: &Path) -> Option<(String, String, usize, Option<String>)> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if fs::metadata(path)
        .map(|m| m.len() > MAX_FILE)
        .unwrap_or(true)
    {
        return None;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match ext.as_str() {
        "yaml" | "yml" | "json" => {
            let text = fs::read_to_string(path).ok()?;
            let head: String = text.chars().take(4000).collect();
            let looks = |key: &str| {
                head.lines().any(|l| {
                    let t = l.trim_start_matches(|c: char| c == '{' || c.is_whitespace());
                    t.starts_with(&format!("{key}:")) || t.starts_with(&format!("\"{key}\""))
                })
            };
            let kind = if looks("openapi") {
                "openapi"
            } else if looks("swagger") {
                "swagger"
            } else {
                return None;
            };
            let doc: serde_json::Value = if ext == "json" {
                serde_json::from_str(&text).ok()?
            } else {
                serde_norway::from_str(&text).ok()?
            };
            let version = doc
                .get(kind)
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            if kind == "openapi" && !version.starts_with('3') {
                return None;
            }
            let title = doc
                .pointer("/info/title")
                .and_then(|t| t.as_str())
                .unwrap_or(&stem)
                .to_string();
            let ops = openapi_operations(&doc);
            let note = if kind == "swagger" {
                Some("Swagger 2.0: convert it to OpenAPI 3 to import it".to_string())
            } else if ops == 0 {
                Some("no operations: shared components only".to_string())
            } else {
                None
            };
            Some((kind.into(), title, ops, note))
        }
        "proto" => {
            let text = fs::read_to_string(path).ok()?;
            let rpcs = text
                .lines()
                .filter(|l| l.trim_start().starts_with("rpc "))
                .count();
            Some((
                "proto".into(),
                stem,
                rpcs,
                Some("gRPC comes with B5.4".into()),
            ))
        }
        "wsdl" => {
            let text = fs::read_to_string(path).ok()?;
            let ops = text
                .matches("<wsdl:operation")
                .count()
                .max(text.matches("<operation").count())
                / 2;
            Some((
                "wsdl".into(),
                stem,
                ops,
                Some("SOAP comes with B5.6".into()),
            ))
        }
        "graphql" | "graphqls" => {
            let text = fs::read_to_string(path).ok()?;
            let types = text
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    t.starts_with("type ") || t.starts_with("input ") || t.starts_with("enum ")
                })
                .count();
            Some((
                "graphql".into(),
                stem,
                types,
                Some("GraphQL comes with B5.1".into()),
            ))
        }
        _ => None,
    }
}

/// Folder id → its `source:`, for every folder of the workspace (and `""`).
pub fn sources(root: &Path) -> Result<Vec<(String, String)>, CollectionError> {
    let workspace = collection::load_workspace(root)?;
    let mut out = Vec::new();
    if let Some(source) = collection::folder_meta(root, "")?.source {
        out.push((String::new(), source));
    }
    fn walk_tree(root: &Path, nodes: &[TreeNode], out: &mut Vec<(String, String)>) {
        for node in nodes {
            if let TreeNode::Folder { id, children, .. } = node {
                if let Ok(Some(source)) = collection::folder_meta(root, id).map(|m| m.source) {
                    out.push((id.clone(), source));
                }
                walk_tree(root, children, out);
            }
        }
    }
    walk_tree(root, &workspace.tree, &mut out);
    Ok(out)
}

fn same_file(root: &Path, a: &str, b: &Path) -> bool {
    let a = fs::canonicalize(root.join(a)).ok();
    let b = fs::canonicalize(b).ok();
    a.is_some() && a == b
}

/// API contracts in the repository of the workspace, OpenAPI first.
pub fn scan(root: &Path) -> Result<Vec<Contract>, CollectionError> {
    let base = scan_root(root);
    let linked = sources(root)?;
    let mut files = Vec::new();
    walk(&base, 0, &mut files);
    let mut out: Vec<Contract> = files
        .iter()
        .filter_map(|file| {
            let (kind, title, operations, note) = inspect(file)?;
            let folder = linked
                .iter()
                .find(|(_, source)| same_file(root, source, file))
                .map(|(id, _)| id.clone());
            Some(Contract {
                path: relative_path(root, file),
                supported: kind == "openapi" && operations > 0,
                kind,
                title,
                operations,
                folder,
                note,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (!a.supported, a.kind.as_str(), a.path.as_str()).cmp(&(
            !b.supported,
            b.kind.as_str(),
            b.path.as_str(),
        ))
    });
    Ok(out)
}

// --------------------------------------------------------------------- drift

/// Method and path segments, path parameters as `{}` (`GET orders/{}`).
pub(crate) fn key_of(request: &HttpRequest) -> (String, Vec<String>) {
    let url = request
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    let path = if let Some(i) = url.find("://") {
        let rest = &url[i + 3..];
        rest.find('/').map(|j| &rest[j..]).unwrap_or("")
    } else {
        url
    };
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // `{{baseUrl}}/orders`: the first segment is the server, not part of the path.
    if segments
        .first()
        .is_some_and(|s| s.starts_with("{{") && s.ends_with("}}") && url.starts_with("{{"))
    {
        segments.remove(0);
    }
    let segments = segments
        .into_iter()
        .map(|s| {
            let param = s.starts_with(':')
                || (s.starts_with('{') && s.ends_with('}'))
                || (s.starts_with("{{") && s.ends_with("}}"));
            if param {
                "{}".to_string()
            } else {
                s.to_string()
            }
        })
        .collect();
    (request.method.to_string(), segments)
}

fn label(key: &(String, Vec<String>)) -> String {
    format!("{} /{}", key.0, key.1.join("/"))
}

fn query_names(url: &str) -> HashSet<String> {
    url.split_once('?')
        .map(|(_, q)| {
            q.split('#')
                .next()
                .unwrap_or_default()
                .split('&')
                .filter_map(|p| p.split('=').next())
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftItem {
    /// `POST /orders/{}`: what the contract and the request have in common.
    pub key: String,
    pub name: String,
    pub method: String,
    pub url: String,
    /// Request id (removed and changed) or the folder it would go to (added).
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Drift {
    pub folder: String,
    pub source: String,
    pub title: String,
    pub added: Vec<DriftItem>,
    pub removed: Vec<DriftItem>,
    pub changed: Vec<DriftItem>,
    pub unchanged: usize,
    /// Requests that call an operation another request already covers.
    #[serde(default)]
    pub repeated: usize,
    pub warnings: Vec<String>,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// An operation of the contract, as the importer would create it.
struct Operation {
    key: (String, Vec<String>),
    folder: Option<String>,
    name: String,
    request: HttpRequest,
    scripts: collection::Scripts,
}

fn operations_of(spec: &Path) -> Result<(String, Vec<Operation>, Vec<String>), String> {
    let plan = import::parse_file(spec).map_err(|e| e.to_string())?;
    if plan.format != ImportFormat::OpenApi {
        return Err(format!(
            "{} is a {}, not an OpenAPI document",
            spec.display(),
            plan.format.label()
        ));
    }
    let mut out = Vec::new();
    fn collect(nodes: &[ImportNode], folder: Option<&str>, out: &mut Vec<Operation>) {
        for node in nodes {
            match node {
                ImportNode::Folder { name, children, .. } => collect(children, Some(name), out),
                ImportNode::Request {
                    name,
                    request,
                    scripts,
                } => out.push(Operation {
                    key: key_of(request),
                    folder: folder.map(str::to_string),
                    name: name.clone(),
                    request: request.clone(),
                    scripts: scripts.clone(),
                }),
            }
        }
    }
    collect(&plan.nodes, None, &mut out);
    Ok((plan.folder.unwrap_or_default(), out, plan.warnings))
}

/// A request matches an operation when the methods are equal and the request's path
/// ends with the operation's (the server part of the URL may hold a prefix).
pub(crate) fn matches(request: &(String, Vec<String>), operation: &(String, Vec<String>)) -> bool {
    request.0 == operation.0
        && !operation.1.is_empty()
        && request.1.len() >= operation.1.len()
        && request.1[request.1.len() - operation.1.len()..] == operation.1[..]
        || (request.0 == operation.0 && request.1.is_empty() && operation.1.is_empty())
}

/// What a request misses that the contract now asks for.
/// What a request misses that the contract now asks for. Headers count when the
/// request sends them itself, inherits them from a folder, or a script mentions them.
fn missing_in(root: &Path, entry: &RequestEntry, operation: &HttpRequest) -> Vec<String> {
    let mut request = entry.request.clone();
    if let Ok(headers) = collection::effective_headers(root, entry) {
        request.headers = headers;
    }
    let scripts = format!("{}\n{}", entry.scripts.pre, entry.scripts.test).to_lowercase();
    missing(&request, operation)
        .into_iter()
        .filter(|m| {
            m.strip_prefix("required header ")
                .is_none_or(|name| !scripts.contains(&name.to_lowercase()))
        })
        .collect()
}

fn missing(request: &HttpRequest, operation: &HttpRequest) -> Vec<String> {
    let mut out = Vec::new();
    let have: HashSet<String> = query_names(&request.url)
        .into_iter()
        .chain(request.disabled_params.iter().map(|p| p.name.clone()))
        .collect();
    for name in query_names(&operation.url) {
        if !have.contains(&name) {
            out.push(format!("required query param {name}"));
        }
    }
    for header in operation.headers.iter().filter(|h| h.enabled) {
        let present = request
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(&header.name));
        if !present && !header.name.eq_ignore_ascii_case("content-type") {
            out.push(format!("required header {}", header.name));
        }
    }
    let has_body = |r: &HttpRequest| {
        r.form.is_some()
            || r.body_file.is_some()
            || r.body.as_deref().is_some_and(|b| !b.trim().is_empty())
    };
    if has_body(operation) && !has_body(request) {
        out.push("a request body".into());
    }
    out
}

fn requests_in(root: &Path, folder_id: &str) -> Result<Vec<RequestEntry>, String> {
    let workspace = collection::load_workspace(root).map_err(|e| e.to_string())?;
    Ok(crate::run::requests_under(&workspace.tree, folder_id))
}

fn spec_path(root: &Path, source: &str) -> PathBuf {
    root.join(source)
}

/// Compares a folder with the contract it comes from (its `source:`, or `source` when
/// given, to link it first).
pub fn drift(root: &Path, folder_id: &str, source: Option<&str>) -> Result<Drift, String> {
    let source = match source {
        Some(s) => s.to_string(),
        None => collection::folder_meta(root, folder_id)
            .map_err(|e| e.to_string())?
            .source
            .ok_or_else(|| {
                format!(
                    "{} is not linked to a contract; give one (pulpo sync {} --from <openapi file>)",
                    if folder_id.is_empty() { "the workspace" } else { folder_id },
                    folder_id
                )
            })?,
    };
    let spec = spec_path(root, &source);
    if !spec.is_file() {
        return Err(format!(
            "the contract {source} does not exist (moved or deleted?)"
        ));
    }
    let (title, operations, warnings) = operations_of(&spec)?;
    let requests = requests_in(root, folder_id)?;
    let keys: Vec<(String, Vec<String>)> = requests.iter().map(|r| key_of(&r.request)).collect();
    let mut used = vec![false; requests.len()];
    let mut drift = Drift {
        folder: folder_id.to_string(),
        source,
        title,
        warnings,
        ..Default::default()
    };
    for op in &operations {
        // The closest match: same method, shortest extra prefix.
        let found = keys
            .iter()
            .enumerate()
            .filter(|(i, k)| !used[*i] && matches(k, &op.key))
            .min_by_key(|(_, k)| k.1.len());
        match found {
            Some((i, _)) => {
                used[i] = true;
                let changes = missing_in(root, &requests[i], &op.request);
                if changes.is_empty() {
                    drift.unchanged += 1;
                } else {
                    drift.changed.push(DriftItem {
                        key: label(&op.key),
                        name: requests[i].name.clone(),
                        method: op.key.0.clone(),
                        url: requests[i].request.url.clone(),
                        target: requests[i].id.clone(),
                        changes,
                    });
                }
            }
            None => drift.added.push(DriftItem {
                key: label(&op.key),
                name: op.name.clone(),
                method: op.key.0.clone(),
                url: op.request.url.clone(),
                target: op.folder.clone().unwrap_or_default(),
                changes: Vec::new(),
            }),
        }
    }
    for (i, request) in requests.iter().enumerate() {
        if used[i] {
            continue;
        }
        // The same call again (a flow-like folder calls an endpoint several times).
        if operations.iter().any(|op| matches(&keys[i], &op.key)) {
            drift.repeated += 1;
            continue;
        }
        // A new operation with the same method, ending and parameters is probably where
        // this one moved to.
        let ending = |k: &(String, Vec<String>)| {
            (
                k.0.clone(),
                k.1.iter().rev().find(|s| *s != "{}").cloned(),
                k.1.iter().filter(|s| *s == "{}").count(),
            )
        };
        let hint = drift
            .added
            .iter()
            .find(|a| {
                operations
                    .iter()
                    .find(|o| label(&o.key) == a.key)
                    .is_some_and(|o| ending(&o.key) == ending(&keys[i]))
            })
            .map(|a| format!("maybe moved to {}", a.key));
        drift.removed.push(DriftItem {
            key: label(&keys[i]),
            name: request.name.clone(),
            method: keys[i].0.clone(),
            url: request.request.url.clone(),
            target: request.id.clone(),
            changes: hint.into_iter().collect(),
        });
    }
    Ok(drift)
}

/// What to apply: operations to add (by key), requests to update and to delete (by id).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub update: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

impl Selection {
    /// Everything that does not delete: add and update all.
    pub fn safe(drift: &Drift) -> Selection {
        Selection {
            add: drift.added.iter().map(|d| d.key.clone()).collect(),
            update: drift.changed.iter().map(|d| d.target.clone()).collect(),
            remove: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
}

fn add_missing(request: &mut HttpRequest, operation: &HttpRequest) {
    let have: HashSet<String> = query_names(&request.url)
        .into_iter()
        .chain(request.disabled_params.iter().map(|p| p.name.clone()))
        .collect();
    let extra: Vec<String> = operation
        .url
        .split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .filter(|p| !have.contains(p.split('=').next().unwrap_or_default()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if !extra.is_empty() {
        let (base, hash) = match request.url.find('#') {
            Some(i) => (request.url[..i].to_string(), request.url[i..].to_string()),
            None => (request.url.clone(), String::new()),
        };
        let sep = if base.contains('?') { '&' } else { '?' };
        request.url = format!("{base}{sep}{}{hash}", extra.join("&"));
    }
    for header in operation.headers.iter().filter(|h| h.enabled) {
        if !request
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(&header.name))
        {
            request
                .headers
                .push(Header::new(header.name.clone(), header.value.clone()));
        }
    }
    let empty = request.form.is_none()
        && request.body_file.is_none()
        && request.body.as_deref().is_none_or(|b| b.trim().is_empty());
    if empty {
        request.body = operation.body.clone();
        request.form = operation.form.clone();
        request.body_file = operation.body_file.clone();
    }
}

/// The start of a request's URL that comes before the operation's path: the server
/// (`{{distributionV2}}`, `https://api.shop`) plus any extra path segments.
fn base_of(url: &str, extra_segments: usize) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let (origin, rest) = if let Some(i) = url.find("://") {
        match url[i + 3..].find('/') {
            Some(j) => (&url[..i + 3 + j], &url[i + 3 + j..]),
            None => (url, ""),
        }
    } else if url.starts_with("{{") {
        match url.find("}}") {
            Some(j) => (&url[..j + 2], &url[j + 2..]),
            None => ("", url),
        }
    } else {
        ("", url)
    };
    let segments: Vec<&str> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .take(extra_segments)
        .collect();
    if segments.is_empty() {
        origin.to_string()
    } else {
        format!("{origin}/{}", segments.join("/"))
    }
}

/// The base most requests of the folder use for this contract, if they agree on one.
fn common_base(requests: &[RequestEntry], operations: &[Operation]) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for r in requests {
        let key = key_of(&r.request);
        if let Some(op) = operations
            .iter()
            .filter(|o| matches(&key, &o.key))
            .max_by_key(|o| o.key.1.len())
        {
            let base = base_of(&r.request.url, key.1.len() - op.key.1.len());
            match counts.iter_mut().find(|(b, _)| *b == base) {
                Some((_, n)) => *n += 1,
                None => counts.push((base, 1)),
            }
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(b, _)| b)
        .filter(|b| !b.is_empty())
}

/// Applies the selected parts of the drift. Updates only add what is missing: nothing a
/// person wrote in a request is changed or lost.
pub fn apply(root: &Path, folder_id: &str, selection: &Selection) -> Result<Applied, String> {
    let current = drift(root, folder_id, None)?;
    let (_, operations, _) = operations_of(&spec_path(root, &current.source))?;
    let mut applied = Applied::default();
    let workspace = collection::load_workspace(root).map_err(|e| e.to_string())?;
    let children: Vec<(String, String)> = {
        let nodes = if folder_id.is_empty() {
            workspace.tree.clone()
        } else {
            fn find(nodes: &[TreeNode], id: &str) -> Option<Vec<TreeNode>> {
                nodes.iter().find_map(|n| match n {
                    TreeNode::Folder {
                        id: fid, children, ..
                    } if fid == id => Some(children.clone()),
                    TreeNode::Folder { children, .. } => find(children, id),
                    _ => None,
                })
            }
            find(&workspace.tree, folder_id).unwrap_or_default()
        };
        nodes
            .iter()
            .filter_map(|n| match n {
                TreeNode::Folder { id, name, .. } => Some((name.to_lowercase(), id.clone())),
                _ => None,
            })
            .collect()
    };
    let own_name = collection::folder_meta(root, folder_id)
        .map(|m| m.name.to_lowercase())
        .unwrap_or_default();
    let mut created: Vec<(String, String)> = Vec::new();
    // New requests use the base the folder already uses, not the contract's {{baseUrl}}.
    let base = common_base(&requests_in(root, folder_id)?, &operations);
    for item in current
        .added
        .iter()
        .filter(|d| selection.add.contains(&d.key))
    {
        let Some(op) = operations.iter().find(|o| label(&o.key) == item.key) else {
            continue;
        };
        let target = match &op.folder {
            None => folder_id.to_string(),
            // A tag named like the linked folder itself: straight into it.
            Some(name) if name.to_lowercase() == own_name => folder_id.to_string(),
            Some(name) => {
                let lower = name.to_lowercase();
                match children
                    .iter()
                    .chain(created.iter())
                    .find(|(n, _)| *n == lower)
                {
                    Some((_, id)) => id.clone(),
                    None => {
                        let id = collection::create_folder(root, folder_id, name)
                            .map_err(|e| e.to_string())?;
                        created.push((lower, id.clone()));
                        id
                    }
                }
            }
        };
        let entry =
            collection::create_request(root, &target, &op.name).map_err(|e| e.to_string())?;
        let mut request = op.request.clone();
        if let Some(base) = &base {
            if let Some(rest) = request.url.strip_prefix("{{baseUrl}}") {
                request.url = format!("{base}{rest}");
            }
        }
        collection::save_request(
            root,
            &RequestEntry {
                request,
                scripts: op.scripts.clone(),
                ..entry.clone()
            },
        )
        .map_err(|e| e.to_string())?;
        applied.added.push(entry.id);
    }
    for item in current
        .changed
        .iter()
        .filter(|d| selection.update.contains(&d.target))
    {
        let mut entry = collection::load_request(root, &item.target).map_err(|e| e.to_string())?;
        let op_key = key_of(&entry.request);
        let Some(op) = operations
            .iter()
            .filter(|o| matches(&op_key, &o.key))
            .max_by_key(|o| o.key.1.len())
        else {
            continue;
        };
        add_missing(&mut entry.request, &op.request);
        collection::save_request(root, &entry).map_err(|e| e.to_string())?;
        applied.updated.push(entry.id);
    }
    for item in current
        .removed
        .iter()
        .filter(|d| selection.remove.contains(&d.target))
    {
        collection::delete_request(root, &item.target).map_err(|e| e.to_string())?;
        applied.removed.push(item.target.clone());
    }
    Ok(applied)
}

/// After importing an OpenAPI file, links the new folder to it so `pulpo sync` can
/// follow the contract from then on.
pub fn remember(root: &Path, folder_id: &str, format: ImportFormat, file: &Path) {
    if format == ImportFormat::OpenApi {
        let _ = collection::set_folder_source(root, folder_id, Some(&relative_path(root, file)));
    }
}

/// Links a folder to a contract and returns the drift.
pub fn link(root: &Path, folder_id: &str, contract: &Path) -> Result<Drift, String> {
    if !contract.is_file() {
        return Err(format!("{} does not exist", contract.display()));
    }
    let source = relative_path(root, contract);
    let result = drift(root, folder_id, Some(&source))?;
    collection::set_folder_source(root, folder_id, Some(&source)).map_err(|e| e.to_string())?;
    Ok(result)
}

/// Text for the terminal.
pub fn to_text(d: &Drift) -> String {
    let where_ = if d.folder.is_empty() {
        "the workspace".to_string()
    } else {
        d.folder.clone()
    };
    if d.is_empty() {
        return format!(
            "{where_} is in step with {} ({} operations).\n",
            d.source, d.unchanged
        );
    }
    let mut out = format!(
        "{where_} vs {} ({}): {} new, {} gone, {} to update, {} in step{}\n",
        d.source,
        d.title,
        d.added.len(),
        d.removed.len(),
        d.changed.len(),
        d.unchanged,
        if d.repeated > 0 {
            format!(" (+{} repeated calls)", d.repeated)
        } else {
            String::new()
        }
    );
    for i in &d.added {
        out.push_str(&format!("+ {:<7} {}  ({})\n", i.method, i.name, i.key));
    }
    for i in &d.removed {
        out.push_str(&format!("- {:<7} {}  {}\n", i.method, i.name, i.target));
        for hint in &i.changes {
            out.push_str(&format!("      {hint}\n"));
        }
    }
    for i in &d.changed {
        out.push_str(&format!(
            "~ {:<7} {}  misses {}\n",
            i.method,
            i.name,
            i.changes.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{create_folder, init_workspace, save_request, Scripts};
    use crate::HttpMethod;

    const SPEC_V1: &str = r#"openapi: 3.0.3
info: { title: Shop, version: "1" }
servers: [{ url: "http://shop.local/api/v1" }]
paths:
  /orders:
    get:
      tags: [Orders]
      summary: List orders
      parameters: [{ name: status, in: query, required: true, schema: { type: string } }]
    post:
      tags: [Orders]
      summary: Create order
      requestBody: { content: { application/json: { example: { item: 1 } } } }
  /orders/{orderId}:
    get: { tags: [Orders], summary: Get order }
"#;

    fn request(root: &Path, id: &str, name: &str, method: HttpMethod, url: &str) {
        save_request(
            root,
            &RequestEntry {
                id: id.into(),
                name: name.into(),
                protocol: crate::Protocol::Http,
                request: HttpRequest::new(method, url),
                scripts: Scripts::default(),
            },
        )
        .unwrap();
    }

    #[test]
    fn keys_ignore_servers_and_parameter_styles() {
        let k = |m, u: &str| key_of(&HttpRequest::new(m, u));
        let expected = (
            "GET".to_string(),
            vec!["orders".to_string(), "{}".to_string()],
        );
        assert_eq!(
            k(HttpMethod::Get, "{{baseUrl}}/orders/{{orderId}}"),
            expected
        );
        assert_eq!(k(HttpMethod::Get, "https://x.io/orders/:id?a=1"), expected);
        assert!(matches(
            &k(HttpMethod::Get, "{{host}}/api/v1/orders/{id}"),
            &expected
        ));
        assert!(!matches(
            &k(HttpMethod::Post, "{{host}}/orders/1"),
            &expected
        ));
        assert_eq!(
            relative_path(Path::new("/a/b"), Path::new("/a/c/d.yml")),
            "../c/d.yml"
        );
    }

    #[test]
    fn finds_drift_and_applies_it_without_losing_edits() {
        // Scan looks through the git repository the workspace is in.
        let dir = crate::git::tests::repo();
        let repo = dir.path();
        fs::create_dir_all(repo.join("api")).unwrap();
        fs::write(repo.join("api/shop.yaml"), SPEC_V1).unwrap();
        fs::write(
            repo.join("api/common.yaml"),
            "openapi: 3.0.3\ninfo: { title: Common, version: '1' }\npaths: {}\ncomponents: {}\n",
        )
        .unwrap();
        fs::write(
            repo.join("api/legacy.json"),
            r#"{"swagger": "2.0", "info": {"title": "Old"}, "paths": {}}"#,
        )
        .unwrap();
        fs::write(repo.join("api/notes.yaml"), "name: not a contract\n").unwrap();
        fs::write(
            repo.join("api/orders.proto"),
            "service Orders {\n  rpc Get (A) returns (B);\n}\n",
        )
        .unwrap();
        let ws = repo.join("collection");
        fs::create_dir_all(&ws).unwrap();
        init_workspace(&ws, "Shop").unwrap();

        // A collection made by hand (as from Postman): another base, other styles.
        let folder = create_folder(&ws, "", "Shop API").unwrap();
        let orders = create_folder(&ws, &folder, "Orders").unwrap();
        request(
            &ws,
            &format!("{orders}/01-list.pulpo.yaml"),
            "Mis pedidos",
            HttpMethod::Get,
            "{{host}}/api/v1/orders",
        );
        request(
            &ws,
            &format!("{orders}/02-get.pulpo.yaml"),
            "Un pedido",
            HttpMethod::Get,
            "{{host}}/api/v1/orders/:id",
        );
        request(
            &ws,
            &format!("{orders}/03-old.pulpo.yaml"),
            "Cancelar",
            HttpMethod::Delete,
            "{{host}}/api/v1/orders/:id",
        );

        // Scan finds the contracts, OpenAPI first, and skips what is not one.
        let found = scan(&ws).unwrap();
        let summary: Vec<(String, bool, usize)> = found
            .iter()
            .map(|c| (c.path.clone(), c.supported, c.operations))
            .collect();
        assert_eq!(
            summary[0],
            ("../api/shop.yaml".to_string(), true, 3),
            "{found:#?}"
        );
        assert!(found.iter().any(|c| c.kind == "proto" && c.operations == 1));
        assert!(found.iter().any(|c| c.kind == "swagger" && !c.supported));
        assert!(found.iter().any(|c| c.path.ends_with("common.yaml")
            && c.note.as_deref().unwrap().contains("components")));
        assert!(!found.iter().any(|c| c.path.ends_with("notes.yaml")));

        let d = link(&ws, &folder, &repo.join("api/shop.yaml")).unwrap();
        assert_eq!(
            collection::folder_meta(&ws, &folder)
                .unwrap()
                .source
                .as_deref(),
            Some("../api/shop.yaml")
        );
        assert_eq!(
            scan(&ws).unwrap()[0].folder.as_deref(),
            Some(folder.as_str())
        );
        assert_eq!(d.added.len(), 1, "{d:#?}");
        assert_eq!(d.added[0].key, "POST /orders");
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].name, "Cancelar");
        assert_eq!(d.changed.len(), 1);
        assert_eq!(
            d.changed[0].changes,
            vec!["required query param status".to_string()]
        );
        assert_eq!(d.unchanged, 1);
        assert!(
            to_text(&d).contains("1 new, 1 gone, 1 to update, 1 in step"),
            "{}",
            to_text(&d)
        );

        // Apply the safe part: add and update, nothing deleted; names kept.
        let applied = apply(&ws, &folder, &Selection::safe(&d)).unwrap();
        assert_eq!(applied.added.len(), 1);
        assert!(
            applied.added[0].starts_with(&orders),
            "goes into the Orders folder: {applied:?}"
        );
        let list = collection::load_request(&ws, &format!("{orders}/01-list.pulpo.yaml")).unwrap();
        assert_eq!(list.name, "Mis pedidos");
        assert_eq!(list.request.url, "{{host}}/api/v1/orders?status={{status}}");
        let created = collection::load_request(&ws, &applied.added[0]).unwrap();
        assert_eq!(
            created.request.url, "{{host}}/api/v1/orders",
            "uses the folder's base"
        );
        assert!(created.request.body.unwrap().contains("item"));
        assert_eq!(
            base_of("https://x.io/api/v1/orders/1?a=1", 2),
            "https://x.io/api/v1"
        );
        assert_eq!(
            base_of("{{distributionV2}}/api/v2/x", 0),
            "{{distributionV2}}"
        );
        let after = drift(&ws, &folder, None).unwrap();
        assert!(
            after.added.is_empty() && after.changed.is_empty() && after.removed.len() == 1,
            "{after:#?}"
        );

        // Directories that are not part of the collection stay out of the tree.
        fs::create_dir_all(ws.join("docs")).unwrap();
        fs::write(ws.join("docs/readme.md"), "notes").unwrap();
        assert!(!collection::load_workspace(&ws)
            .unwrap()
            .tree
            .iter()
            .any(|n| matches!(n, TreeNode::Folder { name, .. } if name == "docs")));

        // Deleting is a separate, explicit choice.
        let removed = apply(
            &ws,
            &folder,
            &Selection {
                remove: vec![after.removed[0].target.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(removed.removed.len(), 1);
        assert!(drift(&ws, &folder, None).unwrap().is_empty());

        // Editing the folder in the app keeps the link.
        collection::save_folder_meta(&ws, &folder, &collection::FolderMeta::default()).unwrap();
        assert!(collection::folder_meta(&ws, &folder)
            .unwrap()
            .source
            .is_some());
        assert!(drift(&ws, "", None).unwrap_err().contains("not linked"));
    }
}
