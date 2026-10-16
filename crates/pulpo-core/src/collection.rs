//! Collections stored as plain files in a workspace folder.
//!
//! Layout:
//! ```text
//! my-api/
//! ├── pulpo.yaml                      workspace file
//! └── 01-orders/                      a folder (optional _folder.yaml for its name)
//!     ├── 01-create-order.pulpo.yaml  a request
//!     └── 01-create-order.body.json   its body, next to it
//! ```
//! Pulpo always writes these files in one canonical form so diffs stay small.

use crate::http::{ApiKeyLocation, Auth, Header, HttpRequest, Param};
use crate::{HttpMethod, Protocol};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const WORKSPACE_FILE: &str = "pulpo.yaml";
pub const FOLDER_FILE: &str = "_folder.yaml";
pub const REQUEST_SUFFIX: &str = ".pulpo.yaml";
pub const FORMAT_VERSION: u32 = 1;
pub const REQUEST_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/pulpo-api/pulpo/main/schemas/request.v1.schema.json";

const SKIPPED_DIRS: [&str; 5] = [".git", ".pulpo", "node_modules", "environments", "target"];

/// How a request body is stored next to the request file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    Json,
    Xml,
    Graphql,
    Text,
}

impl BodyKind {
    pub const ALL: [BodyKind; 4] = [
        BodyKind::Json,
        BodyKind::Xml,
        BodyKind::Graphql,
        BodyKind::Text,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BodyKind::Json => "json",
            BodyKind::Xml => "xml",
            BodyKind::Graphql => "graphql",
            BodyKind::Text => "text",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            BodyKind::Json => "json",
            BodyKind::Xml => "xml",
            BodyKind::Graphql => "graphql",
            BodyKind::Text => "txt",
        }
    }

    /// Guesses the kind from the request headers, falling back to the body itself.
    pub fn detect(headers: &[Header], body: &str) -> BodyKind {
        let content_type = headers
            .iter()
            .find(|h| h.enabled && h.name.eq_ignore_ascii_case("content-type"))
            .map(|h| h.value.to_ascii_lowercase())
            .unwrap_or_default();
        if content_type.contains("json") {
            BodyKind::Json
        } else if content_type.contains("xml") {
            BodyKind::Xml
        } else if content_type.contains("graphql") {
            BodyKind::Graphql
        } else if content_type.is_empty() {
            let trimmed = body.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                BodyKind::Json
            } else if trimmed.starts_with('<') {
                BodyKind::Xml
            } else {
                BodyKind::Text
            }
        } else {
            BodyKind::Text
        }
    }
}

/// A request as shown in the tree. `id` is its path relative to the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEntry {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub request: HttpRequest,
    #[serde(default)]
    pub scripts: Scripts,
}

/// Pre-request and test scripts, stored next to the request (or folder) as
/// `<stem>.pre.js` and `<stem>.test.js`. Empty scripts mean "no file".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scripts {
    #[serde(default)]
    pub pre: String,
    #[serde(default)]
    pub test: String,
}

impl Scripts {
    pub fn is_empty(&self) -> bool {
        self.pre.trim().is_empty() && self.test.trim().is_empty()
    }
}

pub const FOLDER_SCRIPT_STEM: &str = "_folder";

/// A node of the collection tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TreeNode {
    Folder {
        id: String,
        name: String,
        children: Vec<TreeNode>,
        /// Auth for the requests inside (`None` = inherit from the parent).
        #[serde(default)]
        auth: Option<Auth>,
    },
    Request(RequestEntry),
}

/// A workspace loaded from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub root: String,
    pub name: String,
    pub tree: Vec<TreeNode>,
    /// Auth for every request of the workspace, unless a folder or request says otherwise.
    #[serde(default)]
    pub auth: Option<Auth>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum CollectionError {
    NotAWorkspace(String),
    AlreadyAWorkspace(String),
    InvalidFile(String),
    InvalidPath(String),
    Io(String),
}

impl fmt::Display for CollectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CollectionError::NotAWorkspace(m) => {
                write!(f, "not a Pulpo workspace (no {WORKSPACE_FILE}): {m}")
            }
            CollectionError::AlreadyAWorkspace(m) => write!(f, "already a Pulpo workspace: {m}"),
            CollectionError::InvalidFile(m) => write!(f, "invalid file: {m}"),
            CollectionError::InvalidPath(m) => write!(f, "invalid path: {m}"),
            CollectionError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CollectionError {}

fn io_error(path: &Path, err: std::io::Error) -> CollectionError {
    CollectionError::Io(format!("{}: {err}", path.display()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceFileV1 {
    version: u32,
    name: String,
    #[serde(default)]
    auth: Option<Auth>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderFileV1 {
    version: u32,
    name: String,
    #[serde(default)]
    auth: Option<Auth>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestFileV1 {
    version: u32,
    name: String,
    protocol: Protocol,
    #[serde(default)]
    method: Option<HttpMethod>,
    url: String,
    #[serde(default)]
    headers: Vec<Header>,
    #[serde(default)]
    body: Option<BodyKind>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    path_params: Vec<Param>,
    #[serde(default)]
    disabled_params: Vec<Param>,
    #[serde(default)]
    auth: Option<Auth>,
}

fn check_version(version: u32, path: &Path) -> Result<(), CollectionError> {
    if version == FORMAT_VERSION {
        Ok(())
    } else {
        Err(CollectionError::InvalidFile(format!(
            "{}: unsupported version {version}, this Pulpo reads version {FORMAT_VERSION}",
            path.display()
        )))
    }
}

fn parse_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, CollectionError> {
    let text = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    serde_norway::from_str(&text)
        .map_err(|e| CollectionError::InvalidFile(format!("{}: {e}", path.display())))
}

// ---------------------------------------------------------------- canonical writer

/// Quotes a string as a YAML double-quoted scalar.
pub(crate) fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Writes an `auth:` block (nothing when `auth` is None = inherit).
fn auth_yaml(auth: Option<&Auth>) -> String {
    let Some(auth) = auth else {
        return String::new();
    };
    let mut out = String::from("auth:\n");
    match auth {
        Auth::None => out.push_str("  type: \"none\"\n"),
        Auth::Bearer { token } => {
            out.push_str("  type: \"bearer\"\n");
            out.push_str(&format!("  token: {}\n", quote(token)));
        }
        Auth::Basic { username, password } => {
            out.push_str("  type: \"basic\"\n");
            out.push_str(&format!("  username: {}\n", quote(username)));
            out.push_str(&format!("  password: {}\n", quote(password)));
        }
        Auth::ApiKey {
            key,
            value,
            location,
        } => {
            out.push_str("  type: \"apiKey\"\n");
            out.push_str(&format!("  key: {}\n", quote(key)));
            out.push_str(&format!("  value: {}\n", quote(value)));
            let place = match location {
                ApiKeyLocation::Header => "header",
                ApiKeyLocation::Query => "query",
            };
            out.push_str(&format!("  in: {}\n", quote(place)));
        }
    }
    out
}

fn workspace_yaml(name: &str, auth: Option<&Auth>) -> String {
    format!(
        "version: {FORMAT_VERSION}\nname: {}\n{}",
        quote(name),
        auth_yaml(auth)
    )
}

fn folder_yaml(name: &str, auth: Option<&Auth>) -> String {
    format!(
        "version: {FORMAT_VERSION}\nname: {}\n{}",
        quote(name),
        auth_yaml(auth)
    )
}

fn request_yaml(entry: &RequestEntry, body: Option<BodyKind>) -> String {
    let mut out = format!("# yaml-language-server: $schema={REQUEST_SCHEMA_URL}\n");
    out.push_str(&format!("version: {FORMAT_VERSION}\n"));
    out.push_str(&format!("name: {}\n", quote(&entry.name)));
    out.push_str(&format!("protocol: {}\n", quote(entry.protocol.as_str())));
    if entry.protocol == Protocol::Http {
        out.push_str(&format!(
            "method: {}\n",
            quote(&entry.request.method.to_string())
        ));
    }
    out.push_str(&format!("url: {}\n", quote(&entry.request.url)));
    for (key, params) in [
        ("pathParams", &entry.request.path_params),
        ("disabledParams", &entry.request.disabled_params),
    ] {
        if !params.is_empty() {
            out.push_str(&format!("{key}:\n"));
            for param in params {
                out.push_str(&format!("  - name: {}\n", quote(&param.name)));
                out.push_str(&format!("    value: {}\n", quote(&param.value)));
            }
        }
    }
    if !entry.request.headers.is_empty() {
        out.push_str("headers:\n");
        for header in &entry.request.headers {
            out.push_str(&format!("  - name: {}\n", quote(&header.name)));
            out.push_str(&format!("    value: {}\n", quote(&header.value)));
            if !header.enabled {
                out.push_str("    enabled: false\n");
            }
        }
    }
    out.push_str(&auth_yaml(entry.request.auth.as_ref()));
    if let Some(kind) = body {
        out.push_str(&format!("body: {}\n", quote(kind.as_str())));
    }
    if let Some(timeout) = entry.request.timeout_ms {
        out.push_str(&format!("timeoutMs: {timeout}\n"));
    }
    out
}

// ---------------------------------------------------------------- paths

/// Joins a workspace-relative path, refusing anything that would leave the workspace.
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, CollectionError> {
    let rel = Path::new(relative);
    if relative.is_empty() {
        return Ok(root.to_path_buf());
    }
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(CollectionError::InvalidPath(relative.to_string()));
    }
    Ok(root.join(rel))
}

fn relative_id(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn stem_of(request_file: &Path) -> String {
    let name = request_file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(REQUEST_SUFFIX)
        .unwrap_or(&name)
        .to_string()
}

fn script_path(stem_file: &Path, stem: &str, phase: &str) -> PathBuf {
    stem_file.with_file_name(format!("{stem}.{phase}.js"))
}

fn read_script(path: &Path) -> Result<String, CollectionError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(io_error(path, e)),
    }
}

fn write_script(path: &Path, source: &str) -> Result<(), CollectionError> {
    if source.trim().is_empty() {
        if path.exists() {
            fs::remove_file(path).map_err(|e| io_error(path, e))?;
        }
        Ok(())
    } else {
        fs::write(path, source).map_err(|e| io_error(path, e))
    }
}

fn read_scripts(file: &Path, stem: &str) -> Result<Scripts, CollectionError> {
    Ok(Scripts {
        pre: read_script(&script_path(file, stem, "pre"))?,
        test: read_script(&script_path(file, stem, "test"))?,
    })
}

fn write_scripts(file: &Path, stem: &str, scripts: &Scripts) -> Result<(), CollectionError> {
    write_script(&script_path(file, stem, "pre"), &scripts.pre)?;
    write_script(&script_path(file, stem, "test"), &scripts.test)
}

/// Scripts of a folder (`""` = workspace root), from `_folder.pre.js` and `_folder.test.js`.
pub fn folder_scripts(root: &Path, folder_id: &str) -> Result<Scripts, CollectionError> {
    let dir = resolve(root, folder_id)?;
    read_scripts(&dir.join(FOLDER_FILE), FOLDER_SCRIPT_STEM)
}

/// Writes the scripts of a folder (`""` = workspace root). Empty scripts remove the files.
pub fn save_folder_scripts(
    root: &Path,
    folder_id: &str,
    scripts: &Scripts,
) -> Result<(), CollectionError> {
    let dir = resolve(root, folder_id)?;
    if !dir.is_dir() {
        return Err(CollectionError::InvalidPath(folder_id.to_string()));
    }
    write_scripts(&dir.join(FOLDER_FILE), FOLDER_SCRIPT_STEM, scripts)
}

/// Auth set on a folder (`""` = the workspace, from `pulpo.yaml`). `None` = not set.
pub fn folder_auth(root: &Path, folder_id: &str) -> Result<Option<Auth>, CollectionError> {
    if folder_id.is_empty() {
        let file = root.join(WORKSPACE_FILE);
        let parsed: WorkspaceFileV1 = parse_yaml(&file)?;
        return Ok(parsed.auth);
    }
    let file = resolve(root, folder_id)?.join(FOLDER_FILE);
    if !file.is_file() {
        return Ok(None);
    }
    let parsed: FolderFileV1 = parse_yaml(&file)?;
    Ok(parsed.auth)
}

/// Sets (or clears, with `None`) the auth of a folder (`""` = the workspace).
pub fn save_folder_auth(
    root: &Path,
    folder_id: &str,
    auth: Option<&Auth>,
) -> Result<(), CollectionError> {
    if folder_id.is_empty() {
        let file = root.join(WORKSPACE_FILE);
        let parsed: WorkspaceFileV1 = parse_yaml(&file)?;
        return fs::write(&file, workspace_yaml(&parsed.name, auth))
            .map_err(|e| io_error(&file, e));
    }
    let dir = resolve(root, folder_id)?;
    if !dir.is_dir() {
        return Err(CollectionError::InvalidPath(folder_id.to_string()));
    }
    let file = dir.join(FOLDER_FILE);
    let name = if file.is_file() {
        let parsed: FolderFileV1 = parse_yaml(&file)?;
        parsed.name
    } else {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    fs::write(&file, folder_yaml(&name, auth)).map_err(|e| io_error(&file, e))
}

/// The auth a request really uses: its own, or the nearest folder's going up to the
/// workspace. Returns the auth and where it comes from (request id, folder id or "").
pub fn effective_auth(
    root: &Path,
    entry: &RequestEntry,
) -> Result<(Auth, String), CollectionError> {
    if let Some(auth) = &entry.request.auth {
        return Ok((auth.clone(), entry.id.clone()));
    }
    for folder in ancestor_folders(&entry.id).into_iter().rev() {
        if let Some(auth) = folder_auth(root, &folder)? {
            return Ok((auth, folder));
        }
    }
    Ok((Auth::None, String::new()))
}

/// The folders a request lives in, from the workspace root down: `["", "a", "a/b"]`.
pub fn ancestor_folders(request_id: &str) -> Vec<String> {
    let mut folders = vec![String::new()];
    let parts: Vec<&str> = request_id.split('/').collect();
    for i in 1..parts.len() {
        folders.push(parts[..i].join("/"));
    }
    folders
}

fn body_path(request_file: &Path, kind: BodyKind) -> PathBuf {
    request_file.with_file_name(format!(
        "{}.body.{}",
        stem_of(request_file),
        kind.extension()
    ))
}

/// Lowercase ASCII slug for file names: "Create order!" -> "create-order".
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug
    }
}

fn next_order(dir: &Path) -> Result<u32, CollectionError> {
    let mut max = 0;
    for entry in fs::read_dir(dir).map_err(|e| io_error(dir, e))? {
        let entry = entry.map_err(|e| io_error(dir, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u32>() {
            max = max.max(n);
        }
    }
    Ok(max + 1)
}

// ---------------------------------------------------------------- loading

/// Creates `pulpo.yaml` in an existing folder.
pub fn init_workspace(root: &Path, name: &str) -> Result<(), CollectionError> {
    let file = root.join(WORKSPACE_FILE);
    if file.exists() {
        return Err(CollectionError::AlreadyAWorkspace(
            root.display().to_string(),
        ));
    }
    fs::create_dir_all(root).map_err(|e| io_error(root, e))?;
    fs::write(&file, workspace_yaml(name, None)).map_err(|e| io_error(&file, e))
}

/// Reads the whole workspace tree.
pub fn load_workspace(root: &Path) -> Result<Workspace, CollectionError> {
    let file = root.join(WORKSPACE_FILE);
    if !file.is_file() {
        return Err(CollectionError::NotAWorkspace(root.display().to_string()));
    }
    let workspace: WorkspaceFileV1 = parse_yaml(&file)?;
    check_version(workspace.version, &file)?;
    Ok(Workspace {
        root: root.display().to_string(),
        name: workspace.name,
        tree: load_dir(root, root)?,
        auth: workspace.auth,
    })
}

fn load_dir(root: &Path, dir: &Path) -> Result<Vec<TreeNode>, CollectionError> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| io_error(dir, e))?
        .collect::<Result<_, _>>()
        .map_err(|e| io_error(dir, e))?;
    entries.sort_by_key(|e| e.file_name());

    let mut nodes = Vec::new();
    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if file_name.starts_with('.') || SKIPPED_DIRS.contains(&file_name.as_str()) {
                continue;
            }
            let folder_file = path.join(FOLDER_FILE);
            let (name, auth) = if folder_file.is_file() {
                let folder: FolderFileV1 = parse_yaml(&folder_file)?;
                check_version(folder.version, &folder_file)?;
                (folder.name, folder.auth)
            } else {
                (file_name.clone(), None)
            };
            nodes.push(TreeNode::Folder {
                id: relative_id(root, &path),
                name,
                children: load_dir(root, &path)?,
                auth,
            });
        } else if file_name.ends_with(REQUEST_SUFFIX) {
            nodes.push(TreeNode::Request(load_request_file(root, &path)?));
        }
    }
    Ok(nodes)
}

fn load_request_file(root: &Path, path: &Path) -> Result<RequestEntry, CollectionError> {
    let file: RequestFileV1 = parse_yaml(path)?;
    check_version(file.version, path)?;
    let method = match (file.protocol, file.method) {
        (Protocol::Http, Some(method)) => method,
        (Protocol::Http, None) => {
            return Err(CollectionError::InvalidFile(format!(
                "{}: http requests need a method",
                path.display()
            )))
        }
        (_, _) => HttpMethod::Get,
    };
    let body = match file.body {
        Some(kind) => {
            let sidecar = body_path(path, kind);
            Some(fs::read_to_string(&sidecar).map_err(|e| io_error(&sidecar, e))?)
        }
        None => None,
    };
    Ok(RequestEntry {
        id: relative_id(root, path),
        name: file.name,
        protocol: file.protocol,
        request: HttpRequest {
            method,
            url: file.url,
            headers: file.headers,
            body,
            timeout_ms: file.timeout_ms,
            disabled_params: file.disabled_params,
            path_params: file.path_params,
            auth: file.auth,
        },
        scripts: read_scripts(path, &stem_of(path))?,
    })
}

/// Reads one request by its id.
pub fn load_request(root: &Path, id: &str) -> Result<RequestEntry, CollectionError> {
    let path = request_path(root, id)?;
    load_request_file(root, &path)
}

fn request_path(root: &Path, id: &str) -> Result<PathBuf, CollectionError> {
    if !id.ends_with(REQUEST_SUFFIX) {
        return Err(CollectionError::InvalidPath(id.to_string()));
    }
    resolve(root, id)
}

// ---------------------------------------------------------------- writing

/// Writes a request (and its body file) in canonical form. Stale body files are removed.
pub fn save_request(root: &Path, entry: &RequestEntry) -> Result<(), CollectionError> {
    let path = request_path(root, &entry.id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    let body = entry.request.body.as_deref().filter(|b| !b.is_empty());
    let kind = body.map(|b| BodyKind::detect(&entry.request.headers, b));

    for other in BodyKind::ALL {
        let sidecar = body_path(&path, other);
        if Some(other) != kind && sidecar.exists() {
            fs::remove_file(&sidecar).map_err(|e| io_error(&sidecar, e))?;
        }
    }
    if let (Some(kind), Some(body)) = (kind, body) {
        let sidecar = body_path(&path, kind);
        fs::write(&sidecar, body).map_err(|e| io_error(&sidecar, e))?;
    }
    write_scripts(&path, &stem_of(&path), &entry.scripts)?;
    fs::write(&path, request_yaml(entry, kind)).map_err(|e| io_error(&path, e))
}

/// Creates a new request at the end of a folder and returns it.
pub fn create_request(
    root: &Path,
    folder_id: &str,
    name: &str,
) -> Result<RequestEntry, CollectionError> {
    let dir = resolve(root, folder_id)?;
    if !dir.is_dir() {
        return Err(CollectionError::InvalidPath(folder_id.to_string()));
    }
    let file = dir.join(format!(
        "{:02}-{}{REQUEST_SUFFIX}",
        next_order(&dir)?,
        slugify(name)
    ));
    let entry = RequestEntry {
        id: relative_id(root, &file),
        name: name.to_string(),
        protocol: Protocol::Http,
        request: HttpRequest::new(HttpMethod::Get, "https://"),
        scripts: Scripts::default(),
    };
    save_request(root, &entry)?;
    Ok(entry)
}

/// Creates a new folder at the end of its parent and returns its id.
pub fn create_folder(root: &Path, parent_id: &str, name: &str) -> Result<String, CollectionError> {
    let parent = resolve(root, parent_id)?;
    if !parent.is_dir() {
        return Err(CollectionError::InvalidPath(parent_id.to_string()));
    }
    let dir = parent.join(format!("{:02}-{}", next_order(&parent)?, slugify(name)));
    fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    let file = dir.join(FOLDER_FILE);
    fs::write(&file, folder_yaml(name, None)).map_err(|e| io_error(&file, e))?;
    Ok(relative_id(root, &dir))
}

/// Deletes a request and its body and script files.
pub fn delete_request(root: &Path, id: &str) -> Result<(), CollectionError> {
    let path = request_path(root, id)?;
    for kind in BodyKind::ALL {
        let sidecar = body_path(&path, kind);
        if sidecar.exists() {
            fs::remove_file(&sidecar).map_err(|e| io_error(&sidecar, e))?;
        }
    }
    write_scripts(&path, &stem_of(&path), &Scripts::default())?;
    fs::remove_file(&path).map_err(|e| io_error(&path, e))
}

/// Rewrites every file in canonical form. With `check`, only reports what would change.
pub fn format_workspace(root: &Path, check: bool) -> Result<Vec<String>, CollectionError> {
    let workspace = load_workspace(root)?;
    let mut changed = Vec::new();

    let ws_file = root.join(WORKSPACE_FILE);
    rewrite(
        &ws_file,
        &workspace_yaml(&workspace.name, workspace.auth.as_ref()),
        check,
        root,
        &mut changed,
    )?;
    format_nodes(root, &workspace.tree, check, &mut changed)?;
    changed.extend(crate::environment::format_environments(root, check)?);
    Ok(changed)
}

fn format_nodes(
    root: &Path,
    nodes: &[TreeNode],
    check: bool,
    changed: &mut Vec<String>,
) -> Result<(), CollectionError> {
    for node in nodes {
        match node {
            TreeNode::Folder {
                id,
                name,
                children,
                auth,
            } => {
                let file = resolve(root, id)?.join(FOLDER_FILE);
                if file.is_file() {
                    rewrite(
                        &file,
                        &folder_yaml(name, auth.as_ref()),
                        check,
                        root,
                        changed,
                    )?;
                }
                format_nodes(root, children, check, changed)?;
            }
            TreeNode::Request(entry) => {
                let path = request_path(root, &entry.id)?;
                let body = entry.request.body.as_deref().filter(|b| !b.is_empty());
                let kind = body.map(|b| BodyKind::detect(&entry.request.headers, b));
                let on_disk_kind = BodyKind::ALL
                    .into_iter()
                    .find(|k| body_path(&path, *k).exists());
                if kind != on_disk_kind {
                    changed.push(relative_id(root, &path));
                    if !check {
                        save_request(root, entry)?;
                    }
                    continue;
                }
                rewrite(&path, &request_yaml(entry, kind), check, root, changed)?;
            }
        }
    }
    Ok(())
}

fn rewrite(
    path: &Path,
    canonical: &str,
    check: bool,
    root: &Path,
    changed: &mut Vec<String>,
) -> Result<(), CollectionError> {
    let current = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    if current != canonical {
        changed.push(relative_id(root, path));
        if !check {
            fs::write(path, canonical).map_err(|e| io_error(path, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        init_workspace(dir.path(), "Demo").unwrap();
        dir
    }

    fn sample(id: &str) -> RequestEntry {
        RequestEntry {
            id: id.to_string(),
            name: "Create \"order\": no".to_string(),
            protocol: Protocol::Http,
            request: HttpRequest {
                method: HttpMethod::Post,
                url: "{{orders}}/api/v1/checkouts?x=010".to_string(),
                headers: vec![
                    Header::new("Content-Type", "application/json"),
                    Header {
                        name: "X-Debug".into(),
                        value: "yes\tplease".into(),
                        enabled: false,
                    },
                ],
                body: Some("{\n  \"a\": 1\n}\n".to_string()),
                timeout_ms: Some(5000),
                disabled_params: vec![Param::new("debug", "true")],
                path_params: vec![Param::new("tenant", "{{tenantId}}")],
                auth: Some(Auth::Bearer {
                    token: "{{token}}".into(),
                }),
            },
            scripts: Scripts::default(),
        }
    }

    #[test]
    fn stores_request_and_folder_scripts_next_to_their_files() {
        let ws = workspace();
        let folder = create_folder(ws.path(), "", "Orders").unwrap();
        let mut entry = sample(&format!("{folder}/01-create.pulpo.yaml"));
        entry.scripts = Scripts {
            pre: "pm.variables.set('a', 1);\n".into(),
            test: "pm.test('ok', () => {});\n".into(),
        };
        save_request(ws.path(), &entry).unwrap();
        assert!(ws.path().join(&folder).join("01-create.pre.js").is_file());
        assert_eq!(
            load_request(ws.path(), &entry.id).unwrap().scripts,
            entry.scripts
        );

        entry.scripts.pre.clear();
        save_request(ws.path(), &entry).unwrap();
        assert!(!ws.path().join(&folder).join("01-create.pre.js").exists());

        let scripts = Scripts {
            pre: "console.log(1);".into(),
            test: String::new(),
        };
        save_folder_scripts(ws.path(), &folder, &scripts).unwrap();
        save_folder_scripts(ws.path(), "", &scripts).unwrap();
        assert_eq!(folder_scripts(ws.path(), &folder).unwrap(), scripts);
        assert!(ws.path().join("_folder.pre.js").is_file());
        assert_eq!(
            ancestor_folders(&entry.id),
            vec![String::new(), folder.clone()]
        );
        assert!(
            format_workspace(ws.path(), true).unwrap().is_empty(),
            "scripts are not reformatted"
        );

        delete_request(ws.path(), &entry.id).unwrap();
        assert!(!ws.path().join(&folder).join("01-create.test.js").exists());
    }

    #[test]
    fn round_trips_a_request_with_tricky_values() {
        let ws = workspace();
        let folder = create_folder(ws.path(), "", "Órdenes y pagos").unwrap();
        assert_eq!(folder, "01-ordenes-y-pagos");
        let entry = sample(&format!("{folder}/01-create-order.pulpo.yaml"));
        save_request(ws.path(), &entry).unwrap();

        assert_eq!(load_request(ws.path(), &entry.id).unwrap(), entry);
        assert!(ws
            .path()
            .join(&folder)
            .join("01-create-order.body.json")
            .is_file());

        let loaded = load_workspace(ws.path()).unwrap();
        assert_eq!(loaded.name, "Demo");
        match &loaded.tree[0] {
            TreeNode::Folder { name, children, .. } => {
                assert_eq!(name, "Órdenes y pagos");
                assert_eq!(children, &vec![TreeNode::Request(entry)]);
            }
            other => panic!("expected a folder, got {other:?}"),
        }
    }

    #[test]
    fn writes_canonical_yaml() {
        let ws = workspace();
        let entry = sample("01-create-order.pulpo.yaml");
        save_request(ws.path(), &entry).unwrap();
        let text = fs::read_to_string(ws.path().join("01-create-order.pulpo.yaml")).unwrap();
        assert_eq!(
            text,
            format!(
                "# yaml-language-server: $schema={REQUEST_SCHEMA_URL}\n\
                 version: 1\n\
                 name: \"Create \\\"order\\\": no\"\n\
                 protocol: \"http\"\n\
                 method: \"POST\"\n\
                 url: \"{{{{orders}}}}/api/v1/checkouts?x=010\"\n\
                 pathParams:\n  - name: \"tenant\"\n    value: \"{{{{tenantId}}}}\"\n\
                 disabledParams:\n  - name: \"debug\"\n    value: \"true\"\n\
                 headers:\n  - name: \"Content-Type\"\n    value: \"application/json\"\n\
                 \x20 - name: \"X-Debug\"\n    value: \"yes\\tplease\"\n    enabled: false\n\
                 auth:\n  type: \"bearer\"\n  token: \"{{{{token}}}}\"\n\
                 body: \"json\"\n\
                 timeoutMs: 5000\n"
            )
        );
    }

    #[test]
    fn removes_the_body_file_when_the_body_is_cleared() {
        let ws = workspace();
        let mut entry = sample("01-create-order.pulpo.yaml");
        save_request(ws.path(), &entry).unwrap();
        entry.request.body = None;
        save_request(ws.path(), &entry).unwrap();
        assert!(!ws.path().join("01-create-order.body.json").exists());
        assert_eq!(
            load_request(ws.path(), &entry.id).unwrap().request.body,
            None
        );
    }

    #[test]
    fn inherits_auth_from_the_nearest_folder() {
        let ws = workspace();
        let orders = create_folder(ws.path(), "", "Orders").unwrap();
        let public = create_folder(ws.path(), &orders, "Public").unwrap();
        let mut entry = create_request(ws.path(), &public, "Health").unwrap();

        assert_eq!(
            effective_auth(ws.path(), &entry).unwrap(),
            (Auth::None, String::new())
        );

        let bearer = Auth::Bearer {
            token: "{{token}}".into(),
        };
        save_folder_auth(ws.path(), "", Some(&bearer)).unwrap();
        assert_eq!(
            effective_auth(ws.path(), &entry).unwrap(),
            (bearer.clone(), String::new())
        );

        let key = Auth::ApiKey {
            key: "X-Key".into(),
            value: "{{key}}".into(),
            location: ApiKeyLocation::Header,
        };
        save_folder_auth(ws.path(), &orders, Some(&key)).unwrap();
        assert_eq!(
            effective_auth(ws.path(), &entry).unwrap(),
            (key, orders.clone())
        );

        save_folder_auth(ws.path(), &public, Some(&Auth::None)).unwrap();
        assert_eq!(
            effective_auth(ws.path(), &entry).unwrap(),
            (Auth::None, public.clone()),
            "none stops inheritance"
        );

        entry.request.auth = Some(Auth::Basic {
            username: "u".into(),
            password: "{{pw}}".into(),
        });
        save_request(ws.path(), &entry).unwrap();
        let loaded = load_request(ws.path(), &entry.id).unwrap();
        assert_eq!(effective_auth(ws.path(), &loaded).unwrap().1, entry.id);

        let tree = load_workspace(ws.path()).unwrap();
        assert_eq!(tree.auth, Some(bearer));
        assert!(format_workspace(ws.path(), true).unwrap().is_empty());
        assert_eq!(folder_auth(ws.path(), &public).unwrap(), Some(Auth::None));
        save_folder_auth(ws.path(), &public, None).unwrap();
        assert_eq!(folder_auth(ws.path(), &public).unwrap(), None);
        assert_eq!(load_workspace(ws.path()).unwrap().name, "Demo");
    }

    #[test]
    fn numbers_new_requests_after_existing_ones() {
        let ws = workspace();
        let first = create_request(ws.path(), "", "List posts").unwrap();
        let second = create_request(ws.path(), "", "Create post!").unwrap();
        assert_eq!(first.id, "01-list-posts.pulpo.yaml");
        assert_eq!(second.id, "02-create-post.pulpo.yaml");
        delete_request(ws.path(), &first.id).unwrap();
        assert!(!ws.path().join(&first.id).exists());
    }

    #[test]
    fn refuses_paths_outside_the_workspace() {
        let ws = workspace();
        let mut entry = sample("../escape.pulpo.yaml");
        assert!(matches!(
            save_request(ws.path(), &entry),
            Err(CollectionError::InvalidPath(_))
        ));
        entry.id = "/etc/escape.pulpo.yaml".into();
        assert!(matches!(
            save_request(ws.path(), &entry),
            Err(CollectionError::InvalidPath(_))
        ));
        assert!(matches!(
            create_folder(ws.path(), "..", "x"),
            Err(CollectionError::InvalidPath(_))
        ));
    }

    #[test]
    fn formats_hand_edited_files() {
        let ws = workspace();
        fs::write(
            ws.path().join("01-ping.pulpo.yaml"),
            "version: 1\nname: Ping\nprotocol: http\nmethod: GET\nurl: https://example.com\n",
        )
        .unwrap();

        assert_eq!(
            format_workspace(ws.path(), true).unwrap(),
            vec!["01-ping.pulpo.yaml"]
        );
        assert_eq!(
            format_workspace(ws.path(), false).unwrap(),
            vec!["01-ping.pulpo.yaml"]
        );
        assert!(format_workspace(ws.path(), true).unwrap().is_empty());
        let entry = load_request(ws.path(), "01-ping.pulpo.yaml").unwrap();
        assert_eq!(entry.name, "Ping");
        assert_eq!(entry.request.url, "https://example.com");
    }

    #[test]
    fn rejects_unknown_fields_and_versions() {
        let ws = workspace();
        fs::write(
            ws.path().join("01-a.pulpo.yaml"),
            "version: 1\nname: A\nprotocol: http\nmethod: GET\nurl: x\nmetod: POST\n",
        )
        .unwrap();
        assert!(matches!(
            load_workspace(ws.path()),
            Err(CollectionError::InvalidFile(_))
        ));
        fs::write(
            ws.path().join("01-a.pulpo.yaml"),
            "version: 2\nname: A\nprotocol: http\nmethod: GET\nurl: x\n",
        )
        .unwrap();
        assert!(matches!(
            load_workspace(ws.path()),
            Err(CollectionError::InvalidFile(_))
        ));
    }

    #[test]
    fn requires_a_workspace_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load_workspace(dir.path()),
            Err(CollectionError::NotAWorkspace(_))
        ));
        init_workspace(dir.path(), "x").unwrap();
        assert!(matches!(
            init_workspace(dir.path(), "x"),
            Err(CollectionError::AlreadyAWorkspace(_))
        ));
    }

    #[test]
    fn detects_body_kinds() {
        assert_eq!(
            BodyKind::detect(&[Header::new("content-type", "application/xml")], "x"),
            BodyKind::Xml
        );
        assert_eq!(BodyKind::detect(&[], "  [1, 2]"), BodyKind::Json);
        assert_eq!(BodyKind::detect(&[], "hello"), BodyKind::Text);
        assert_eq!(slugify("¡Crear pedido nº 2!"), "crear-pedido-n-2");
        assert_eq!(slugify("***"), "untitled");
    }

    #[test]
    fn loads_the_bundled_example() {
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/jsonplaceholder");
        let ws = load_workspace(&example).unwrap();
        assert_eq!(ws.name, "JSONPlaceholder");
        assert!(!ws.tree.is_empty());
        assert!(
            format_workspace(&example, true).unwrap().is_empty(),
            "example is not canonical"
        );
    }
}
