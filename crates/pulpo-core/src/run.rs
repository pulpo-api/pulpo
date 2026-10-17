//! Running a request the way the app and `pulpo run` do: folder and request scripts,
//! variables (environment + local values + iteration data), send (with retries), test
//! scripts, snapshots, save local values.

use crate::certificates;
use crate::collection::{self, CollectionError, RequestEntry, TreeNode};
use crate::cookies::{self, SharedJar};
use crate::data::Row;
use crate::environment::{self, SecretStore};
use crate::http::{self, HttpResponse, SendContext, SendError};
use crate::local;
use crate::script::{self, Phase, Script, ScriptContext, TestResult};
use crate::snapshot;
use crate::variables::{self, Variables};
use crate::Protocol;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

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
    /// 0-based iteration of a run with several (data rows or `--iterations`).
    #[serde(default)]
    pub iteration: usize,
    /// Times the request was sent (more than 1 after retries).
    #[serde(default)]
    pub attempts: u32,
}

impl RunReport {
    /// A request passes when it got a response, no test failed and no script broke.
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.script_errors.is_empty() && self.tests.iter().all(|t| t.passed)
    }
}

/// How one request runs inside a bigger run: its iteration and data, retries, snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOptions {
    #[serde(default)]
    pub iteration: usize,
    /// How many iterations the run has (0 counts as 1).
    #[serde(default)]
    pub iterations: usize,
    /// The iteration's data row: `{{name}}` and `pm.iterationData`.
    #[serde(default)]
    pub data: Row,
    /// Extra attempts after a network error or a 5xx response.
    #[serde(default)]
    pub retries: u32,
    /// Rewrite snapshots instead of comparing with them.
    #[serde(default)]
    pub update_snapshots: bool,
}

/// The state that requests of one run share: environment, local values and cookies.
/// A persistent session saves local values and cookies after each request (the app and
/// `pulpo run`); a load test keeps them in memory, one session per virtual user.
pub struct Session {
    environment_id: Option<String>,
    environment_name: String,
    env: Variables,
    local: HashMap<String, String>,
    jar: Arc<SharedJar>,
    persist: bool,
}

impl Session {
    pub fn open(
        root: &Path,
        environment_id: Option<&str>,
        store: &dyn SecretStore,
        persist: bool,
    ) -> Result<Session, String> {
        let (env, environment_name) = match environment_id {
            Some(id) => environment::variables_for(root, id, store)
                .and_then(|vars| {
                    environment::load_environment(root, id, store, false).map(|e| (vars, e.name))
                })
                .map_err(|e| e.to_string())?,
            None => (Variables::new(), String::new()),
        };
        Ok(Session {
            environment_id: environment_id.map(str::to_string),
            environment_name,
            env,
            local: local::load_local(root, environment_id),
            jar: Arc::new(SharedJar::new(cookies::load_jar(root, environment_id))),
            persist,
        })
    }

    /// An in-memory copy (same values and cookies) that saves nothing.
    pub fn detached(&self) -> Session {
        Session {
            environment_id: self.environment_id.clone(),
            environment_name: self.environment_name.clone(),
            env: self.env.clone(),
            local: self.local.clone(),
            jar: Arc::new(SharedJar::new(self.jar.snapshot())),
            persist: false,
        }
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
    run_entry_with(root, entry, environment_id, store, &RunOptions::default()).await
}

/// [`run_entry`] as one step of a run with iterations, data and retries.
pub async fn run_entry_with(
    root: &Path,
    entry: &RequestEntry,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    options: &RunOptions,
) -> RunReport {
    match Session::open(root, environment_id, store, true) {
        Ok(mut session) => run_in(root, entry, &mut session, options).await,
        Err(e) => RunReport {
            id: entry.id.clone(),
            name: entry.name.clone(),
            iteration: options.iteration,
            error: Some(SendError::Other(e)),
            ..Default::default()
        },
    }
}

/// A network failure or a server error: worth sending again.
fn retryable(outcome: &Result<HttpResponse, SendError>) -> bool {
    match outcome {
        Ok(response) => response.status >= 500,
        Err(SendError::Timeout(_) | SendError::Connect(_)) => true,
        Err(_) => false,
    }
}

/// Runs one request inside a session, which keeps the values its scripts set.
pub async fn run_in(
    root: &Path,
    entry: &RequestEntry,
    session: &mut Session,
    options: &RunOptions,
) -> RunReport {
    let mut report = RunReport {
        id: entry.id.clone(),
        name: entry.name.clone(),
        iteration: options.iteration,
        ..Default::default()
    };
    if entry.protocol != Protocol::Http {
        report.error = Some(SendError::Other(format!(
            "{} requests arrive in phase 4",
            entry.protocol.as_str()
        )));
        return report;
    }

    let (pre, test) = match scripts_for(root, entry) {
        Ok(scripts) => scripts,
        Err(e) => {
            report.error = Some(SendError::Other(e.to_string()));
            return report;
        }
    };

    // The request carries the auth it really uses (its own or a folder's), so scripts
    // see it and `send` applies it.
    // Settings work the same way, field by field; folder headers are added too.
    let mut effective = entry.request.clone();
    let inherited = collection::effective_auth(root, entry).and_then(|(auth, _)| {
        Ok((
            auth,
            collection::effective_settings(root, entry)?,
            collection::effective_headers(root, entry)?,
        ))
    });
    match inherited {
        Ok((auth, settings, headers)) => {
            effective.auth = Some(auth);
            effective.settings = settings;
            effective.headers = headers;
        }
        Err(e) => {
            report.error = Some(SendError::Other(e.to_string()));
            return report;
        }
    }

    // One jar for the whole request: pm.sendRequest logins, redirects and the request itself.
    let jar = session.jar.clone();
    let iteration_count = options.iterations.max(1);

    let context = ScriptContext {
        request_name: &entry.name,
        environment_name: &session.environment_name,
        env: &session.env,
        local: &session.local,
        cookies: Some(jar.clone()),
        data: Some(&options.data),
        iteration: options.iteration,
        iteration_count,
    };
    let (request, outcome) = script::run_phase(Phase::PreRequest, &pre, &context, &effective, None);
    let mut all_changes = outcome.changes.clone();
    merge(&mut session.local, &outcome.changes);
    report.logs.extend(outcome.logs);
    report.script_errors.extend(outcome.errors);
    report.tests.extend(outcome.tests);

    // Environment < values kept from earlier requests < the iteration's data < values set
    // by this request's pre-request scripts.
    let mut vars = session.env.clone();
    vars.extend(session.local.clone());
    vars.extend(options.data.clone());
    for (name, value) in &outcome.changes {
        if let Some(value) = value {
            vars.insert(name.clone(), value.clone());
        }
    }
    let prepared = variables::resolve_request(&request, &vars)
        .map_err(|missing| SendError::Unresolved(missing.join(", ")))
        .and_then(|resolved| {
            let client_pem = client_certificate(root, &resolved.url, &vars)?;
            Ok((resolved, client_pem))
        });
    match prepared {
        Err(e) => report.error = Some(e),
        Ok((resolved, client_pem)) => {
            let send_context = SendContext {
                root: Some(root),
                cookies: Some(jar.clone()),
                client_pem,
            };
            let mut sent = http::send_with(&resolved, &send_context).await;
            report.attempts = 1;
            while retryable(&sent) && report.attempts <= options.retries {
                report.attempts += 1;
                sent = http::send_with(&resolved, &send_context).await;
            }
            match sent {
                Err(e) => report.error = Some(e),
                Ok(response) => {
                    let context = ScriptContext {
                        request_name: &entry.name,
                        environment_name: &session.environment_name,
                        env: &session.env,
                        local: &session.local,
                        cookies: Some(jar.clone()),
                        data: Some(&options.data),
                        iteration: options.iteration,
                        iteration_count,
                    };
                    let (_, outcome) =
                        script::run_phase(Phase::Test, &test, &context, &resolved, Some(&response));
                    for (k, v) in &outcome.changes {
                        all_changes.insert(k.clone(), v.clone());
                    }
                    merge(&mut session.local, &outcome.changes);
                    report.logs.extend(outcome.logs);
                    report.script_errors.extend(outcome.errors);
                    report.tests.extend(outcome.tests);
                    // A load test (no persistence) neither writes nor checks snapshots.
                    if session.persist && !outcome.snapshots.is_empty() {
                        report.tests.extend(snapshot::check(
                            root,
                            &entry.id,
                            &response,
                            &outcome.snapshots,
                            options.update_snapshots,
                        ));
                    }
                    report.response = Some(response);
                }
            }
        }
    }

    if session.persist {
        let environment_id = session.environment_id.as_deref();
        if jar.changed() {
            if let Err(e) = cookies::save_jar(root, environment_id, &jar.snapshot()) {
                report
                    .script_errors
                    .push(format!("could not save cookies: {e}"));
            }
        }
        if let Err(e) = local::apply_changes(root, environment_id, &all_changes) {
            report
                .script_errors
                .push(format!("could not save local values: {e}"));
        }
    }
    report
}

/// The client certificate (mTLS) set up on this machine for the URL's host, as PEM.
fn client_certificate(
    root: &Path,
    url: &str,
    vars: &Variables,
) -> Result<Option<Vec<u8>>, SendError> {
    let Ok(url) = reqwest::Url::parse(url) else {
        return Ok(None);
    };
    let all = certificates::load_certificates(root);
    let Some(certificate) = certificates::find(&all, &url) else {
        return Ok(None);
    };
    let mut missing = BTreeSet::new();
    let passphrase = variables::resolve_text(
        certificate.passphrase.as_deref().unwrap_or(""),
        vars,
        &mut missing,
    );
    if !missing.is_empty() {
        return Err(SendError::Unresolved(
            missing.into_iter().collect::<Vec<_>>().join(", "),
        ));
    }
    certificates::identity_pem(root, certificate, &passphrase)
        .map(Some)
        .map_err(SendError::File)
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

    /// `/login` redirects to `/home` setting a cookie; every response echoes the Cookie header.
    async fn cookie_server(count: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..count {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap();
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                let cookie = text
                    .lines()
                    .find_map(|l| l.strip_prefix("cookie: "))
                    .unwrap_or("")
                    .to_string();
                let reply = if path == "/login" {
                    "HTTP/1.1 302 Found\r\nlocation: /home\r\nset-cookie: sid=xyz; Path=/; HttpOnly\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    let body = format!(r#"{{"path":"{path}","cookie":"{cookie}"}}"#);
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn keeps_cookies_across_redirects_and_requests() {
        // login + redirected home, then me, then a request with cookies off.
        let base = cookie_server(4).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let store = MemoryStore::default();
        let plain = |id: &str, path: &str| RequestEntry {
            id: id.into(),
            name: id.into(),
            protocol: Protocol::Http,
            request: HttpRequest::new(HttpMethod::Get, format!("{base}{path}")),
            scripts: Scripts::default(),
        };

        let login = plain("01-login.pulpo.yaml", "/login");
        let first = run_entry(ws.path(), &login, None, &store).await;
        assert!(
            first
                .response
                .unwrap()
                .body
                .contains(r#""cookie":"sid=xyz""#),
            "the cookie set during the redirect is sent to /home"
        );

        let mut me = plain("02-me.pulpo.yaml", "/me");
        me.scripts.test =
            "pm.test('jar', () => pm.expect(pm.cookies.get('sid')).to.equal('xyz'));".into();
        let second = run_entry(ws.path(), &me, None, &store).await;
        assert!(second.passed(), "{second:?}");
        assert_eq!(second.tests.len(), 1);
        assert!(second
            .response
            .unwrap()
            .body
            .contains(r#""cookie":"sid=xyz""#));
        assert_eq!(
            cookies::load_jar(ws.path(), None).cookies[0].name,
            "sid",
            "the jar is saved in .pulpo/"
        );

        let mut off = plain("03-off.pulpo.yaml", "/off");
        off.request.cookies = false;
        let third = run_entry(ws.path(), &off, None, &store).await;
        assert!(third.response.unwrap().body.contains(r#""cookie":"""#));
    }

    #[tokio::test]
    async fn sends_the_auth_inherited_from_the_workspace() {
        let base = server(1).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        crate::collection::save_folder_auth(
            ws.path(),
            "",
            Some(&crate::http::Auth::Basic {
                username: "{{user}}".into(),
                password: "pw".into(),
            }),
        )
        .unwrap();
        let request = RequestEntry {
            id: "01-me.pulpo.yaml".into(),
            name: "me".into(),
            protocol: Protocol::Http,
            request: HttpRequest::new(HttpMethod::Get, format!("{base}/me")),
            scripts: Scripts::default(),
        };
        save_request(ws.path(), &request).unwrap();
        local::apply_changes(
            ws.path(),
            None,
            &HashMap::from([("user".to_string(), Some("ana".to_string()))]),
        )
        .unwrap();

        let report = run_entry(ws.path(), &request, None, &MemoryStore::default()).await;
        assert!(report.passed(), "{report:?}");
        assert!(
            report
                .response
                .unwrap()
                .body
                .contains(r#""auth":"Basic YW5hOnB3""#),
            "variables are resolved before encoding"
        );
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

    /// Answers each connection with the next `(status, body)`, echoing the request line
    /// into `{line}` in the body.
    async fn scripted_server(replies: Vec<(u16, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for (status, body) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap();
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let line = text.lines().next().unwrap_or("").to_string();
                let body = body.replace("{line}", &line);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn iteration_data_fills_variables_and_scripts() {
        let base = scripted_server(vec![(200, r#"{"line":"{line}"}"#)]).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        local::apply_changes(
            ws.path(),
            None,
            &HashMap::from([("user".to_string(), Some("kept".to_string()))]),
        )
        .unwrap();
        let mut request = entry(
            "01-user.pulpo.yaml",
            &format!("{base}/users/{{{{user}}}}"),
            Scripts {
                pre: String::new(),
                test: "pm.test('data', () => {
                    pm.expect(pm.iterationData.get('user')).to.equal('ana');
                    pm.expect(pm.variables.get('user')).to.equal('ana');
                    pm.expect(pm.info.iteration).to.equal(1);
                    pm.expect(pm.info.iterationCount).to.equal(2);
                });"
                .into(),
            },
        );
        request.request.headers.clear();
        let options = RunOptions {
            iteration: 1,
            iterations: 2,
            data: HashMap::from([("user".to_string(), "ana".to_string())]),
            ..Default::default()
        };
        let report =
            run_entry_with(ws.path(), &request, None, &MemoryStore::default(), &options).await;
        assert!(report.passed(), "{report:?}");
        assert_eq!(report.iteration, 1);
        assert!(
            report.response.unwrap().body.contains("GET /users/ana "),
            "data wins over kept values"
        );
    }

    #[tokio::test]
    async fn retries_server_errors_only_as_asked() {
        let base = scripted_server(vec![(503, "{}"), (502, "{}"), (200, "{}"), (500, "{}")]).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let mut request = entry("01-x.pulpo.yaml", &format!("{base}/x"), Scripts::default());
        request.request.headers.clear();
        let store = MemoryStore::default();
        let retried = run_entry_with(
            ws.path(),
            &request,
            None,
            &store,
            &RunOptions {
                retries: 2,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(retried.attempts, 3);
        assert_eq!(retried.response.unwrap().status, 200);
        let once = run_entry(ws.path(), &request, None, &store).await;
        assert_eq!((once.attempts, once.response.unwrap().status), (1, 500));
    }

    #[tokio::test]
    async fn scripts_check_schemas_and_snapshots() {
        let body = r#"{"id":7,"at":"now"}"#;
        let base = scripted_server(vec![(200, body), (200, r#"{"id":"7","at":"later"}"#)]).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let mut request = entry(
            "01-order.pulpo.yaml",
            &format!("{base}/order"),
            Scripts {
                pre: String::new(),
                test: r#"pm.test('schema', () => pm.response.to.have.jsonSchema({
                        type: 'object', required: ['id'], properties: { id: { type: 'integer' } } }));
                    pm.test('contract', () => pm.response.to.have.contract({ '2XX': { type: 'object' }, '404': null }));
                    pm.test('undocumented', () => pm.response.to.have.contract({ '404': null }));
                    pm.response.to.matchSnapshot({ ignore: ['$.at'] });"#
                    .into(),
            },
        );
        request.request.headers.clear();
        save_request(ws.path(), &request).unwrap();
        let store = MemoryStore::default();

        let first = run_entry(ws.path(), &request, None, &store).await;
        let results: Vec<(&str, bool)> = first
            .tests
            .iter()
            .map(|t| (t.name.as_str(), t.passed))
            .collect();
        assert_eq!(
            results,
            [
                ("schema", true),
                ("contract", true),
                ("undocumented", false),
                ("Snapshot \"response\" saved", true)
            ]
        );
        assert!(first.tests[2]
            .error
            .as_deref()
            .unwrap()
            .contains("status 200 is not documented"));

        let second = run_entry(ws.path(), &request, None, &store).await;
        let schema = &second.tests[0];
        assert!(!schema.passed);
        assert!(
            schema.error.as_deref().unwrap().contains("/id:"),
            "{schema:?}"
        );
        let snapshot = &second.tests[3];
        assert_eq!(
            snapshot.error.as_deref(),
            Some("changed: $.body.id: 7 → \"7\""),
            "the ignored field does not count"
        );
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
