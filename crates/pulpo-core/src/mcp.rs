//! `pulpo mcp`: a Model Context Protocol server over stdio, so the developer's AI
//! (Claude Code, Cursor, Copilot…) can read, run and edit the collection of the repo.
//!
//! Secret values never reach the AI: they are replaced by `••••` in everything a tool
//! returns. Sending to an environment that looks like production needs
//! `confirmProduction: true`, which the AI should only pass after asking the person.

use crate::collection::{self, RequestEntry, Scripts, TreeNode};
use crate::environment::{self, SecretStore};
use crate::export::{self, ExportFormat};
use crate::http::{Header, HttpRequest};
use crate::run::{self, RunOptions, RunReport};
use crate::{diff, import, HttpMethod};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const PROTOCOL: &str = "2025-06-18";
/// Response bodies longer than this are cut: the AI gets the start and the size.
const MAX_BODY: usize = 20_000;

const INSTRUCTIONS: &str = "Pulpo keeps the API calls of this repository as files (a collection). \
Use list_requests to find a request, get_request to read it, run_request or run_folder to call the API \
and see the response and test results. create_request and update_request change the collection files; \
the person reviews them in git. Values are written as {{variable}} and come from environments; secret \
values are never shown. Do not set confirmProduction unless the person asked you to call production.";

/// Where the server works: a workspace and the store holding its secrets.
pub struct Server<'a> {
    root: PathBuf,
    store: &'a dyn SecretStore,
    runtime: tokio::runtime::Runtime,
}

/// The workspace at `dir`, or the only one a few levels below it (a repo whose
/// collection lives in `api/`).
pub fn find_workspace(dir: &Path) -> Option<PathBuf> {
    if dir.join(collection::WORKSPACE_FILE).is_file() {
        return Some(dir.to_path_buf());
    }
    fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        if depth == 0 || found.len() > 1 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !path.is_dir()
                || name.starts_with('.')
                || matches!(name.as_str(), "node_modules" | "target" | "dist" | "build")
            {
                continue;
            }
            if path.join(collection::WORKSPACE_FILE).is_file() {
                found.push(path);
            } else {
                walk(&path, depth - 1, found);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, 3, &mut found);
    (found.len() == 1).then(|| found.remove(0))
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn tools() -> Value {
    let env = json!({ "type": "string", "description": "Environment name (see list_environments). Omit for none." });
    let confirm = json!({ "type": "boolean", "description": "Only after the person explicitly asked to call a production environment." });
    let headers = json!({
        "type": "array",
        "items": { "type": "object", "properties": { "name": { "type": "string" }, "value": { "type": "string" } }, "required": ["name", "value"] }
    });
    json!([
        {
            "name": "list_requests",
            "description": "List the requests of the collection (id, method, name, URL), optionally inside a folder or matching words.",
            "inputSchema": { "type": "object", "properties": {
                "folder": { "type": "string", "description": "Folder id, e.g. 02-orders. Omit for all." },
                "query": { "type": "string", "description": "Words that must appear in the name, URL or folder." }
            } }
        },
        {
            "name": "get_request",
            "description": "Read one request: method, URL, headers, body, auth and scripts, as stored in the collection.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }
        },
        {
            "name": "run_request",
            "description": "Send one request with its scripts and tests and return the response (status, headers, body) and test results.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string" }, "environment": env, "confirmProduction": confirm
            }, "required": ["id"] }
        },
        {
            "name": "run_folder",
            "description": "Run every request of a folder (or the whole collection) in order, like a test suite, and return a summary.",
            "inputSchema": { "type": "object", "properties": {
                "folder": { "type": "string", "description": "Folder id. Omit for the whole collection." },
                "environment": env, "confirmProduction": confirm
            } }
        },
        {
            "name": "create_request",
            "description": "Add a request to the collection (a new file the person reviews in git). Use {{variables}} for hosts and secrets.",
            "inputSchema": { "type": "object", "properties": {
                "folder": { "type": "string", "description": "Folder id; omit for the top level." },
                "name": { "type": "string" },
                "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] },
                "url": { "type": "string" },
                "headers": headers,
                "body": { "type": "string", "description": "Raw body, e.g. JSON." },
                "testScript": { "type": "string", "description": "Tests in Postman style: pm.test(...), pm.expect(...)." }
            }, "required": ["name", "method", "url"] }
        },
        {
            "name": "update_request",
            "description": "Change fields of an existing request; fields left out stay as they are. headers replaces the whole list.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string" }, "name": { "type": "string" },
                "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] },
                "url": { "type": "string" }, "headers": headers, "body": { "type": "string" },
                "preScript": { "type": "string" }, "testScript": { "type": "string" }
            }, "required": ["id"] }
        },
        {
            "name": "list_environments",
            "description": "List the environments and their variables. Secret values are hidden.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "import_collection",
            "description": "Import a curl command, a Postman/Insomnia/Bruno/Hoppscotch collection, an OpenAPI document, a HAR or a .http file (as text) into the collection.",
            "inputSchema": { "type": "object", "properties": {
                "text": { "type": "string" },
                "folder": { "type": "string", "description": "Folder id to import into; omit for the top level." }
            }, "required": ["text"] }
        },
        {
            "name": "export_collection",
            "description": "Export the collection or a folder as Postman v2.1, OpenAPI 3 or a .http file. Returns the text.",
            "inputSchema": { "type": "object", "properties": {
                "format": { "type": "string", "enum": ["postman", "openapi", "http"] },
                "folder": { "type": "string" }
            }, "required": ["format"] }
        },
        {
            "name": "list_flows",
            "description": "List the flows: features told as a sequence of existing requests (and other flows), with their steps.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "run_flow",
            "description": "Run a flow step by step, passing the values it saves; stops at the first failing step.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "Flow id (see list_flows)." },
                "environment": env, "confirmProduction": confirm
            }, "required": ["id"] }
        },
        {
            "name": "save_flow",
            "description": "Create or replace a flow. Steps reference requests by id (never copy them) or other flows. save keeps response values (JSONPath) as {{variables}} for later steps; expect checks the status.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "Existing flow id to replace; omit to create one from the name." },
                "name": { "type": "string" },
                "description": { "type": "string" },
                "steps": { "type": "array", "items": { "type": "object", "properties": {
                    "request": { "type": "string", "description": "Request id" },
                    "flow": { "type": "string", "description": "Or a flow id, to reuse it" },
                    "name": { "type": "string" },
                    "save": { "type": "object", "additionalProperties": { "type": "string" }, "description": "variable: JSONPath, e.g. { \"token\": \"$.data.token\" }" },
                    "expect": { "type": "object", "properties": { "status": { "type": "integer" } } }
                } } }
            }, "required": ["name", "steps"] }
        },
        {
            "name": "collection_diff",
            "description": "What changed in the collection between two git revisions (or a revision and the files on disk), request by request, as Markdown.",
            "inputSchema": { "type": "object", "properties": {
                "from": { "type": "string", "description": "Commit, branch or tag. Default HEAD." },
                "to": { "type": "string", "description": "Another revision. Omit for the files on disk." }
            } }
        }
    ])
}

fn arg<'v>(args: &'v Value, name: &str) -> Option<&'v str> {
    args.get(name)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn headers_arg(args: &Value) -> Option<Vec<Header>> {
    args.get("headers").and_then(|h| h.as_array()).map(|list| {
        list.iter()
            .filter_map(|h| {
                let name = h.get("name")?.as_str()?.trim();
                (!name.is_empty()).then(|| {
                    Header::new(name, h.get("value").and_then(|v| v.as_str()).unwrap_or(""))
                })
            })
            .collect()
    })
}

/// Environment names that look like production.
fn looks_like_production(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            matches!(
                word,
                "prod" | "prd" | "production" | "produccion" | "live" | "pro"
            )
        })
}

fn cut(text: &str) -> String {
    if text.len() <= MAX_BODY {
        return text.to_string();
    }
    let mut end = MAX_BODY;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} bytes in all, cut)", &text[..end], text.len())
}

impl<'a> Server<'a> {
    pub fn new(root: PathBuf, store: &'a dyn SecretStore) -> Result<Server<'a>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Server {
            root,
            store,
            runtime,
        })
    }

    /// Reads JSON-RPC messages, one per line, and writes the answers.
    pub fn serve(&self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(message) => self.handle(&message),
                Err(e) => Some(json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                })),
            };
            if let Some(reply) = reply {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// One JSON-RPC message; `None` for notifications.
    pub fn handle(&self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned()?;
        let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "pulpo", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match self.call(name, &args) {
                    Ok(text) => text_result(text, false),
                    Err(e) => text_result(e, true),
                })
            }
            other => {
                Err(json!({ "code": -32601, "message": format!("method not found: {other}") }))
            }
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        })
    }

    fn call(&self, name: &str, args: &Value) -> Result<String, String> {
        if !self.root.join(collection::WORKSPACE_FILE).is_file() {
            return Err(format!(
                "{} is not a Pulpo workspace (no {}); start `pulpo mcp -w <folder>` or run `pulpo init` there",
                self.root.display(),
                collection::WORKSPACE_FILE
            ));
        }
        match name {
            "list_requests" => self.list_requests(arg(args, "folder"), arg(args, "query")),
            "get_request" => self.get_request(arg(args, "id").ok_or("id is required")?),
            "run_request" => {
                let id = arg(args, "id").ok_or("id is required")?;
                let env = self.environment(args)?;
                let entry = collection::load_request(&self.root, id).map_err(|e| e.to_string())?;
                let report = self.runtime.block_on(run::run_entry_with(
                    &self.root,
                    &entry,
                    env.as_ref().map(|(id, _)| id.as_str()),
                    self.store,
                    &RunOptions::default(),
                ));
                Ok(self.mask(&env, &describe_run(&report)))
            }
            "run_folder" => self.run_folder(arg(args, "folder").unwrap_or(""), args),
            "create_request" => self.create_request(args),
            "update_request" => self.update_request(args),
            "list_environments" => self.list_environments(),
            "import_collection" => {
                let text = arg(args, "text").ok_or("text is required")?;
                let plan = import::parse(text).map_err(|e| e.to_string())?;
                let summary = import::apply(
                    &self.root,
                    arg(args, "folder").unwrap_or(""),
                    &plan,
                    self.store,
                )
                .map_err(|e| e.to_string())?;
                let mut out = format!(
                    "Imported {}: {} requests, {} folders into `{}`.",
                    plan.format.label(),
                    summary.requests,
                    summary.folders,
                    if summary.folder_id.is_empty() {
                        "the top level"
                    } else {
                        &summary.folder_id
                    }
                );
                for w in summary.warnings.iter().take(20) {
                    out.push_str(&format!("\n- warning: {w}"));
                }
                Ok(out)
            }
            "export_collection" => {
                let format = arg(args, "format")
                    .and_then(ExportFormat::parse)
                    .ok_or("format must be postman, openapi or http")?;
                let exported =
                    export::export(&self.root, arg(args, "folder").unwrap_or(""), format)
                        .map_err(|e| e.to_string())?;
                Ok(cut(&exported.text))
            }
            "list_flows" => {
                let flows = crate::flow::list_flows(&self.root).map_err(|e| e.to_string())?;
                if flows.is_empty() {
                    return Ok("No flows yet. Create one with save_flow.".into());
                }
                let mut out = String::new();
                for f in flows {
                    out.push_str(&format!("## {} (id: {})\n", f.name, f.id));
                    if !f.description.is_empty() {
                        out.push_str(&format!("{}\n", f.description));
                    }
                    for node in crate::flow::plan(&self.root, &f.id).map_err(|e| e.to_string())? {
                        out.push_str(&format!(
                            "{}{} {} {}{}\n",
                            "  ".repeat(node.depth),
                            node.path,
                            if node.kind == "flow" {
                                "flow".to_string()
                            } else {
                                node.method.clone()
                            },
                            node.name,
                            node.problem.map(|p| format!(" ✗ {p}")).unwrap_or_default()
                        ));
                    }
                }
                Ok(out)
            }
            "run_flow" => {
                let id = arg(args, "id").ok_or("id is required")?;
                let env = self.environment(args)?;
                let report = self.runtime.block_on(crate::flow::run_flow(
                    &self.root,
                    id,
                    env.as_ref().map(|(id, _)| id.as_str()),
                    self.store,
                    &mut |_| {},
                ))?;
                let mut out = format!(
                    "Flow {}: {}\n",
                    report.flow,
                    if report.passed { "passed" } else { "failed" }
                );
                for step in &report.steps {
                    match &step.report {
                        None => out.push_str(&format!("- {} skipped\n", step.path)),
                        Some(r) => {
                            out.push_str(&format!(
                                "\n### {} {}\n",
                                if r.passed() { "✓" } else { "✗" },
                                r.name
                            ));
                            out.push_str(&describe_run(r));
                            for (var, value) in &step.saved {
                                out.push_str(&format!("saved {var} = {value}\n"));
                            }
                        }
                    }
                }
                Ok(self.mask(&env, &out))
            }
            "save_flow" => {
                let steps: Vec<crate::flow::Step> =
                    serde_json::from_value(args.get("steps").cloned().unwrap_or(json!([])))
                        .map_err(|e| format!("steps: {e}"))?;
                let flow = crate::flow::Flow {
                    id: arg(args, "id").unwrap_or("").to_string(),
                    version: 1,
                    name: arg(args, "name").ok_or("name is required")?.to_string(),
                    description: arg(args, "description").unwrap_or("").to_string(),
                    steps,
                };
                let saved = crate::flow::save_flow(&self.root, &flow).map_err(|e| e.to_string())?;
                let problems: Vec<String> = crate::flow::plan(&self.root, &saved.id)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter_map(|n| n.problem.map(|p| format!("step {}: {p}", n.path)))
                    .collect();
                let mut out = format!(
                    "Saved flow {} (id: {}, file flows/{}.flow.yaml).",
                    saved.name, saved.id, saved.id
                );
                for p in problems {
                    out.push_str(&format!("\n✗ {p}"));
                }
                Ok(out)
            }
            "collection_diff" => {
                let diff = diff::diff_refs(
                    &self.root,
                    arg(args, "from").unwrap_or("HEAD"),
                    arg(args, "to"),
                )?;
                Ok(diff::to_markdown(&diff))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    /// The environment of a call (id and name) after the production check.
    fn environment(&self, args: &Value) -> Result<Option<(String, String)>, String> {
        let Some(name) = arg(args, "environment") else {
            return Ok(None);
        };
        let envs =
            environment::list_environments(&self.root, self.store).map_err(|e| e.to_string())?;
        let env = envs
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name) || e.id == name)
            .ok_or_else(|| {
                format!(
                    "no environment {name}; there are: {}",
                    envs.iter()
                        .map(|e| e.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let confirmed = args
            .get("confirmProduction")
            .and_then(|c| c.as_bool())
            .unwrap_or(false);
        if looks_like_production(&env.name) && !confirmed {
            return Err(format!(
                "{} looks like a production environment. Ask the person first; call again with confirmProduction: true only if they say yes.",
                env.name
            ));
        }
        Ok(Some((env.id.clone(), env.name.clone())))
    }

    /// Hides the secret values of the environment in text going to the AI.
    fn mask(&self, env: &Option<(String, String)>, text: &str) -> String {
        let Some((id, _)) = env else {
            return text.to_string();
        };
        let Ok(loaded) = environment::load_environment(&self.root, id, self.store, true) else {
            return text.to_string();
        };
        let mut secrets: Vec<String> = loaded
            .variables
            .iter()
            .filter(|v| v.secret)
            .filter_map(|v| {
                std::env::var(environment::secret_override_name(&v.name))
                    .ok()
                    .or(v.has_value.then(|| v.value.clone()))
            })
            .filter(|v| v.len() >= 3)
            .collect();
        // Longest first, so a secret inside another is not half hidden.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut out = text.to_string();
        for secret in secrets {
            out = out.replace(&secret, "••••");
            let encoded = crate::import::base64(secret.as_bytes());
            if encoded.len() >= 4 {
                out = out.replace(&encoded, "••••");
            }
        }
        out
    }

    fn list_requests(&self, folder: Option<&str>, query: Option<&str>) -> Result<String, String> {
        let workspace = collection::load_workspace(&self.root).map_err(|e| e.to_string())?;
        let words: Vec<String> = query
            .unwrap_or("")
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut out = format!("Collection \"{}\"\n", workspace.name);
        let mut count = 0;
        fn walk(
            nodes: &[TreeNode],
            path: &str,
            words: &[String],
            out: &mut String,
            count: &mut usize,
        ) {
            for node in nodes {
                match node {
                    TreeNode::Folder { name, children, .. } => {
                        let inner = if path.is_empty() {
                            name.clone()
                        } else {
                            format!("{path} / {name}")
                        };
                        walk(children, &inner, words, out, count);
                    }
                    TreeNode::Request(entry) => {
                        let haystack =
                            format!("{path} {} {}", entry.name, entry.request.url).to_lowercase();
                        if words.iter().all(|w| haystack.contains(w)) {
                            *count += 1;
                            out.push_str(&format!(
                                "- {} {} — {}{} — id: {}\n",
                                entry.request.method,
                                entry.request.url,
                                if path.is_empty() {
                                    String::new()
                                } else {
                                    format!("{path} / ")
                                },
                                entry.name,
                                entry.id
                            ));
                        }
                    }
                }
            }
        }
        let nodes = match folder {
            None => workspace.tree.clone(),
            Some(folder) => {
                let found = run::requests_under(&workspace.tree, folder);
                if found.is_empty() {
                    return Err(format!("no requests in folder {folder}"));
                }
                found.into_iter().map(TreeNode::Request).collect()
            }
        };
        walk(&nodes, "", &words, &mut out, &mut count);
        if count == 0 {
            out.push_str("(no requests match)\n");
        }
        Ok(out)
    }

    fn get_request(&self, id: &str) -> Result<String, String> {
        let entry = collection::load_request(&self.root, id).map_err(|e| e.to_string())?;
        let (auth, from) =
            collection::effective_auth(&self.root, &entry).map_err(|e| e.to_string())?;
        let mut value = serde_json::to_value(&entry).map_err(|e| e.to_string())?;
        value["effectiveAuth"] = json!({
            "type": auth.label(),
            "from": if from.is_empty() { "nowhere (no auth)".to_string() } else { from },
        });
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
    }

    fn run_folder(&self, folder: &str, args: &Value) -> Result<String, String> {
        let env = self.environment(args)?;
        let workspace = collection::load_workspace(&self.root).map_err(|e| e.to_string())?;
        let requests = run::requests_under(&workspace.tree, folder);
        if requests.is_empty() {
            return Err(format!("nothing to run at '{folder}'"));
        }
        let mut session = run::Session::open(
            &self.root,
            env.as_ref().map(|(id, _)| id.as_str()),
            self.store,
            true,
        )?;
        let mut reports = Vec::new();
        for entry in &requests {
            let mut report = self.runtime.block_on(run::run_in(
                &self.root,
                entry,
                &mut session,
                &RunOptions::default(),
            ));
            report
                .logs
                .extend(session.save_tokens(&self.root, self.store));
            reports.push(report);
        }
        let passed = reports.iter().filter(|r| r.passed()).count();
        let mut out = format!(
            "{} requests: {passed} passed, {} failed{}\n",
            reports.len(),
            reports.len() - passed,
            env.as_ref()
                .map(|(_, n)| format!(" (environment {n})"))
                .unwrap_or_default()
        );
        for r in &reports {
            let status = match (&r.response, &r.error) {
                (Some(resp), _) => format!("{} {}", resp.status, resp.status_text),
                (None, Some(e)) => format!("error: {e}"),
                _ => String::new(),
            };
            out.push_str(&format!(
                "{} {} — {} — id: {}\n",
                if r.passed() { "✓" } else { "✗" },
                r.name,
                status,
                r.id
            ));
            for t in r.tests.iter().filter(|t| !t.passed) {
                out.push_str(&format!(
                    "    ✗ {}: {}\n",
                    t.name,
                    t.error.as_deref().unwrap_or("failed")
                ));
            }
            for e in &r.script_errors {
                out.push_str(&format!("    script error: {e}\n"));
            }
        }
        Ok(self.mask(&env, &out))
    }

    fn create_request(&self, args: &Value) -> Result<String, String> {
        let name = arg(args, "name").ok_or("name is required")?;
        let method = HttpMethod::from_str(arg(args, "method").unwrap_or("GET"))
            .map_err(|e| e.to_string())?;
        let url = arg(args, "url").ok_or("url is required")?;
        let created =
            collection::create_request(&self.root, arg(args, "folder").unwrap_or(""), name)
                .map_err(|e| e.to_string())?;
        let mut request = HttpRequest::new(method, url);
        request.headers = headers_arg(args).unwrap_or_default();
        request.body = arg(args, "body").map(str::to_string);
        let entry = RequestEntry {
            request,
            scripts: Scripts {
                pre: String::new(),
                test: arg(args, "testScript").unwrap_or("").to_string(),
            },
            ..created
        };
        collection::save_request(&self.root, &entry).map_err(|e| e.to_string())?;
        Ok(format!("Created {} (id: {}).", entry.name, entry.id))
    }

    fn update_request(&self, args: &Value) -> Result<String, String> {
        let id = arg(args, "id").ok_or("id is required")?;
        let mut entry = collection::load_request(&self.root, id).map_err(|e| e.to_string())?;
        let mut changed = Vec::new();
        if let Some(method) = arg(args, "method") {
            entry.request.method = HttpMethod::from_str(method).map_err(|e| e.to_string())?;
            changed.push("method");
        }
        if let Some(url) = arg(args, "url") {
            entry.request.url = url.to_string();
            changed.push("URL");
        }
        if let Some(headers) = headers_arg(args) {
            entry.request.headers = headers;
            changed.push("headers");
        }
        if let Some(body) = args.get("body").and_then(|b| b.as_str()) {
            entry.request.body = (!body.is_empty()).then(|| body.to_string());
            changed.push("body");
        }
        if let Some(pre) = args.get("preScript").and_then(|s| s.as_str()) {
            entry.scripts.pre = pre.to_string();
            changed.push("pre-request script");
        }
        if let Some(test) = args.get("testScript").and_then(|s| s.as_str()) {
            entry.scripts.test = test.to_string();
            changed.push("test script");
        }
        if let Some(name) = arg(args, "name") {
            entry.name = name.to_string();
            changed.push("name");
        }
        if changed.is_empty() {
            return Err("nothing to change".into());
        }
        collection::save_request(&self.root, &entry).map_err(|e| e.to_string())?;
        Ok(format!("Updated {} ({}).", entry.name, changed.join(", ")))
    }

    fn list_environments(&self) -> Result<String, String> {
        let envs =
            environment::list_environments(&self.root, self.store).map_err(|e| e.to_string())?;
        if envs.is_empty() {
            return Ok(
                "No environments. Requests use {{variables}} that an environment would fill."
                    .into(),
            );
        }
        let mut out = String::new();
        for env in envs {
            out.push_str(&format!(
                "## {}{}\n",
                env.name,
                if looks_like_production(&env.name) {
                    " (production: needs confirmProduction)"
                } else {
                    ""
                }
            ));
            for v in env.variables {
                let value = if v.secret {
                    if v.has_value {
                        "•••• (secret)".to_string()
                    } else {
                        "(secret, not set)".to_string()
                    }
                } else {
                    v.value
                };
                out.push_str(&format!("- {}: {value}\n", v.name));
            }
        }
        Ok(out)
    }
}

/// A run, for the AI: status, headers, body and tests.
fn describe_run(report: &RunReport) -> String {
    let mut out = format!("{} — id: {}\n", report.name, report.id);
    if let Some(sent) = report.response.as_ref().and_then(|r| r.sent.as_ref()) {
        out.push_str(&format!("\nSent: {} {}\n", sent.method, sent.url));
        for h in &sent.headers {
            out.push_str(&format!("{}: {}\n", h.name, h.value));
        }
        if let Some(body) = &sent.body {
            out.push_str(&format!("\n{}\n", cut(body)));
        }
    }
    match (&report.response, &report.error) {
        (Some(r), _) => {
            out.push_str(&format!(
                "\nResponse: {} {} · {} ms · {} bytes\n",
                r.status, r.status_text, r.duration_ms, r.size_bytes
            ));
            for h in &r.headers {
                out.push_str(&format!("{}: {}\n", h.name, h.value));
            }
            if r.body_is_text {
                out.push_str(&format!("\n{}\n", cut(&r.body)));
            } else {
                out.push_str("\n(binary body)\n");
            }
        }
        (None, Some(e)) => out.push_str(&format!("\nError: {e}\n")),
        _ => {}
    }
    if !report.tests.is_empty() {
        out.push_str("\nTests:\n");
        for t in &report.tests {
            out.push_str(&format!(
                "{} {}{}\n",
                if t.passed { "✓" } else { "✗" },
                t.name,
                t.error
                    .as_ref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            ));
        }
    }
    for e in &report.script_errors {
        out.push_str(&format!("Script error: {e}\n"));
    }
    for l in &report.logs {
        out.push_str(&format!("log: {l}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::init_workspace;
    use crate::environment::{Environment, MemoryStore, Variable};
    use std::io::Cursor;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn call(server: &Server, name: &str, args: Value) -> (String, bool) {
        let reply = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                             "params": { "name": name, "arguments": args } }))
            .unwrap();
        let result = &reply["result"];
        (
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            result["isError"].as_bool().unwrap_or(false),
        )
    }

    /// Echoes the Authorization header back.
    fn echo_server() -> String {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = std_listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                std_listener.set_nonblocking(true).unwrap();
                let listener = TcpListener::from_std(std_listener).unwrap();
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut buffer = vec![0u8; 8192];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let auth = text
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().starts_with("authorization:").then(|| l[14..].trim().to_string()))
                        .unwrap_or_default();
                    let body = format!("{{\"youSent\":\"{auth}\"}}");
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
            });
        });
        format!("http://{address}")
    }

    #[test]
    fn speaks_mcp_and_works_on_the_collection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Shop").unwrap();
        let store = MemoryStore::default();
        let base = echo_server();
        for (name, secret) in [("Local", "s3cr3t-token"), ("Prod", "prod-token")] {
            let created = environment::create_environment(root, name, &store).unwrap();
            environment::save_environment(
                root,
                &Environment {
                    id: created.id,
                    name: name.into(),
                    variables: vec![
                        Variable {
                            name: "base".into(),
                            value: base.clone(),
                            secret: false,
                            has_value: false,
                        },
                        Variable {
                            name: "token".into(),
                            value: secret.into(),
                            secret: true,
                            has_value: true,
                        },
                    ],
                },
                &store,
            )
            .unwrap();
        }
        let server = Server::new(root.to_path_buf(), &store).unwrap();

        // The protocol: initialize, a notification (no answer) and the tool list.
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let mut out = Vec::new();
        server.serve(Cursor::new(input), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["result"]["serverInfo"]["name"], "pulpo");
        assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 13);

        // Create, list, read and run a request.
        let (text, error) = call(
            &server,
            "create_request",
            json!({
                "name": "Who am I", "method": "GET", "url": "{{base}}/me",
                "headers": [{ "name": "Authorization", "value": "Bearer {{token}}" }],
                "testScript": "pm.test('ok', () => pm.response.to.have.status(200));"
            }),
        );
        assert!(!error, "{text}");
        let id = text
            .split("id: ")
            .nth(1)
            .unwrap()
            .trim_end_matches(").")
            .to_string();
        let (list, _) = call(&server, "list_requests", json!({ "query": "who" }));
        assert!(list.contains("GET {{base}}/me — Who am I"), "{list}");
        let (read, _) = call(&server, "get_request", json!({ "id": id }));
        assert!(read.contains("Bearer {{token}}"), "{read}");

        let (run, error) = call(
            &server,
            "run_request",
            json!({ "id": id, "environment": "local" }),
        );
        assert!(!error, "{run}");
        assert!(run.contains("Response: 200 OK"), "{run}");
        assert!(run.contains("✓ ok"), "{run}");
        assert!(!run.contains("s3cr3t-token"), "the secret leaked: {run}");
        assert!(run.contains("Bearer ••••"), "{run}");

        // Production needs the person's yes.
        let (refused, error) = call(
            &server,
            "run_request",
            json!({ "id": id, "environment": "Prod" }),
        );
        assert!(error && refused.contains("production"), "{refused}");
        let (allowed, error) = call(
            &server,
            "run_request",
            json!({ "id": id, "environment": "Prod", "confirmProduction": true }),
        );
        assert!(!error && !allowed.contains("prod-token"), "{allowed}");

        let (envs, _) = call(&server, "list_environments", json!({}));
        assert!(
            envs.contains("- token: •••• (secret)") && !envs.contains("s3cr3t"),
            "{envs}"
        );
        assert!(envs.contains("## Prod (production"), "{envs}");

        let (updated, error) = call(
            &server,
            "update_request",
            json!({ "id": id, "url": "{{base}}/me?x=1" }),
        );
        assert!(!error && updated.contains("URL"), "{updated}");
        let (suite, _) = call(&server, "run_folder", json!({ "environment": "Local" }));
        assert!(suite.starts_with("1 requests: 1 passed"), "{suite}");

        let (imported, error) = call(
            &server,
            "import_collection",
            json!({ "text": "curl https://example.com/health" }),
        );
        assert!(!error && imported.contains("1 requests"), "{imported}");
        let (exported, _) = call(&server, "export_collection", json!({ "format": "http" }));
        assert!(exported.contains("### Who am I"), "{exported}");
        // A flow made by the AI, run with the secrets still hidden.
        let (saved, error) = call(
            &server,
            "save_flow",
            json!({
                "name": "Check me", "steps": [{ "request": id, "expect": { "status": 200 } }]
            }),
        );
        assert!(!error && saved.contains("id: check-me"), "{saved}");
        let (flows, _) = call(&server, "list_flows", json!({}));
        assert!(flows.contains("## Check me (id: check-me)"), "{flows}");
        let (ran, error) = call(
            &server,
            "run_flow",
            json!({ "id": "check-me", "environment": "Local" }),
        );
        assert!(!error && ran.starts_with("Flow Check me: passed"), "{ran}");
        assert!(!ran.contains("s3cr3t-token"), "{ran}");
        let (unknown, error) = call(&server, "nope", json!({}));
        assert!(error && unknown.contains("unknown tool"));
    }

    #[test]
    fn recognises_production_names_and_finds_the_workspace() {
        for name in ["Prod", "MEDY Prd", "production-eu", "Live"] {
            assert!(looks_like_production(name), "{name}");
        }
        for name in ["Local", "Dev", "Staging", "product-catalog"] {
            assert!(!looks_like_production(name), "{name}");
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("api")).unwrap();
        init_workspace(&dir.path().join("api"), "Shop").unwrap();
        assert_eq!(find_workspace(dir.path()), Some(dir.path().join("api")));
    }
}
