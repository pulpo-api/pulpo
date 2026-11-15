//! Flows: a feature told as a sequence of existing requests and other flows.
//!
//! A flow lives in `flows/<id>.flow.yaml`. Its steps *reference* requests of the
//! collection by id (they are never copied), so a change to a request reaches every flow
//! that uses it. `save` keeps values of a response (JSONPath) as variables of the flow
//! only, and `expect` checks the status. Anything more complex belongs in the request's
//! own scripts.

use crate::collection::{self, quote, CollectionError, RequestEntry};
use crate::environment::SecretStore;
use crate::run::{self, RunOptions, RunReport, Session};
use crate::script::TestResult;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

pub const FLOWS_DIR: &str = "flows";
pub const FLOW_SUFFIX: &str = ".flow.yaml";
/// Subflows deeper than this are a mistake, not a design.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Expect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

/// One step: a request of the collection or another flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Step {
    Request {
        /// Request id, e.g. `01-api/03-login/01-login.pulpo.yaml`.
        request: String,
        /// A label for this step, when the request's own name is not enough.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Variable → JSONPath into the response body.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        save: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<Expect>,
    },
    Flow {
        /// Flow id (the file name without `.flow.yaml`).
        flow: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flow {
    /// File name without `.flow.yaml`; set when loaded. The app needs it, so it is
    /// serialized; the file itself is written by `to_yaml`, which leaves it out.
    #[serde(default)]
    pub id: String,
    #[serde(default = "one")]
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

fn one() -> u32 {
    1
}

fn flow_path(root: &Path, id: &str) -> Result<std::path::PathBuf, CollectionError> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(CollectionError::InvalidPath(format!("flow id {id}")));
    }
    Ok(root.join(FLOWS_DIR).join(format!("{id}{FLOW_SUFFIX}")))
}

/// Every flow of the workspace, by name.
pub fn list_flows(root: &Path) -> Result<Vec<Flow>, CollectionError> {
    let dir = root.join(FLOWS_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut flows = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| collection::io_error(&dir, e))? {
        let Ok(entry) = entry else { continue };
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = file.strip_suffix(FLOW_SUFFIX) {
            flows.push(load_flow(root, id)?);
        }
    }
    flows.sort_by_key(|f| f.name.to_lowercase());
    Ok(flows)
}

pub fn load_flow(root: &Path, id: &str) -> Result<Flow, CollectionError> {
    let path = flow_path(root, id)?;
    if !path.is_file() {
        return Err(CollectionError::InvalidPath(format!("no flow {id}")));
    }
    let mut flow: Flow = collection::parse_yaml(&path)?;
    flow.id = id.to_string();
    Ok(flow)
}

/// The flow as YAML, written by hand so diffs stay small and stable.
pub fn to_yaml(flow: &Flow) -> String {
    let mut out = format!("version: 1\nname: {}\n", quote(&flow.name));
    if !flow.description.is_empty() {
        out.push_str(&format!("description: {}\n", quote(&flow.description)));
    }
    if flow.steps.is_empty() {
        out.push_str("steps: []\n");
        return out;
    }
    out.push_str("steps:\n");
    for step in &flow.steps {
        match step {
            Step::Request {
                request,
                name,
                save,
                expect,
            } => {
                out.push_str(&format!("  - request: {}\n", quote(request)));
                if let Some(name) = name {
                    out.push_str(&format!("    name: {}\n", quote(name)));
                }
                if !save.is_empty() {
                    out.push_str("    save:\n");
                    for (var, path) in save {
                        out.push_str(&format!("      {var}: {}\n", quote(path)));
                    }
                }
                if let Some(status) = expect.as_ref().and_then(|e| e.status) {
                    out.push_str(&format!("    expect:\n      status: {status}\n"));
                }
            }
            Step::Flow { flow } => out.push_str(&format!("  - flow: {}\n", quote(flow))),
        }
    }
    out
}

/// Saves a flow; a new one (empty id) gets an id from its name.
pub fn save_flow(root: &Path, flow: &Flow) -> Result<Flow, CollectionError> {
    let mut flow = flow.clone();
    if flow.name.trim().is_empty() {
        return Err(CollectionError::InvalidFile("a flow needs a name".into()));
    }
    if flow.id.is_empty() {
        let base = collection::slugify(&flow.name);
        let base = if base.is_empty() {
            "flow".to_string()
        } else {
            base
        };
        let mut id = base.clone();
        let mut n = 2;
        while flow_path(root, &id)?.exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        flow.id = id;
    }
    let path = flow_path(root, &flow.id)?;
    fs::create_dir_all(root.join(FLOWS_DIR)).map_err(|e| collection::io_error(&path, e))?;
    fs::write(&path, to_yaml(&flow)).map_err(|e| collection::io_error(&path, e))?;
    Ok(flow)
}

pub fn delete_flow(root: &Path, id: &str) -> Result<(), CollectionError> {
    let path = flow_path(root, id)?;
    fs::remove_file(&path).map_err(|e| collection::io_error(&path, e))
}

// --------------------------------------------------------------------- the plan

/// A step of the flow once subflows are expanded: what the tree view draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    /// `"2"`, `"1.3"`: position in the flow and its subflows.
    pub path: String,
    pub depth: usize,
    /// `request`, `flow` (a subflow header) or `missing`.
    pub kind: String,
    /// Request or flow id.
    pub target: String,
    pub name: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub save: Vec<String>,
    #[serde(default)]
    pub expect: Option<u16>,
    /// Why the step cannot run (missing request, a cycle…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// The expanded steps of a flow, in order.
pub fn plan(root: &Path, id: &str) -> Result<Vec<PlanNode>, CollectionError> {
    let flow = load_flow(root, id)?;
    let mut out = Vec::new();
    expand(root, &flow, "", 0, &mut vec![flow.id.clone()], &mut out);
    Ok(out)
}

fn expand(
    root: &Path,
    flow: &Flow,
    prefix: &str,
    depth: usize,
    stack: &mut Vec<String>,
    out: &mut Vec<PlanNode>,
) {
    for (i, step) in flow.steps.iter().enumerate() {
        let path = if prefix.is_empty() {
            format!("{}", i + 1)
        } else {
            format!("{prefix}.{}", i + 1)
        };
        match step {
            Step::Request {
                request,
                name,
                save,
                expect,
            } => {
                let found = collection::load_request(root, request).ok();
                out.push(PlanNode {
                    path,
                    depth,
                    kind: if found.is_some() {
                        "request"
                    } else {
                        "missing"
                    }
                    .into(),
                    target: request.clone(),
                    name: name
                        .clone()
                        .or_else(|| found.as_ref().map(|e| e.name.clone()))
                        .unwrap_or_else(|| request.clone()),
                    method: found
                        .as_ref()
                        .map(|e| e.request.method.to_string())
                        .unwrap_or_default(),
                    url: found
                        .as_ref()
                        .map(|e| e.request.url.clone())
                        .unwrap_or_default(),
                    save: save.keys().cloned().collect(),
                    expect: expect.as_ref().and_then(|e| e.status),
                    problem: found
                        .is_none()
                        .then(|| format!("no request {request} in the collection")),
                });
            }
            Step::Flow { flow: sub } => {
                let problem = if stack.contains(sub) {
                    Some(format!(
                        "{sub} includes itself ({} → {sub})",
                        stack.join(" → ")
                    ))
                } else if depth + 1 > MAX_DEPTH {
                    Some("subflows nest too deep".to_string())
                } else {
                    None
                };
                let loaded = if problem.is_none() {
                    load_flow(root, sub).ok()
                } else {
                    None
                };
                out.push(PlanNode {
                    path: path.clone(),
                    depth,
                    kind: if loaded.is_some() { "flow" } else { "missing" }.into(),
                    target: sub.clone(),
                    name: loaded
                        .as_ref()
                        .map(|f| f.name.clone())
                        .unwrap_or_else(|| sub.clone()),
                    method: String::new(),
                    url: String::new(),
                    save: Vec::new(),
                    expect: None,
                    problem: problem.or_else(|| loaded.is_none().then(|| format!("no flow {sub}"))),
                });
                if let Some(subflow) = loaded {
                    stack.push(sub.clone());
                    expand(root, &subflow, &path, depth + 1, stack, out);
                    stack.pop();
                }
            }
        }
    }
}

/// Request id → names of the flows that use it (directly or through a subflow).
pub fn usage(root: &Path) -> Result<HashMap<String, Vec<String>>, CollectionError> {
    let mut used: HashMap<String, Vec<String>> = HashMap::new();
    for flow in list_flows(root)? {
        for node in plan(root, &flow.id)? {
            if node.kind == "request" {
                let names = used.entry(node.target).or_default();
                if !names.contains(&flow.name) {
                    names.push(flow.name.clone());
                }
            }
        }
    }
    Ok(used)
}

// --------------------------------------------------------------------- running

/// What happened at one step, sent while the flow runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepEvent {
    pub path: String,
    /// `running`, `passed`, `failed` or `skipped`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<RunReport>,
    /// Variables the step saved (name → value).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub saved: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowReport {
    pub flow: String,
    pub passed: bool,
    pub steps: Vec<StepEvent>,
}

impl FlowReport {
    /// The run reports of the steps that ran, for JUnit, HTML and Markdown reports.
    pub fn run_reports(&self) -> Vec<RunReport> {
        self.steps.iter().filter_map(|s| s.report.clone()).collect()
    }
}

fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Runs a flow. The environment's values and the request scripts work as usual; values
/// saved by the flow are variables of this run only. Stops at the first failing step.
pub async fn run_flow(
    root: &Path,
    id: &str,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    on_step: &mut (dyn FnMut(&StepEvent) + Send),
) -> Result<FlowReport, String> {
    let flow = load_flow(root, id).map_err(|e| e.to_string())?;
    let nodes = plan(root, id).map_err(|e| e.to_string())?;
    if let Some(bad) = nodes.iter().find(|n| n.problem.is_some()) {
        return Err(format!(
            "step {} ({}): {}",
            bad.path,
            bad.name,
            bad.problem.as_deref().unwrap_or_default()
        ));
    }
    // In memory: a flow does not leave its values or cookies in the workspace.
    let mut session = Session::open(root, environment_id, store, false)?.detached();
    let mut vars: crate::data::Row = HashMap::new();
    let mut report = FlowReport {
        flow: flow.name.clone(),
        passed: true,
        steps: Vec::new(),
    };
    let steps: Vec<(PlanNode, Option<Step>)> = {
        // The step definition of each request node, to read its save/expect.
        let mut defs = Vec::new();
        collect_steps(root, &flow, "", &mut defs);
        nodes
            .into_iter()
            .map(|n| {
                let def = defs
                    .iter()
                    .find(|(p, _)| *p == n.path)
                    .map(|(_, s)| s.clone());
                (n, def)
            })
            .collect()
    };
    let mut failed = false;
    for (node, def) in steps {
        if node.kind != "request" {
            continue;
        }
        if failed {
            let event = StepEvent {
                path: node.path.clone(),
                state: "skipped".into(),
                report: None,
                saved: BTreeMap::new(),
            };
            on_step(&event);
            report.steps.push(event);
            continue;
        }
        on_step(&StepEvent {
            path: node.path.clone(),
            state: "running".into(),
            report: None,
            saved: BTreeMap::new(),
        });
        let entry: RequestEntry =
            collection::load_request(root, &node.target).map_err(|e| e.to_string())?;
        let options = RunOptions {
            data: vars.clone(),
            ..Default::default()
        };
        let mut run_report = run::run_in(root, &entry, &mut session, &options).await;
        run_report.name = format!("{} · {}", node.path, entry.name);
        let mut saved = BTreeMap::new();
        if let Some(Step::Request { save, expect, .. }) = def {
            if let (Some(want), Some(response)) =
                (expect.as_ref().and_then(|e| e.status), &run_report.response)
            {
                run_report.tests.push(TestResult {
                    name: format!("status is {want}"),
                    passed: response.status == want,
                    error: (response.status != want)
                        .then(|| format!("expected {want}, got {}", response.status)),
                });
            }
            if let Some(response) = &run_report.response {
                for (var, path) in &save {
                    match crate::query::json_path(&response.body, path) {
                        Ok(values) if !values.is_empty() => {
                            let text = value_text(&values[0]);
                            vars.insert(var.clone(), text.clone());
                            saved.insert(var.clone(), text);
                        }
                        Ok(_) => run_report.tests.push(TestResult {
                            name: format!("save {var}"),
                            passed: false,
                            error: Some(format!("{path} found nothing in the response")),
                        }),
                        Err(e) => run_report.tests.push(TestResult {
                            name: format!("save {var}"),
                            passed: false,
                            error: Some(format!("{path}: {e}")),
                        }),
                    }
                }
            }
        }
        let passed = run_report.passed();
        failed = !passed;
        report.passed &= passed;
        let event = StepEvent {
            path: node.path.clone(),
            state: if passed { "passed" } else { "failed" }.into(),
            report: Some(run_report),
            saved,
        };
        on_step(&event);
        report.steps.push(event);
    }
    Ok(report)
}

fn collect_steps(root: &Path, flow: &Flow, prefix: &str, out: &mut Vec<(String, Step)>) {
    for (i, step) in flow.steps.iter().enumerate() {
        let path = if prefix.is_empty() {
            format!("{}", i + 1)
        } else {
            format!("{prefix}.{}", i + 1)
        };
        if let Step::Flow { flow: sub } = step {
            // `plan` already refused cycles before anything runs.
            if out.len() < 10_000 {
                if let Ok(subflow) = load_flow(root, sub) {
                    collect_steps(root, &subflow, &path, out);
                }
            }
        }
        out.push((path, step.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{init_workspace, save_request, Scripts};
    use crate::environment::MemoryStore;
    use crate::http::HttpRequest;
    use crate::HttpMethod;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// `/login` → a token; `/me` → 200 with the user if the token is right, else 401.
    async fn api() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let (status, body) = if text.starts_with("POST /login") {
                    (
                        "200 OK",
                        r#"{"data":{"token":"t-42","user":{"id":7}}}"#.to_string(),
                    )
                } else if text.starts_with("GET /users/7") && text.contains("Bearer t-42") {
                    ("200 OK", r#"{"name":"Ana"}"#.to_string())
                } else {
                    ("401 Unauthorized", "{}".to_string())
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://{address}")
    }

    fn request(root: &Path, id: &str, name: &str, method: HttpMethod, url: &str, auth: bool) {
        let mut request = HttpRequest::new(method, url);
        if auth {
            request.headers.push(crate::http::Header::new(
                "Authorization",
                "Bearer {{token}}",
            ));
        }
        save_request(
            root,
            &RequestEntry {
                id: id.into(),
                name: name.into(),
                protocol: crate::Protocol::Http,
                request,
                scripts: Scripts::default(),
            },
        )
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_steps_passing_saved_values_and_reusing_subflows() {
        let base = api().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Shop").unwrap();
        request(
            root,
            "01-login.pulpo.yaml",
            "Login",
            HttpMethod::Post,
            &format!("{base}/login"),
            false,
        );
        request(
            root,
            "02-me.pulpo.yaml",
            "Who am I",
            HttpMethod::Get,
            &format!("{base}/users/{{{{userId}}}}"),
            true,
        );
        let mut save = BTreeMap::new();
        save.insert("token".to_string(), "$.data.token".to_string());
        save.insert("userId".to_string(), "$.data.user.id".to_string());
        let login = save_flow(
            root,
            &Flow {
                id: String::new(),
                version: 1,
                name: "Log in".into(),
                description: String::new(),
                steps: vec![Step::Request {
                    request: "01-login.pulpo.yaml".into(),
                    name: None,
                    save,
                    expect: Some(Expect { status: Some(200) }),
                }],
            },
        )
        .unwrap();
        assert_eq!(login.id, "log-in");
        let profile = save_flow(
            root,
            &Flow {
                id: String::new(),
                version: 1,
                name: "See my profile".into(),
                description: "Login, then the profile".into(),
                steps: vec![
                    Step::Flow {
                        flow: "log-in".into(),
                    },
                    Step::Request {
                        request: "02-me.pulpo.yaml".into(),
                        name: Some("Profile".into()),
                        save: BTreeMap::new(),
                        expect: Some(Expect { status: Some(200) }),
                    },
                ],
            },
        )
        .unwrap();
        let text = fs::read_to_string(root.join("flows/see-my-profile.flow.yaml")).unwrap();
        assert!(text.contains("  - flow: \"log-in\"\n"), "{text}");
        assert_eq!(load_flow(root, &profile.id).unwrap().steps, profile.steps);
        // The flows folder is not part of the request tree.
        assert!(collection::load_workspace(root)
            .unwrap()
            .tree
            .iter()
            .all(|n| !matches!(n, collection::TreeNode::Folder { name, .. } if name == "flows")));

        // What the app receives carries the id; the file does not.
        let listed = serde_json::to_value(list_flows(root).unwrap()).unwrap();
        assert_eq!(listed[0]["id"], "log-in");
        assert!(!text.contains("id:"), "{text}");

        let nodes = plan(root, &profile.id).unwrap();
        let paths: Vec<(&str, &str)> = nodes
            .iter()
            .map(|n| (n.path.as_str(), n.kind.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![("1", "flow"), ("1.1", "request"), ("2", "request")]
        );
        assert_eq!(nodes[2].name, "Profile");

        let mut events = Vec::new();
        let report = run_flow(root, &profile.id, None, &MemoryStore::default(), &mut |e| {
            events.push(format!("{} {}", e.path, e.state))
        })
        .await
        .unwrap();
        assert!(report.passed, "{report:#?}");
        assert_eq!(
            events,
            vec!["1.1 running", "1.1 passed", "2 running", "2 passed"]
        );
        assert_eq!(report.steps[0].saved["userId"], "7");
        assert_eq!(report.run_reports().len(), 2);
        // Nothing was written to the workspace's local values.
        assert!(!root.join(".pulpo/local.json").exists());

        let used = usage(root).unwrap();
        assert_eq!(used["02-me.pulpo.yaml"], vec!["See my profile".to_string()]);
        assert_eq!(used["01-login.pulpo.yaml"].len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stops_at_a_failing_step_and_refuses_broken_flows() {
        let base = api().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Shop").unwrap();
        request(
            root,
            "02-me.pulpo.yaml",
            "Who am I",
            HttpMethod::Get,
            &format!("{base}/users/7"),
            true,
        );
        request(
            root,
            "03-again.pulpo.yaml",
            "Again",
            HttpMethod::Get,
            &format!("{base}/users/7"),
            false,
        );
        let flow = save_flow(
            root,
            &Flow {
                id: String::new(),
                version: 1,
                name: "No login".into(),
                description: String::new(),
                steps: vec![
                    Step::Request {
                        request: "03-again.pulpo.yaml".into(),
                        name: None,
                        save: BTreeMap::new(),
                        expect: Some(Expect { status: Some(200) }),
                    },
                    Step::Request {
                        request: "02-me.pulpo.yaml".into(),
                        name: None,
                        save: BTreeMap::new(),
                        expect: None,
                    },
                ],
            },
        )
        .unwrap();
        let report = run_flow(root, &flow.id, None, &MemoryStore::default(), &mut |_| {})
            .await
            .unwrap();
        assert!(!report.passed);
        assert_eq!(report.steps[0].state, "failed");
        assert_eq!(report.steps[1].state, "skipped");
        let failure = &report.steps[0].report.as_ref().unwrap().tests[0];
        assert_eq!(failure.error.as_deref(), Some("expected 200, got 401"));

        // A flow that includes itself, and one with a missing request.
        save_flow(
            root,
            &Flow {
                id: "loop".into(),
                version: 1,
                name: "Loop".into(),
                description: String::new(),
                steps: vec![Step::Flow {
                    flow: "loop".into(),
                }],
            },
        )
        .unwrap();
        let error = run_flow(root, "loop", None, &MemoryStore::default(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(error.contains("includes itself"), "{error}");
        save_flow(
            root,
            &Flow {
                id: "gone".into(),
                version: 1,
                name: "Gone".into(),
                description: String::new(),
                steps: vec![Step::Request {
                    request: "99-nope.pulpo.yaml".into(),
                    name: None,
                    save: BTreeMap::new(),
                    expect: None,
                }],
            },
        )
        .unwrap();
        assert_eq!(plan(root, "gone").unwrap()[0].kind, "missing");
        assert!(load_flow(root, "../etc").is_err());
    }
}
