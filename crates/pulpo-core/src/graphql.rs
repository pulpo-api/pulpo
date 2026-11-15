//! GraphQL: the schema of an endpoint, read by introspection.
//!
//! The request itself is sent like any other (auth, headers, environment and pre-request
//! scripts apply), with the introspection query instead of its own. The result is cached
//! in `.pulpo/graphql/` per endpoint, so the app has it at hand offline and can say what
//! changed since the last time.

use crate::collection::{io_error, CollectionError, RequestEntry};
use crate::environment::SecretStore;
use crate::http::Graphql;
use crate::local::ensure_local_dir;
use crate::run::{self, RunOptions, Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The standard introspection query (what GraphiQL and the GraphQL tools send).
pub const INTROSPECTION_QUERY: &str = r#"query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types { ...FullType }
    directives {
      name
      description
      locations
      args { ...InputValue }
    }
  }
}
fragment FullType on __Type {
  kind
  name
  description
  fields(includeDeprecated: true) {
    name
    description
    args { ...InputValue }
    type { ...TypeRef }
    isDeprecated
    deprecationReason
  }
  inputFields { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) {
    name
    description
    isDeprecated
    deprecationReason
  }
  possibleTypes { ...TypeRef }
}
fragment InputValue on __InputValue {
  name
  description
  type { ...TypeRef }
  defaultValue
}
fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType {
        kind
        name
        ofType {
          kind
          name
          ofType {
            kind
            name
            ofType { kind name ofType { kind name } }
          }
        }
      }
    }
  }
}"#;

/// A schema read from an endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    /// The introspection result, `{"__schema": …}`, as JSON text (what graphql-js reads).
    pub introspection: String,
    /// `Type` and `Type.field` names new since the cached schema.
    pub added: Vec<String>,
    /// `Type` and `Type.field` names gone since the cached schema.
    pub removed: Vec<String>,
    /// Whether there was a cached schema to compare with.
    pub compared: bool,
    /// Unix seconds when it was read.
    pub fetched_at: u64,
    /// True when it comes from the cache (the endpoint was not asked).
    pub cached: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cached {
    url: String,
    fetched_at: u64,
    schema: Value,
}

fn cache_file(root: &Path, url: &str) -> Result<PathBuf, CollectionError> {
    let dir = ensure_local_dir(root)?.join("graphql");
    fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    let digest = Sha256::digest(url.trim().as_bytes());
    let key: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok(dir.join(format!("{key}.json")))
}

fn read_cache(root: &Path, url: &str) -> Option<Cached> {
    let file = cache_file(root, url).ok()?;
    serde_json::from_str(&fs::read_to_string(file).ok()?).ok()
}

/// `Type` and `Type.field` (also input fields and enum values) of an introspection result.
pub fn names_of(schema: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let types = schema
        .pointer("/__schema/types")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for t in &types {
        let Some(name) = t.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.starts_with("__") {
            continue;
        }
        out.insert(name.to_string());
        for key in ["fields", "inputFields", "enumValues"] {
            for f in t.get(key).and_then(Value::as_array).into_iter().flatten() {
                if let Some(field) = f.get("name").and_then(Value::as_str) {
                    out.insert(format!("{name}.{field}"));
                }
            }
        }
    }
    out
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The cached schema of a request's endpoint, if any (nothing is sent).
pub fn cached_schema(root: &Path, entry: &RequestEntry) -> Option<Schema> {
    let cached = read_cache(root, &entry.request.url)?;
    Some(Schema {
        introspection: cached.schema.to_string(),
        fetched_at: cached.fetched_at,
        cached: true,
        ..Default::default()
    })
}

/// Asks the endpoint of `entry` for its schema, compares it with the cached one and caches
/// the new one. Nothing else is saved: values set by scripts stay in memory.
pub async fn fetch_schema(
    root: &Path,
    entry: &RequestEntry,
    environment_id: Option<&str>,
    store: &dyn SecretStore,
) -> Result<Schema, String> {
    let mut probe = entry.clone();
    probe.request.graphql = Some(Graphql {
        query: INTROSPECTION_QUERY.into(),
        variables: String::new(),
        operation_name: Some("IntrospectionQuery".into()),
    });
    probe.request.body = None;
    probe.request.form = None;
    probe.request.body_file = None;
    probe.request.method = crate::HttpMethod::Post;
    // Tests of the request are about its own response, not this one.
    probe.scripts.test = String::new();
    let mut session = Session::open(root, environment_id, store, false)?.detached();
    let report = run::run_in(root, &probe, &mut session, &RunOptions::default()).await;
    let response = match (&report.response, &report.error) {
        (Some(response), _) => response,
        (None, Some(error)) => return Err(error.to_string()),
        (None, None) => return Err("no response".into()),
    };
    let body: Value = serde_json::from_str(&response.body).map_err(|_| {
        format!(
            "{} {}: the answer is not JSON, is this a GraphQL endpoint?",
            response.status, response.status_text
        )
    })?;
    let Some(schema) = body.pointer("/data/__schema").cloned() else {
        let reason = body
            .pointer("/errors/0/message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} {}", response.status, response.status_text));
        return Err(format!(
            "no schema in the answer: {reason} (is introspection turned off?)"
        ));
    };
    let schema = serde_json::json!({ "__schema": schema });
    let previous = read_cache(root, &entry.request.url);
    let (added, removed) = match &previous {
        Some(old) => {
            let (before, after) = (names_of(&old.schema), names_of(&schema));
            (
                after.difference(&before).cloned().collect(),
                before.difference(&after).cloned().collect(),
            )
        }
        None => (Vec::new(), Vec::new()),
    };
    let fetched_at = now();
    let file = cache_file(root, &entry.request.url).map_err(|e| e.to_string())?;
    let cached = Cached {
        url: entry.request.url.clone(),
        fetched_at,
        schema: schema.clone(),
    };
    fs::write(&file, serde_json::to_string(&cached).unwrap_or_default())
        .map_err(|e| io_error(&file, e).to_string())?;
    Ok(Schema {
        introspection: schema.to_string(),
        added,
        removed,
        compared: previous.is_some(),
        fetched_at,
        cached: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{init_workspace, Scripts};
    use crate::environment::MemoryStore;
    use crate::http::HttpRequest;
    use crate::{HttpMethod, Protocol};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Answers introspection with a schema that gains a field on the second call, and
    /// echoes any other body.
    async fn server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0u8; 16384];
                let mut read = 0;
                // Read until the whole body arrived (content-length).
                loop {
                    let n = socket.read(&mut buffer[read..]).await.unwrap_or(0);
                    read += n;
                    let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                    if n == 0 {
                        break;
                    }
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if read >= end + 4 + length {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                let reply = if body.contains("IntrospectionQuery") {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    let extra = if n == 0 { "" } else { r#",{"name":"email"}"# };
                    format!(
                        r#"{{"data":{{"__schema":{{"queryType":{{"name":"Query"}},"types":[{{"kind":"OBJECT","name":"Query","fields":[{{"name":"me"}}]}},{{"kind":"OBJECT","name":"User","fields":[{{"name":"id"}}{extra}]}},{{"kind":"OBJECT","name":"__Type","fields":[]}}]}}}}}}"#
                    )
                } else {
                    format!(r#"{{"echo":{}}}"#, serde_json::to_string(&body).unwrap())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        format!("http://{address}/graphql")
    }

    fn entry(url: &str) -> RequestEntry {
        let mut request = HttpRequest::new(HttpMethod::Post, url);
        request.graphql = Some(Graphql {
            query: "query Me($id: ID!) { user(id: $id) { id } }".into(),
            variables: "{\"id\": \"{{userId}}\"}".into(),
            operation_name: None,
        });
        RequestEntry {
            id: "01-me.pulpo.yaml".into(),
            name: "Me".into(),
            protocol: Protocol::Graphql,
            request,
            scripts: Scripts::default(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_caches_and_compares_the_schema() {
        let url = server().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Api").unwrap();
        let me = entry(&url);
        assert!(cached_schema(root, &me).is_none());
        let first = fetch_schema(root, &me, None, &MemoryStore::default())
            .await
            .unwrap();
        assert!(!first.compared);
        assert!(first.introspection.contains("\"__schema\""));
        assert!(cached_schema(root, &me).unwrap().cached);
        let second = fetch_schema(root, &me, None, &MemoryStore::default())
            .await
            .unwrap();
        assert!(second.compared);
        assert_eq!(second.added, vec!["User.email".to_string()]);
        assert!(second.removed.is_empty());
        // Introspection types are not part of the comparison.
        assert!(
            !names_of(&serde_json::from_str(&second.introspection).unwrap())
                .iter()
                .any(|n| n.starts_with("__"))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sends_the_operation_as_json_with_resolved_variables() {
        let url = server().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Api").unwrap();
        let me = entry(&url);
        let mut options = RunOptions::default();
        options.data.insert("userId".into(), "42".into());
        let mut session = Session::open(root, None, &MemoryStore::default(), false).unwrap();
        let report = run::run_in(root, &me, &mut session, &options).await;
        let echoed: Value = serde_json::from_str(&report.response.unwrap().body).unwrap();
        let sent: Value = serde_json::from_str(echoed["echo"].as_str().unwrap()).unwrap();
        assert_eq!(sent["query"], "query Me($id: ID!) { user(id: $id) { id } }");
        assert_eq!(sent["variables"]["id"], "42");
        assert!(sent.get("operationName").is_none());
    }

    #[test]
    fn saves_the_query_and_variables_as_their_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Api").unwrap();
        let mut me = entry("{{api}}/graphql");
        me.request.graphql.as_mut().unwrap().operation_name = Some("Me".into());
        crate::collection::save_request(root, &me).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("01-me.body.graphql")).unwrap(),
            "query Me($id: ID!) { user(id: $id) { id } }"
        );
        assert!(fs::read_to_string(root.join("01-me.variables.json"))
            .unwrap()
            .contains("{{userId}}"));
        let yaml = fs::read_to_string(root.join("01-me.pulpo.yaml")).unwrap();
        assert!(yaml.contains("protocol: \"graphql\""), "{yaml}");
        assert!(yaml.contains("method: \"POST\""), "{yaml}");
        assert!(yaml.contains("body: \"graphql\""), "{yaml}");
        assert!(yaml.contains("operationName: \"Me\""), "{yaml}");
        let back = crate::collection::load_request(root, "01-me.pulpo.yaml").unwrap();
        assert_eq!(back.request.graphql, me.request.graphql);
        assert_eq!(back.protocol, Protocol::Graphql);

        // Without variables the file goes away.
        let mut plain = back.clone();
        plain.request.graphql.as_mut().unwrap().variables = String::new();
        crate::collection::save_request(root, &plain).unwrap();
        assert!(!root.join("01-me.variables.json").exists());
    }

    #[test]
    fn imported_json_bodies_become_graphql() {
        let mut request = HttpRequest::new(HttpMethod::Post, "https://x/graphql");
        request.body = Some(
            r#"{"query":"query { me { id } }","variables":{"a":1},"operationName":"Me"}"#.into(),
        );
        assert!(request.promote_graphql());
        let g = request.graphql.as_ref().unwrap();
        assert_eq!(g.query, "query { me { id } }");
        assert!(g.variables.contains("\"a\": 1"));
        assert_eq!(g.operation_name.as_deref(), Some("Me"));
        assert!(request.body.is_none());
        let mut rest = HttpRequest::new(HttpMethod::Post, "https://x/search");
        rest.body = Some(r#"{"query":"shoes","page":2}"#.into());
        assert!(!rest.promote_graphql());
    }
}
