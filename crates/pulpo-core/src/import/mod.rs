//! Importers: Postman collections and environments, OpenAPI 3 and curl commands.
//!
//! Each importer turns its input into an [`ImportPlan`]; [`apply`] writes the plan
//! into a workspace as regular Pulpo files.

mod curl;
mod openapi;
mod postman;

use crate::collection::{self, CollectionError, RequestEntry, Scripts};
use crate::environment::{self, Environment, SecretStore};
use crate::http::{Auth, Form, HttpRequest};
use crate::Protocol;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

pub use curl::parse_curl;

/// A folder or request to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportNode {
    Folder {
        name: String,
        children: Vec<ImportNode>,
        scripts: Scripts,
        /// `None` = inherit.
        auth: Option<Auth>,
    },
    Request {
        name: String,
        request: HttpRequest,
        scripts: Scripts,
    },
}

/// What an importer produced. Nothing is written until [`apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlan {
    pub format: ImportFormat,
    /// When set, the nodes go inside a new folder with this name.
    pub folder: Option<String>,
    /// Scripts of the top level (a Postman collection's own scripts).
    pub folder_scripts: Scripts,
    /// Auth of the top level (a Postman collection's or an OpenAPI document's global one).
    pub folder_auth: Option<Auth>,
    pub nodes: Vec<ImportNode>,
    pub environments: Vec<Environment>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportFormat {
    PostmanCollection,
    PostmanEnvironment,
    OpenApi,
    Curl,
}

impl ImportFormat {
    pub fn label(self) -> &'static str {
        match self {
            ImportFormat::PostmanCollection => "Postman collection",
            ImportFormat::PostmanEnvironment => "Postman environment",
            ImportFormat::OpenApi => "OpenAPI",
            ImportFormat::Curl => "curl command",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum ImportError {
    UnknownFormat(String),
    Invalid(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::UnknownFormat(m) => write!(f, "unrecognised format: {m}"),
            ImportError::Invalid(m) => write!(f, "invalid input: {m}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// Result of writing a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub format: Option<ImportFormat>,
    /// Id of the folder that received the import ("" for the workspace root).
    pub folder_id: String,
    pub folders: usize,
    pub requests: usize,
    pub environments: Vec<String>,
    pub warnings: Vec<String>,
}

/// Recognises the input and parses it.
pub fn parse(text: &str) -> Result<ImportPlan, ImportError> {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.starts_with("curl ") || trimmed.starts_with("curl\t") || trimmed == "curl" {
        return curl::plan(trimmed);
    }
    if trimmed.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| ImportError::Invalid(format!("JSON: {e}")))?;
        return parse_value(value);
    }
    match serde_norway::from_str::<serde_json::Value>(trimmed) {
        Ok(value) if value.is_object() => parse_value(value),
        _ => Err(ImportError::UnknownFormat(
            "expected a Postman collection or environment, an OpenAPI 3 document or a curl command"
                .into(),
        )),
    }
}

/// Like [`parse`], but reads a file and follows OpenAPI `$ref`s to sibling files.
pub fn parse_file(path: &Path) -> Result<ImportPlan, ImportError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ImportError::Invalid(format!("{}: {e}", path.display())))?;
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    let value = if trimmed.starts_with("curl") {
        None
    } else if trimmed.starts_with('{') {
        serde_json::from_str::<serde_json::Value>(trimmed).ok()
    } else {
        serde_norway::from_str::<serde_json::Value>(trimmed).ok()
    };
    match value {
        Some(mut value) if value.get("openapi").is_some() => {
            let mut warnings = Vec::new();
            let base = path.parent().unwrap_or_else(|| Path::new("."));
            openapi::inline_external_refs(
                &mut value,
                base,
                None,
                &mut Default::default(),
                &mut warnings,
                0,
            );
            let mut plan = parse_value(value)?;
            warnings.sort();
            warnings.dedup();
            plan.warnings.extend(warnings);
            Ok(plan)
        }
        _ => parse(&text),
    }
}

fn parse_value(value: serde_json::Value) -> Result<ImportPlan, ImportError> {
    let schema = value
        .pointer("/info/schema")
        .and_then(|s| s.as_str())
        .unwrap_or_default();
    if schema.contains("getpostman.com/json/collection")
        || (value.get("info").is_some() && value.get("item").is_some())
    {
        return postman::collection(&value);
    }
    if value.get("values").is_some()
        && (value.get("_postman_variable_scope").is_some() || value.get("name").is_some())
    {
        return postman::environment(&value);
    }
    if let Some(version) = value.get("openapi").and_then(|v| v.as_str()) {
        if version.starts_with('3') {
            return openapi::document(&value);
        }
    }
    if value.get("swagger").is_some() {
        return Err(ImportError::UnknownFormat(
            "Swagger 2.0 is not supported yet; convert it to OpenAPI 3 first".into(),
        ));
    }
    Err(ImportError::UnknownFormat(
        "expected a Postman collection or environment, an OpenAPI 3 document or a curl command"
            .into(),
    ))
}

/// Writes the plan into `folder_id` of the workspace at `root`.
/// Secrets that cannot be stored become warnings instead of failing the import.
pub fn apply(
    root: &Path,
    folder_id: &str,
    plan: &ImportPlan,
    store: &dyn SecretStore,
) -> Result<ImportSummary, CollectionError> {
    let mut summary = ImportSummary {
        format: Some(plan.format),
        folder_id: folder_id.to_string(),
        warnings: plan.warnings.clone(),
        ..Default::default()
    };
    let target = match &plan.folder {
        Some(name) => {
            summary.folders += 1;
            collection::create_folder(root, folder_id, name)?
        }
        None => folder_id.to_string(),
    };
    summary.folder_id = target.clone();
    if !plan.folder_scripts.is_empty() {
        collection::save_folder_scripts(root, &target, &plan.folder_scripts)?;
    }
    if plan.folder_auth.is_some() {
        collection::save_folder_auth(root, &target, plan.folder_auth.as_ref())?;
    }
    write_nodes(root, &target, &plan.nodes, &mut summary)?;

    for env in &plan.environments {
        let created =
            environment::create_environment(root, &env.name, &environment::MemoryStore::default())?;
        let mut with_values = env.clone();
        with_values.id = created.id.clone();
        if let Err(e) = environment::save_environment(root, &with_values, store) {
            let mut without_secrets = with_values.clone();
            for v in without_secrets.variables.iter_mut().filter(|v| v.secret) {
                v.value.clear();
            }
            environment::save_environment(
                root,
                &without_secrets,
                &environment::MemoryStore::default(),
            )?;
            summary.warnings.push(format!(
                "{}: secret values were not stored ({e}); set them with ⚙ or `pulpo secret set`",
                env.name
            ));
        }
        summary.environments.push(created.id);
    }
    Ok(summary)
}

fn write_nodes(
    root: &Path,
    folder_id: &str,
    nodes: &[ImportNode],
    summary: &mut ImportSummary,
) -> Result<(), CollectionError> {
    for node in nodes {
        match node {
            ImportNode::Folder {
                name,
                children,
                scripts,
                auth,
            } => {
                let id = collection::create_folder(root, folder_id, name)?;
                if !scripts.is_empty() {
                    collection::save_folder_scripts(root, &id, scripts)?;
                }
                if auth.is_some() {
                    collection::save_folder_auth(root, &id, auth.as_ref())?;
                }
                summary.folders += 1;
                write_nodes(root, &id, children, summary)?;
            }
            ImportNode::Request {
                name,
                request,
                scripts,
            } => {
                let created = collection::create_request(root, folder_id, name)?;
                collection::save_request(
                    root,
                    &RequestEntry {
                        id: created.id,
                        name: name.clone(),
                        protocol: Protocol::Http,
                        request: request.clone(),
                        scripts: scripts.clone(),
                    },
                )?;
                summary.requests += 1;
            }
        }
    }
    Ok(())
}

/// A body as importers find it: raw text, a form or a file of the workspace.
#[derive(Debug, Default)]
pub(super) struct ImportedBody {
    pub raw: Option<String>,
    pub form: Option<Form>,
    pub file: Option<String>,
}

/// A path for a file the imported request sends. Relative paths that stay inside the
/// workspace are kept; anything else (usually an absolute path on the author's machine)
/// becomes `files/<name>`, with a warning to copy the file there.
pub(super) fn workspace_path(path: &str, warnings: &mut Vec<String>, at: &str) -> String {
    let clean = path.trim().replace('\\', "/");
    let clean = clean.trim_start_matches("./").to_string();
    let inside = !clean.is_empty()
        && !clean.starts_with('/')
        && !clean.contains(':')
        && !clean.split('/').any(|part| part == "..");
    if inside {
        return clean;
    }
    let name = clean
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("upload.bin");
    let target = format!("files/{name}");
    if clean.is_empty() {
        warnings.push(format!(
            "{at}: a file to upload had no path; put it at {target}"
        ));
    } else {
        warnings.push(format!(
            "{at}: {path} is outside the workspace; copy it to {target}"
        ));
    }
    target
}

/// Standard base64, used for `Authorization: Basic`.
pub(crate) fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{init_workspace, load_workspace, TreeNode};
    use crate::environment::{list_environments, MemoryStore};

    #[test]
    fn keeps_workspace_paths_and_moves_the_rest_to_files() {
        let mut warnings = Vec::new();
        assert_eq!(
            workspace_path("./files/a.png", &mut warnings, "x"),
            "files/a.png"
        );
        assert_eq!(
            workspace_path("data\\b.csv", &mut warnings, "x"),
            "data/b.csv"
        );
        assert!(warnings.is_empty());
        assert_eq!(
            workspace_path("/Users/ana/photo.png", &mut warnings, "x"),
            "files/photo.png"
        );
        assert_eq!(
            workspace_path("C:\\Users\\ana\\cv.pdf", &mut warnings, "x"),
            "files/cv.pdf"
        );
        assert_eq!(
            workspace_path("../secret.txt", &mut warnings, "x"),
            "files/secret.txt"
        );
        assert_eq!(workspace_path("", &mut warnings, "x"), "files/upload.bin");
        assert_eq!(warnings.len(), 4);
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:pa ss"), "dXNlcjpwYSBzcw==");
    }

    #[test]
    fn rejects_unknown_input() {
        assert!(matches!(parse("hello"), Err(ImportError::UnknownFormat(_))));
        assert!(matches!(
            parse(r#"{"swagger":"2.0"}"#),
            Err(ImportError::UnknownFormat(_))
        ));
        assert!(matches!(parse("{ nope"), Err(ImportError::Invalid(_))));
    }

    #[test]
    fn applies_a_plan_with_nested_folders_and_environments() {
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let store = MemoryStore::default();
        let plan = parse(
            r#"{
              "info": {"name": "Shop", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
              "variable": [{"key": "base", "value": "https://shop"}],
              "item": [
                {"name": "Orders", "item": [
                  {"name": "List orders", "request": {"method": "GET", "url": "{{base}}/orders"}}
                ]},
                {"name": "Health", "request": "{{base}}/health"}
              ]
            }"#,
        )
        .unwrap();

        let summary = apply(ws.path(), "", &plan, &store).unwrap();

        assert_eq!((summary.folders, summary.requests), (2, 2));
        assert_eq!(summary.folder_id, "01-shop");
        let tree = load_workspace(ws.path()).unwrap().tree;
        let TreeNode::Folder { name, children, .. } = &tree[0] else {
            panic!("{tree:?}")
        };
        assert_eq!(name, "Shop");
        assert_eq!(children.len(), 2);
        let envs = list_environments(ws.path(), &store).unwrap();
        assert_eq!(envs[0].name, "Shop variables");
        assert_eq!(envs[0].variables[0].value, "https://shop");
    }

    #[test]
    fn keeps_importing_when_secrets_cannot_be_stored() {
        struct Broken;
        impl SecretStore for Broken {
            fn get(&self, _: &str) -> Result<Option<String>, String> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &str) -> Result<(), String> {
                Err("no keychain".into())
            }
            fn delete(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let plan = parse(
            r#"{"name": "Local", "_postman_variable_scope": "environment",
                "values": [{"key": "pwd", "value": "s3cr3t", "type": "secret", "enabled": true}]}"#,
        )
        .unwrap();
        let summary = apply(ws.path(), "", &plan, &Broken).unwrap();
        assert!(summary.warnings.iter().any(|w| w.contains("no keychain")));
        let text = std::fs::read_to_string(ws.path().join("environments/local.yaml")).unwrap();
        assert!(!text.contains("s3cr3t"));
        assert!(text.contains("secret: true"));
    }
}
