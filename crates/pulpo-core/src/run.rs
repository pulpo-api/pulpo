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
use crate::oauth::{self, OAuth2, Obtained, TokenInfo, Tokens};
use crate::script::{self, Phase, Script, ScriptContext, TestResult};
use crate::snapshot;
use crate::variables::{self, Variables};
use crate::Protocol;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    /// OAuth 2.0 tokens of the environment.
    tokens: Tokens,
    /// Tokens obtained in this session, for [`Session::save_tokens`].
    new_tokens: Vec<String>,
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
            tokens: oauth::load_tokens(root, environment_id, store),
            new_tokens: Vec::new(),
            persist,
        })
    }

    /// The environment's values with the local ones on top.
    pub fn variables(&self) -> Variables {
        let mut vars = self.env.clone();
        vars.extend(self.local.clone());
        vars
    }

    /// An in-memory copy (same values and cookies) that saves nothing.
    pub fn detached(&self) -> Session {
        Session {
            environment_id: self.environment_id.clone(),
            environment_name: self.environment_name.clone(),
            env: self.env.clone(),
            local: self.local.clone(),
            jar: Arc::new(SharedJar::new(self.jar.snapshot())),
            tokens: self.tokens.clone(),
            new_tokens: Vec::new(),
            persist: false,
        }
    }

    /// `{{variables}}` of an OAuth 2.0 auth, with the environment and local values.
    fn resolve_oauth(&self, config: &OAuth2) -> Result<OAuth2, String> {
        let mut vars = self.env.clone();
        vars.extend(self.local.clone());
        let mut request = http::HttpRequest::new(crate::HttpMethod::Get, "");
        request.auth = Some(http::Auth::OAuth2(config.clone()));
        match variables::resolve_request(&request, &vars) {
            Ok(resolved) => match resolved.auth {
                Some(http::Auth::OAuth2(config)) => Ok(config),
                _ => unreachable!("resolving keeps the kind of auth"),
            },
            Err(missing) => Err(SendError::Unresolved(missing.join(", ")).to_string()),
        }
    }

    /// The access token for a (resolved) OAuth 2.0 auth, and a note when it is new.
    async fn oauth_token(
        &mut self,
        root: &Path,
        config: &OAuth2,
        settings: &http::Settings,
        vars: &Variables,
    ) -> Result<(String, Option<String>), String> {
        let client_pem =
            client_certificate(root, &config.token_url, vars).map_err(|e| e.to_string())?;
        let context = SendContext {
            root: Some(root),
            cookies: None,
            client_pem,
        };
        let (token, obtained) = oauth::token_for(config, &self.tokens, settings, &context).await?;
        if obtained == Obtained::Cached {
            return Ok((token.access_token, None));
        }
        let note = match (obtained, token.expires_at) {
            (Obtained::Refreshed, _) => "OAuth 2.0: refreshed the access token".to_string(),
            (_, Some(at)) => format!(
                "OAuth 2.0: got a new access token (expires in {} s)",
                at.saturating_sub(token.obtained_at)
            ),
            _ => "OAuth 2.0: got a new access token".to_string(),
        };
        let key = oauth::cache_key(config);
        self.tokens.insert(key.clone(), token.clone());
        if self.persist && !self.new_tokens.contains(&key) {
            self.new_tokens.push(key);
        }
        Ok((token.access_token, Some(note)))
    }

    /// Caches the tokens this session obtained (the secrets go to the keychain). On error
    /// they stay in memory for the rest of the session; returns what went wrong.
    pub fn save_tokens(&mut self, root: &Path, store: &dyn SecretStore) -> Vec<String> {
        let environment_id = self.environment_id.clone();
        std::mem::take(&mut self.new_tokens)
            .into_iter()
            .filter_map(|key| {
                let token = self.tokens.get(&key)?;
                oauth::save_token(root, environment_id.as_deref(), store, &key, Some(token))
                    .err()
                    .map(|e| format!("OAuth 2.0: {e}; the token is kept only for this run"))
            })
            .collect()
    }
}

/// Gets a new OAuth 2.0 token and caches it: a login in the browser (`open` shows the
/// page) for the authorization code grant, a token request for the others.
pub async fn oauth_login(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    config: &OAuth2,
    settings: &http::Settings,
    open: &(dyn Fn(&str) -> Result<(), String> + Sync),
) -> Result<TokenInfo, String> {
    let session = Session::open(root, environment_id, store, true)?;
    let config = session.resolve_oauth(config)?;
    let mut vars = session.env.clone();
    vars.extend(session.local.clone());
    let client_pem =
        client_certificate(root, &config.token_url, &vars).map_err(|e| e.to_string())?;
    let context = SendContext {
        root: Some(root),
        cookies: None,
        client_pem,
    };
    let token = match config.grant {
        oauth::Grant::AuthorizationCode => {
            oauth::authorize(&config, settings, &context, open, oauth::LOGIN_TIMEOUT).await?
        }
        _ => oauth::fetch(&config, settings, &context).await?,
    };
    let key = oauth::cache_key(&config);
    oauth::save_token(root, environment_id, store, &key, Some(&token))?;
    Ok(TokenInfo::of(&key, &token, oauth::now()))
}

/// A cached token and the requests whose auth uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToken {
    #[serde(flatten)]
    pub info: TokenInfo,
    /// Requests (ids) whose auth gives this token. Empty = none in the workspace now.
    pub used_by: Vec<String>,
    /// "Renew" can get a new one: a refresh token, a grant without the browser, or a
    /// request that holds the settings for a browser login.
    pub can_renew: bool,
    /// The auth has a revocation URL.
    pub can_revoke: bool,
}

/// An OAuth 2.0 auth (resolved), its settings and the requests that use it.
type UsedAuth = (OAuth2, http::Settings, Vec<String>);

/// OAuth 2.0 auths of the workspace's requests, resolved in the session, by cache key.
fn oauth_auths(root: &Path, session: &Session) -> Result<BTreeMap<String, UsedAuth>, String> {
    let workspace = collection::load_workspace(root).map_err(|e| e.to_string())?;
    let mut found: BTreeMap<String, UsedAuth> = BTreeMap::new();
    for entry in requests_under(&workspace.tree, "") {
        let Ok((http::Auth::OAuth2(config), _)) = collection::effective_auth(root, &entry) else {
            continue;
        };
        // An auth with variables the environment lacks cannot have a token here.
        let Ok(config) = session.resolve_oauth(&config) else {
            continue;
        };
        let settings = collection::effective_settings(root, &entry).map_err(|e| e.to_string())?;
        let key = oauth::cache_key(&config);
        found
            .entry(key)
            .or_insert_with(|| (config, settings, Vec::new()))
            .2
            .push(entry.id.clone());
    }
    Ok(found)
}

/// Every cached OAuth 2.0 token of an environment, newest first.
pub fn oauth_tokens(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
) -> Result<Vec<ActiveToken>, String> {
    let session = Session::open(root, environment_id, store, false)?;
    let auths = oauth_auths(root, &session)?;
    let now = oauth::now();
    let mut tokens: Vec<ActiveToken> = session
        .tokens
        .iter()
        .map(|(key, token)| {
            let auth = auths.get(key);
            ActiveToken {
                info: TokenInfo::of(key, token, now),
                used_by: auth.map(|(_, _, ids)| ids.clone()).unwrap_or_default(),
                can_renew: auth.is_some(),
                can_revoke: auth.is_some_and(|(config, _, _)| !config.revoke_url.is_empty()),
            }
        })
        .collect();
    tokens.sort_by_key(|t| std::cmp::Reverse(t.info.obtained_at));
    Ok(tokens)
}

/// The OAuth 2.0 auth (resolved) and settings behind a cached token.
fn auth_for_key(
    root: &Path,
    session: &Session,
    key: &str,
) -> Result<(OAuth2, http::Settings), String> {
    oauth_auths(root, session)?
        .remove(key)
        .map(|(config, settings, _)| (config, settings))
        .ok_or_else(|| {
            "no request of the workspace uses this token's settings any more; forget it instead"
                .to_string()
        })
}

fn oauth_context<'a>(
    root: &'a Path,
    session: &Session,
    config: &OAuth2,
) -> Result<SendContext<'a>, String> {
    let mut vars = session.env.clone();
    vars.extend(session.local.clone());
    let client_pem =
        client_certificate(root, &config.token_url, &vars).map_err(|e| e.to_string())?;
    Ok(SendContext {
        root: Some(root),
        cookies: None,
        client_pem,
    })
}

/// Gets a fresh token for a cached one: with its refresh token, a new token request, or a
/// browser login (authorization code without a refresh token).
pub async fn oauth_renew(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    key: &str,
    open: &(dyn Fn(&str) -> Result<(), String> + Sync),
) -> Result<TokenInfo, String> {
    let session = Session::open(root, environment_id, store, true)?;
    let (config, settings) = auth_for_key(root, &session, key)?;
    let context = oauth_context(root, &session, &config)?;
    let refreshed = match session.tokens.get(key) {
        Some(oauth::Token {
            refresh_token: Some(refresh),
            ..
        }) => oauth::refresh(&config, refresh, &settings, &context)
            .await
            .ok(),
        _ => None,
    };
    let token = match refreshed {
        Some(token) => token,
        None => match config.grant {
            oauth::Grant::AuthorizationCode => {
                oauth::authorize(&config, &settings, &context, open, oauth::LOGIN_TIMEOUT).await?
            }
            _ => oauth::fetch(&config, &settings, &context).await?,
        },
    };
    oauth::save_token(root, environment_id, store, key, Some(&token))?;
    Ok(TokenInfo::of(key, &token, oauth::now()))
}

/// Revokes a cached token at the identity provider and forgets it. Returns a note when it
/// could only be forgotten (no revocation URL).
pub async fn oauth_revoke(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    key: &str,
) -> Result<Option<String>, String> {
    let session = Session::open(root, environment_id, store, false)?;
    let token = session
        .tokens
        .get(key)
        .ok_or("this token is not cached any more")?;
    let note = match auth_for_key(root, &session, key) {
        Ok((config, settings)) if !config.revoke_url.is_empty() => {
            let context = oauth_context(root, &session, &config)?;
            oauth::revoke(&config, token, &settings, &context).await?;
            None
        }
        _ => Some("no revocation URL: the token was only forgotten here".to_string()),
    };
    oauth::save_token(root, environment_id, store, key, None)?;
    Ok(note)
}

/// Forgets a cached token by its key, without asking the identity provider.
pub fn oauth_forget_key(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    key: &str,
) -> Result<(), String> {
    oauth::save_token(root, environment_id, store, key, None)
}

/// The cached token of an OAuth 2.0 auth in an environment, if any.
pub fn oauth_status(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    config: &OAuth2,
) -> Result<Option<TokenInfo>, String> {
    let session = Session::open(root, environment_id, store, false)?;
    let key = oauth::cache_key(&session.resolve_oauth(config)?);
    Ok(session
        .tokens
        .get(&key)
        .map(|token| TokenInfo::of(&key, token, oauth::now())))
}

/// Forgets the cached token of an OAuth 2.0 auth in an environment.
pub fn oauth_forget(
    root: &Path,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
    config: &OAuth2,
) -> Result<(), String> {
    let session = Session::open(root, environment_id, store, false)?;
    let key = oauth::cache_key(&session.resolve_oauth(config)?);
    oauth::save_token(root, environment_id, store, &key, None)
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
        Ok(mut session) => {
            let mut report = run_in(root, entry, &mut session, options).await;
            report.logs.extend(session.save_tokens(root, store));
            report
        }
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
/// A request ready to open a connection (WebSocket): folder headers, auth, settings and
/// variables (environment and local values) applied, OAuth 2.0 token included. Also
/// returns the variables, to resolve messages typed later. Scripts do not run here.
pub async fn prepare(
    root: &Path,
    entry: &RequestEntry,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
) -> Result<(http::HttpRequest, Variables), String> {
    let mut session = Session::open(root, environment_id, store, false)?.detached();
    let mut effective = entry.request.clone();
    let (auth, _) = collection::effective_auth(root, entry).map_err(|e| e.to_string())?;
    effective.auth = Some(auth);
    effective.settings = collection::effective_settings(root, entry).map_err(|e| e.to_string())?;
    effective.headers = collection::effective_headers(root, entry).map_err(|e| e.to_string())?;
    let vars = session.variables();
    let mut resolved = variables::resolve_request(&effective, &vars)
        .map_err(|missing| SendError::Unresolved(missing.join(", ")).to_string())?;
    if let Some(http::Auth::OAuth2(config)) = resolved.auth.clone() {
        let (token, _) = session
            .oauth_token(root, &config, &resolved.settings, &vars)
            .await?;
        resolved.auth = None;
        oauth::apply_token(&mut resolved.headers, &config.header_prefix, &token);
    }
    Ok((with_auth(&resolved), vars))
}

/// The request with its (simple) auth written into headers or the URL, for protocols
/// that do not go through `http::send_with` (WebSocket handshakes).
pub fn with_auth(request: &http::HttpRequest) -> http::HttpRequest {
    let mut out = request.clone();
    if let Some(auth) = request.auth.clone() {
        http::apply_auth(&mut out.url, &mut out.headers, &auth);
        out.auth = None;
    }
    out
}

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
    if !matches!(
        entry.protocol,
        Protocol::Http | Protocol::Graphql | Protocol::Websocket | Protocol::Sse
    ) {
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
        Ok((mut resolved, client_pem)) => 'send: {
            if let Some(http::Auth::OAuth2(config)) = resolved.auth.clone() {
                match session
                    .oauth_token(root, &config, &resolved.settings, &vars)
                    .await
                {
                    Ok((token, note)) => {
                        report.logs.extend(note);
                        resolved.auth = None;
                        oauth::apply_token(&mut resolved.headers, &config.header_prefix, &token);
                    }
                    Err(e) => {
                        report.error = Some(SendError::Other(e));
                        break 'send;
                    }
                }
            }
            let send_context = SendContext {
                root: Some(root),
                cookies: Some(jar.clone()),
                client_pem,
            };
            let websocket = crate::websocket::is_websocket_url(&resolved.url);
            let stream = crate::sse::is_event_stream(&resolved);
            let mut sent = if websocket {
                crate::websocket::exchange(&with_auth(&resolved)).await
            } else if stream {
                crate::sse::collect(&with_auth(&resolved)).await
            } else {
                http::send_with(&resolved, &send_context).await
            };
            report.attempts = 1;
            while !websocket && !stream && retryable(&sent) && report.attempts <= options.retries {
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

    /// A fake identity server: `/token` hands out tokens by grant, anything else echoes
    /// the Authorization header. Records each token request as `auth|body`.
    async fn identity_server(log: Arc<std::sync::Mutex<Vec<String>>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut refreshed = 0;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut data = Vec::new();
                let mut buffer = vec![0u8; 8192];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    data.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break;
                        }
                    }
                    if read == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&data).to_string();
                let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                let line = head.lines().next().unwrap_or("").to_string();
                let auth = head
                    .lines()
                    .find_map(|l| l.strip_prefix("authorization: "))
                    .unwrap_or("")
                    .to_string();
                let reply = if line.starts_with("POST /token") {
                    log.lock().unwrap().push(format!("{auth}|{body}"));
                    if body.contains("grant_type=client_credentials") {
                        r#"{"access_token":"cc-1","token_type":"Bearer","expires_in":1,"refresh_token":"r-1"}"#.to_string()
                    } else if body.contains("grant_type=refresh_token") {
                        refreshed += 1;
                        format!(
                            r#"{{"access_token":"cc-{}","expires_in":3600}}"#,
                            refreshed + 1
                        )
                    } else if body.contains("grant_type=authorization_code")
                        && body.contains("code=abc")
                    {
                        r#"{"access_token":"ac-1","expires_in":3600}"#.to_string()
                    } else {
                        r#"{"error":"invalid_grant","error_description":"bad code"}"#.to_string()
                    }
                } else if line.starts_with("POST /revoke") {
                    log.lock().unwrap().push(format!("revoke|{body}"));
                    "{}".to_string()
                } else {
                    format!(r#"{{"auth":"{auth}"}}"#)
                };
                let status = if reply.contains("\"error\"") {
                    "400 Bad Request"
                } else {
                    "200 OK"
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://{address}")
    }

    fn oauth_request(base: &str, config: OAuth2) -> RequestEntry {
        let mut request = HttpRequest::new(HttpMethod::Get, format!("{base}/api"));
        request.auth = Some(http::Auth::OAuth2(config));
        RequestEntry {
            id: "01-api.pulpo.yaml".into(),
            name: "api".into(),
            protocol: Protocol::Http,
            request,
            scripts: Scripts::default(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetches_caches_and_refreshes_client_credentials_tokens() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let base = identity_server(log.clone()).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        local::apply_changes(
            ws.path(),
            None,
            &HashMap::from([("secret".to_string(), Some("s3".to_string()))]),
        )
        .unwrap();
        let config = OAuth2 {
            grant: oauth::Grant::ClientCredentials,
            token_url: format!("{base}/token"),
            client_id: "app".into(),
            client_secret: "{{secret}}".into(),
            scope: "read".into(),
            ..Default::default()
        };
        let entry = oauth_request(&base, config.clone());
        let store = MemoryStore::default();
        let body = |r: &RunReport| {
            r.response
                .as_ref()
                .map(|r| r.body.clone())
                .unwrap_or_default()
        };

        let first = run_entry(ws.path(), &entry, None, &store).await;
        assert!(first.passed(), "{first:?}");
        assert_eq!(body(&first), r#"{"auth":"Bearer cc-1"}"#);
        assert!(
            first
                .logs
                .iter()
                .any(|l| l.contains("got a new access token")),
            "{:?}",
            first.logs
        );
        let sent = log.lock().unwrap()[0].clone();
        assert!(
            sent.starts_with("Basic YXBwOnMz|"),
            "client secret in the header: {sent}"
        );
        assert!(sent.contains("scope=read"), "{sent}");

        // cc-1 expires in 1 s (inside the safety margin): the refresh token gets a new one.
        let second = run_entry(ws.path(), &entry, None, &store).await;
        assert_eq!(body(&second), r#"{"auth":"Bearer cc-2"}"#, "{second:?}");
        assert!(log.lock().unwrap()[1].contains("grant_type=refresh_token"));

        // cc-2 is valid for an hour: no token request at all.
        let third = run_entry(ws.path(), &entry, None, &store).await;
        assert_eq!(body(&third), r#"{"auth":"Bearer cc-2"}"#);
        assert!(third.logs.is_empty(), "{:?}", third.logs);
        assert_eq!(log.lock().unwrap().len(), 2);

        let status = oauth_status(ws.path(), None, &store, &config)
            .unwrap()
            .unwrap();
        assert!(status.valid && status.has_refresh);
        assert_eq!(status.preview, "cc-2");
        oauth_forget(ws.path(), None, &store, &config).unwrap();
        assert_eq!(
            oauth_status(ws.path(), None, &store, &config).unwrap(),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_renews_and_revokes_active_tokens() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let base = identity_server(log.clone()).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let config = OAuth2 {
            grant: oauth::Grant::ClientCredentials,
            token_url: format!("{base}/token"),
            revoke_url: format!("{base}/revoke"),
            client_id: "app".into(),
            client_secret: "s3".into(),
            ..Default::default()
        };
        let entry = oauth_request(&base, config);
        collection::save_request(ws.path(), &entry).unwrap();
        let store = MemoryStore::default();
        assert!(oauth_tokens(ws.path(), None, &store).unwrap().is_empty());

        let report = run_entry(ws.path(), &entry, None, &store).await;
        assert!(report.passed(), "{report:?}");
        let tokens = oauth_tokens(ws.path(), None, &store).unwrap();
        assert_eq!(tokens.len(), 1);
        let token = &tokens[0];
        assert_eq!(token.info.preview, "cc-1");
        assert_eq!(token.used_by, vec![entry.id.clone()]);
        assert!(token.can_renew && token.can_revoke);

        // Renew uses the refresh token.
        let never = |_: &str| -> Result<(), String> { panic!("no browser for this grant") };
        let renewed = oauth_renew(ws.path(), None, &store, &token.info.key, &never)
            .await
            .unwrap();
        assert_eq!(renewed.preview, "cc-2");
        assert!(log.lock().unwrap()[1].contains("grant_type=refresh_token"));

        // Revoke sends the refresh token (it ends its access tokens too) and forgets it.
        let note = oauth_revoke(ws.path(), None, &store, &token.info.key)
            .await
            .unwrap();
        assert_eq!(note, None);
        let revoked = log.lock().unwrap()[2].clone();
        assert!(
            revoked.starts_with("revoke|token=r-1&token_type_hint=refresh_token"),
            "{revoked}"
        );
        assert!(oauth_tokens(ws.path(), None, &store).unwrap().is_empty());

        // A token no request uses any more can only be forgotten.
        run_entry(ws.path(), &entry, None, &store).await;
        collection::delete_request(ws.path(), &entry.id).unwrap();
        let orphan = oauth_tokens(ws.path(), None, &store).unwrap().remove(0);
        assert!(orphan.used_by.is_empty() && !orphan.can_renew && !orphan.can_revoke);
        let error = oauth_renew(ws.path(), None, &store, &orphan.info.key, &never)
            .await
            .unwrap_err();
        assert!(error.contains("forget it"), "{error}");
        oauth_forget_key(ws.path(), None, &store, &orphan.info.key).unwrap();
        assert!(oauth_tokens(ws.path(), None, &store).unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn logs_in_with_the_authorization_code_and_pkce() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let base = identity_server(log.clone()).await;
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let config = OAuth2 {
            grant: oauth::Grant::AuthorizationCode,
            auth_url: format!("{base}/authorize"),
            token_url: format!("{base}/token"),
            client_id: "spa".into(),
            client_auth: oauth::ClientAuth::Body,
            ..Default::default()
        };
        let entry = oauth_request(&base, config.clone());
        let store = MemoryStore::default();

        let before = run_entry(ws.path(), &entry, None, &store).await;
        let error = before.error.map(|e| e.to_string()).unwrap_or_default();
        assert!(error.contains("Get new access token"), "{error}");

        // The "browser": reads the login URL and comes back to the redirect URI.
        let challenge = Arc::new(std::sync::Mutex::new(String::new()));
        let seen = challenge.clone();
        let browser = move |url: &str| -> Result<(), String> {
            let query = url.split_once('?').unwrap().1.to_string();
            let param = |name: &str| {
                query
                    .split('&')
                    .find_map(|p| p.strip_prefix(&format!("{name}=")))
                    .unwrap()
                    .to_string()
            };
            *seen.lock().unwrap() = param("code_challenge");
            let redirect = param("redirect_uri")
                .replace("%3A", ":")
                .replace("%2F", "/");
            let state = param("state");
            let rest = redirect.strip_prefix("http://").unwrap().to_string();
            let (host, path) = rest.split_once('/').unwrap();
            let (host, path) = (host.to_string(), path.to_string());
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut stream = std::net::TcpStream::connect(&host).unwrap();
                write!(
                    stream,
                    "GET /{path}?code=abc&state={state} HTTP/1.1\r\nhost: {host}\r\n\r\n"
                )
                .unwrap();
                let mut page = String::new();
                let _ = stream.read_to_string(&mut page);
                assert!(page.contains("Logged in"), "{page}");
            });
            Ok(())
        };
        let info = oauth_login(
            ws.path(),
            None,
            &store,
            &config,
            &http::Settings::default(),
            &browser,
        )
        .await
        .unwrap();
        assert_eq!(info.preview, "ac-1");
        let sent = log.lock().unwrap()[0].clone();
        assert!(
            sent.starts_with('|'),
            "no Authorization header for a public client: {sent}"
        );
        assert!(sent.contains("client_id=spa"), "{sent}");
        let verifier = sent
            .split('|')
            .nth(1)
            .unwrap()
            .split('&')
            .find_map(|p| p.strip_prefix("code_verifier="))
            .unwrap()
            .to_string();
        assert_eq!(oauth::pkce_challenge(&verifier), *challenge.lock().unwrap());

        let after = run_entry(ws.path(), &entry, None, &store).await;
        assert_eq!(
            after.response.map(|r| r.body).unwrap_or_default(),
            r#"{"auth":"Bearer ac-1"}"#
        );
    }
}
