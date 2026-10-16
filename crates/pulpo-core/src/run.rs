//! Running a request the way the app and `pulpo run` do: folder and request scripts,
//! variables (environment + local values), send, test scripts, save local values.

use crate::collection::{self, CollectionError, RequestEntry, TreeNode};
use crate::environment::{self, SecretStore};
use crate::http::{self, HttpResponse, SendError};
use crate::local;
use crate::script::{self, Phase, Script, ScriptContext, TestResult};
use crate::variables::{self, Variables};
use crate::Protocol;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Everything that happened while running one request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub id: String,
    pub name: String,
    pub response: Option<HttpResponse>,
    pub error: Option<SendError>,
    pub tests: Vec<TestResult>,
    pub logs: Vec<String>,
    pub script_errors: Vec<String>,
}

impl RunReport {
    /// A request passes when it got a response, no test failed and no script broke.
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.script_errors.is_empty() && self.tests.iter().all(|t| t.passed)
    }
}

fn scripts_for(
    root: &Path,
    entry: &RequestEntry,
) -> Result<(Vec<Script>, Vec<Script>), CollectionError> {
    let mut pre = Vec::new();
    let mut test = Vec::new();
    for folder in collection::ancestor_folders(&entry.id) {
        let scripts = collection::folder_scripts(root, &folder)?;
        let label = if folder.is_empty() {
            collection::FOLDER_SCRIPT_STEM.to_string()
        } else {
            format!("{folder}/{}", collection::FOLDER_SCRIPT_STEM)
        };
        pre.push(Script {
            label: format!("{label}.pre.js"),
            source: scripts.pre,
        });
        test.push(Script {
            label: format!("{label}.test.js"),
            source: scripts.test,
        });
    }
    let stem = entry.id.trim_end_matches(collection::REQUEST_SUFFIX);
    pre.push(Script {
        label: format!("{stem}.pre.js"),
        source: entry.scripts.pre.clone(),
    });
    test.push(Script {
        label: format!("{stem}.test.js"),
        source: entry.scripts.test.clone(),
    });
    Ok((pre, test))
}

fn merge(local: &mut HashMap<String, String>, changes: &HashMap<String, Option<String>>) {
    for (name, value) in changes {
        match value {
            Some(v) => {
                local.insert(name.clone(), v.clone());
            }
            None => {
                local.remove(name);
            }
        }
    }
}

/// Runs one request. `entry` may have unsaved edits; folder scripts are read from disk.
pub async fn run_entry(
    root: &Path,
    entry: &RequestEntry,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
) -> RunReport {
    let mut report = RunReport {
        id: entry.id.clone(),
        name: entry.name.clone(),
        ..Default::default()
    };
    if entry.protocol != Protocol::Http {
        report.error = Some(SendError::Other(format!(
            "{} requests arrive in phase 4",
            entry.protocol.as_str()
        )));
        return report;
    }

    let (env, environment_name) = match environment_id {
        Some(id) => match environment::variables_for(root, id, store).and_then(|vars| {
            environment::load_environment(root, id, store, false).map(|e| (vars, e.name))
        }) {
            Ok(found) => found,
            Err(e) => {
                report.error = Some(SendError::Other(e.to_string()));
                return report;
            }
        },
        None => (Variables::new(), String::new()),
    };
    let mut local_values = local::load_local(root, environment_id);
    let (pre, test) = match scripts_for(root, entry) {
        Ok(scripts) => scripts,
        Err(e) => {
            report.error = Some(SendError::Other(e.to_string()));
            return report;
        }
    };

    let context = ScriptContext {
        request_name: &entry.name,
        environment_name: &environment_name,
        env: &env,
        local: &local_values,
    };
    let (request, outcome) =
        script::run_phase(Phase::PreRequest, &pre, &context, &entry.request, None);
    let mut all_changes = outcome.changes.clone();
    merge(&mut local_values, &outcome.changes);
    report.logs.extend(outcome.logs);
    report.script_errors.extend(outcome.errors);
    report.tests.extend(outcome.tests);

    let mut vars = env.clone();
    vars.extend(local_values.clone());
    match variables::resolve_request(&request, &vars) {
        Err(missing) => report.error = Some(SendError::Unresolved(missing.join(", "))),
        Ok(resolved) => match http::send(&resolved).await {
            Err(e) => report.error = Some(e),
            Ok(response) => {
                let context = ScriptContext {
                    request_name: &entry.name,
                    environment_name: &environment_name,
                    env: &env,
                    local: &local_values,
                };
                let (_, outcome) =
                    script::run_phase(Phase::Test, &test, &context, &resolved, Some(&response));
                for (k, v) in &outcome.changes {
                    all_changes.insert(k.clone(), v.clone());
                }
                report.logs.extend(outcome.logs);
                report.script_errors.extend(outcome.errors);
                report.tests.extend(outcome.tests);
                report.response = Some(response);
            }
        },
    }

    if let Err(e) = local::apply_changes(root, environment_id, &all_changes) {
        report
            .script_errors
            .push(format!("could not save local values: {e}"));
    }
    report
}

/// Requests under `id` (a request, a folder or `""` for everything), in tree order.
pub fn requests_under(tree: &[TreeNode], id: &str) -> Vec<RequestEntry> {
    fn collect(nodes: &[TreeNode], out: &mut Vec<RequestEntry>) {
        for node in nodes {
            match node {
                TreeNode::Folder { children, .. } => collect(children, out),
                TreeNode::Request(entry) => out.push(entry.clone()),
            }
        }
    }
    fn find<'a>(nodes: &'a [TreeNode], id: &str) -> Option<&'a TreeNode> {
        for node in nodes {
            match node {
                TreeNode::Folder {
                    id: folder_id,
                    children,
                    ..
                } => {
                    if folder_id == id {
                        return Some(node);
                    }
                    if let Some(found) = find(children, id) {
                        return Some(found);
                    }
                }
                TreeNode::Request(entry) if entry.id == id => return Some(node),
                TreeNode::Request(_) => {}
            }
        }
        None
    }
    let mut out = Vec::new();
    let id = id.trim_matches('/');
    if id.is_empty() {
        collect(tree, &mut out);
    } else if let Some(node) = find(tree, id) {
        collect(std::slice::from_ref(node), &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{
        init_workspace, load_workspace, save_folder_scripts, save_request, Scripts,
    };
    use crate::environment::MemoryStore;
    use crate::http::HttpRequest;
    use crate::HttpMethod;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves `count` requests, answering each with the request line and Authorization header.
    async fn server(count: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..count {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap();
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let line = text.lines().next().unwrap_or("").to_string();
                let auth = text
                    .lines()
                    .find_map(|l| l.strip_prefix("authorization: "))
                    .unwrap_or("")
                    .to_string();
                let body = format!(r#"{{"line":"{line}","auth":"{auth}","token":"t-123"}}"#);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    fn entry(id: &str, url: &str, scripts: Scripts) -> RequestEntry {
        let mut request = HttpRequest::new(HttpMethod::Get, url);
        request.headers.push(crate::http::Header::new(
            "Authorization",
            "Bearer {{token}}",
        ));
        RequestEntry {
            id: id.into(),
            name: id.into(),
            protocol: Protocol::Http,
            request,
            scripts,
        }
    }

    #[tokio::test]
    async fn chains_requests_through_scripts_and_local_values() {
        let base = server(2).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let store = MemoryStore::default();
        save_folder_scripts(
            ws.path(),
            "",
            &Scripts {
                pre:
                    "pm.variables.set('calls', String(Number(pm.variables.get('calls') || 0) + 1));"
                        .into(),
                test: "pm.test('status 200', () => pm.response.to.have.status(200));".into(),
            },
        )
        .unwrap();

        let login = entry(
            "01-login.pulpo.yaml",
            &format!("{base}/login"),
            Scripts {
                pre: "pm.request.headers.remove('Authorization');".into(),
                test: "pm.environment.set('token', pm.response.json().token);".into(),
            },
        );
        let me = entry(
            "02-me.pulpo.yaml",
            &format!("{base}/me"),
            Scripts {
                pre: String::new(),
                test: "pm.test('sends the token', () => pm.expect(pm.response.json().auth).to.equal('Bearer t-123'));".into(),
            },
        );
        save_request(ws.path(), &login).unwrap();
        save_request(ws.path(), &me).unwrap();

        let first = run_entry(ws.path(), &login, None, &store).await;
        assert!(first.passed(), "{first:?}");
        assert!(
            first
                .response
                .as_ref()
                .unwrap()
                .body
                .contains(r#""auth":"""#),
            "auth header removed"
        );

        let second = run_entry(ws.path(), &me, None, &store).await;
        assert!(second.passed(), "{second:?}");
        assert_eq!(second.tests.len(), 2);

        let saved = local::load_local(ws.path(), None);
        assert_eq!(saved["token"], "t-123");
        assert_eq!(saved["calls"], "2");
    }

    #[tokio::test]
    async fn reports_missing_variables_without_sending() {
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let report = run_entry(
            ws.path(),
            &entry("01-x.pulpo.yaml", "{{base}}/x", Scripts::default()),
            None,
            &MemoryStore::default(),
        )
        .await;
        assert_eq!(
            report.error,
            Some(SendError::Unresolved("base, token".into()))
        );
        assert!(!report.passed());
    }

    #[test]
    fn lists_requests_under_a_folder_in_order() {
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let folder = collection::create_folder(ws.path(), "", "Orders").unwrap();
        collection::create_request(ws.path(), &folder, "Create").unwrap();
        collection::create_request(ws.path(), &folder, "List").unwrap();
        collection::create_request(ws.path(), "", "Health").unwrap();
        let tree = load_workspace(ws.path()).unwrap().tree;

        let names = |id: &str| {
            requests_under(&tree, id)
                .into_iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(""), ["Create", "List", "Health"]);
        assert_eq!(names(&folder), ["Create", "List"]);
        assert_eq!(names("02-health.pulpo.yaml"), ["Health"]);
        assert!(names("nope").is_empty());
    }
}
