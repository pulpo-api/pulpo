//! A local mock server: the collection's saved examples (and the example responses of a
//! linked OpenAPI) answered as a fake API, so a front end works without its back end.
//!
//! Requests become routes by method and path (`GET /orders/:id` matches `/orders/7`).
//! The answer is the first 2xx example unless the caller asks for another with
//! `Prefer: code=404`, `Prefer: example=<name>` or `?__example=<name>`. CORS is open.

use crate::collection::{self, TreeNode};
use crate::http::Header;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// How often the routes are read again (edits apply without a restart).
const REFRESH: Duration = Duration::from_secs(1);

/// One response a route can give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockAnswer {
    pub name: String,
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: String,
}

/// A method and path the mock answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    pub method: String,
    /// `/orders/:id`.
    pub path: String,
    /// The request it comes from, or `OpenAPI · <file>`.
    pub name: String,
    /// `example` or `openapi`.
    pub source: String,
    pub answers: Vec<MockAnswer>,
}

/// A request the mock got, for the log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub method: String,
    pub path: String,
    pub status: u16,
    /// `Request name · example name`, empty when nothing matched.
    pub matched: String,
    pub at_ms: u64,
}

/// `/orders/:id` segments of a request URL: no server part, `{{base}}` stripped,
/// `:x`, `{x}` and `{{x}}` as parameters.
fn route_path(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default().trim();
    let path = if let Some(i) = url.find("://") {
        let rest = &url[i + 3..];
        rest.find('/').map(|j| &rest[j..]).unwrap_or("")
    } else {
        url
    };
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments
        .first()
        .is_some_and(|s| s.starts_with("{{") && s.ends_with("}}"))
        && url.starts_with("{{")
    {
        segments.remove(0);
    }
    let out: Vec<String> = segments
        .into_iter()
        .map(|s| {
            if let Some(name) = s.strip_prefix(':') {
                format!(":{name}")
            } else if s.starts_with("{{") && s.ends_with("}}") {
                format!(":{}", &s[2..s.len() - 2])
            } else if s.starts_with('{') && s.ends_with('}') {
                format!(":{}", &s[1..s.len() - 1])
            } else {
                s.to_string()
            }
        })
        .collect();
    format!("/{}", out.join("/"))
}

fn walk(
    nodes: &[TreeNode],
    folder: &str,
    inside: bool,
    out: &mut Vec<String>,
    folders: &mut Vec<String>,
) {
    for node in nodes {
        match node {
            TreeNode::Folder { id, children, .. } => {
                let now = inside || folder.is_empty() || id == folder;
                if now {
                    folders.push(id.clone());
                }
                walk(children, folder, now, out, folders);
            }
            TreeNode::Request(entry) => {
                if inside || folder.is_empty() {
                    out.push(entry.id.clone());
                }
            }
        }
    }
}

/// The routes of a folder (`""` = the whole workspace).
pub fn routes(root: &Path, folder_id: &str) -> Result<Vec<Route>, String> {
    let workspace = collection::load_workspace(root).map_err(|e| e.to_string())?;
    let (mut ids, mut folders) = (Vec::new(), vec![String::new()]);
    walk(&workspace.tree, folder_id, false, &mut ids, &mut folders);
    let mut out: Vec<Route> = Vec::new();
    for id in ids {
        let Ok(entry) = collection::load_request(root, &id) else {
            continue;
        };
        let examples = crate::examples::list_examples(root, &id).unwrap_or_default();
        if examples.is_empty() {
            continue;
        }
        out.push(Route {
            method: entry.request.method.to_string(),
            path: route_path(&entry.request.url),
            name: entry.name.clone(),
            source: "example".into(),
            answers: examples
                .into_iter()
                .map(|e| MockAnswer {
                    name: e.name,
                    status: e.status,
                    headers: e.headers,
                    body: e.body,
                })
                .collect(),
        });
    }
    // OpenAPI files linked to the folders fill in what has no example.
    for folder in folders
        .iter()
        .filter(|f| folder_id.is_empty() || f.starts_with(folder_id) || f.is_empty())
    {
        let Ok(meta) = collection::folder_meta(root, folder) else {
            continue;
        };
        let Some(source) = meta.source else { continue };
        let file: PathBuf = root.join(&source);
        let Ok(samples) = crate::import::openapi_responses(&file) else {
            continue;
        };
        for (method, path, sample) in samples {
            let path = route_path(&path);
            let method = method.to_uppercase();
            if out
                .iter()
                .any(|r| r.method == method && same_shape(&r.path, &path))
            {
                continue;
            }
            out.push(Route {
                method,
                path,
                name: format!("OpenAPI · {source}"),
                source: "openapi".into(),
                answers: vec![MockAnswer {
                    name: "example".into(),
                    status: 200,
                    headers: vec![Header::new("Content-Type", "application/json")],
                    body: serde_json::to_string_pretty(&sample).unwrap_or_default(),
                }],
            });
        }
    }
    out.sort_by(|a, b| {
        (literal_count(&b.path), &a.path, &a.method).cmp(&(
            literal_count(&a.path),
            &b.path,
            &b.method,
        ))
    });
    Ok(out)
}

fn literal_count(path: &str) -> usize {
    path.split('/')
        .filter(|s| !s.is_empty() && !s.starts_with(':'))
        .count()
}

fn same_shape(a: &str, b: &str) -> bool {
    let sa: Vec<&str> = a.split('/').collect();
    let sb: Vec<&str> = b.split('/').collect();
    sa.len() == sb.len()
        && sa
            .iter()
            .zip(&sb)
            .all(|(x, y)| x == y || (x.starts_with(':') && y.starts_with(':')))
}

fn matches(route: &str, path: &str) -> bool {
    let r: Vec<&str> = route.split('/').filter(|s| !s.is_empty()).collect();
    let p: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    r.len() == p.len() && r.iter().zip(&p).all(|(a, b)| a.starts_with(':') || a == b)
}

/// The route for a call: the one with most literal segments wins (`/orders/new` over
/// `/orders/:id`).
pub fn find<'a>(routes: &'a [Route], method: &str, path: &str) -> Option<&'a Route> {
    routes
        .iter()
        .filter(|r| r.method.eq_ignore_ascii_case(method) && matches(&r.path, path))
        .max_by_key(|r| literal_count(&r.path))
}

/// The answer a call asks for: `Prefer: example=<name>`, `Prefer: code=<status>`,
/// `?__example=<name>`, else the first 2xx, else the first.
pub fn choose<'a>(route: &'a Route, prefer: &str, query: &str) -> &'a MockAnswer {
    let wanted_name = prefer
        .split([',', ';'])
        .find_map(|p| p.trim().strip_prefix("example="))
        .map(|s| s.trim_matches('"').to_string())
        .or_else(|| {
            query
                .split('&')
                .find_map(|p| p.strip_prefix("__example="))
                .map(|s| s.replace("%20", " ").replace('+', " "))
        });
    if let Some(name) = wanted_name {
        if let Some(a) = route
            .answers
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(&name))
        {
            return a;
        }
    }
    if let Some(code) = prefer
        .split([',', ';'])
        .find_map(|p| p.trim().strip_prefix("code="))
        .and_then(|c| c.trim().parse::<u16>().ok())
    {
        if let Some(a) = route.answers.iter().find(|a| a.status == code) {
            return a;
        }
    }
    route
        .answers
        .iter()
        .find(|a| (200..300).contains(&a.status))
        .unwrap_or(&route.answers[0])
}

/// A running mock; dropping it (or [`Mock::stop`]) stops it.
pub struct Mock {
    pub address: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
}

impl Mock {
    pub fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop();
    }
}

type Shared = Arc<Mutex<(Instant, Vec<Route>)>>;

fn cors(builder: hyper::http::response::Builder) -> hyper::http::response::Builder {
    builder
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-headers", "*")
        .header("access-control-allow-methods", "*")
        .header("access-control-expose-headers", "*")
}

/// Header values are ASCII: anything else (accents in names) goes percent-encoded.
fn header_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c.to_string()
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf)
                    .bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect()
            }
        })
        .collect()
}

fn answer(
    routes: &[Route],
    method: &str,
    path: &str,
    query: &str,
    prefer: &str,
) -> (Response<Full<Bytes>>, Hit) {
    let mut hit = Hit {
        method: method.to_string(),
        path: if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        },
        ..Default::default()
    };
    if method == "OPTIONS" {
        hit.status = 204;
        let r = cors(Response::builder().status(204))
            .body(Full::new(Bytes::new()))
            .unwrap_or_default();
        return (r, hit);
    }
    let Some(route) = find(routes, method, path) else {
        hit.status = 404;
        let known: Vec<String> = routes
            .iter()
            .take(30)
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        let body =
            serde_json::json!({ "error": format!("no mock for {method} {path}"), "routes": known });
        let r = cors(
            Response::builder()
                .status(404)
                .header("content-type", "application/json"),
        )
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap_or_default();
        return (r, hit);
    };
    let a = choose(route, prefer, query);
    hit.status = a.status;
    hit.matched = format!("{} · {}", route.name, a.name);
    let mut b = cors(Response::builder().status(a.status)).header(
        "x-pulpo-mock",
        header_text(&format!("{} / {}", route.name, a.name)),
    );
    for h in a.headers.iter().filter(|h| h.enabled) {
        let n = h.name.to_ascii_lowercase();
        if ["content-length", "transfer-encoding", "connection"].contains(&n.as_str())
            || n.starts_with("access-control-")
        {
            continue;
        }
        b = b.header(h.name.as_str(), h.value.as_str());
    }
    let r = b
        .body(Full::new(Bytes::from(a.body.clone())))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(500)
                .body(Full::new(Bytes::from("bad example headers")))
                .unwrap_or_default()
        });
    (r, hit)
}

/// Starts the mock on `127.0.0.1:port` (0 = any free port); every call goes to `on_hit`.
pub async fn start(
    root: &Path,
    folder_id: &str,
    port: u16,
    on_hit: impl Fn(Hit) + Send + Sync + 'static,
) -> Result<Mock, String> {
    let first = routes(root, folder_id)?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("port {port}: {e}"))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let shared: Shared = Arc::new(Mutex::new((Instant::now(), first)));
    let (root, folder) = (root.to_path_buf(), folder_id.to_string());
    let on_hit = Arc::new(on_hit);
    let started = Instant::now();
    let (tx, mut rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = &mut rx => break,
                a = listener.accept() => a,
            };
            let Ok((stream, _)) = accepted else { continue };
            let (shared, root, folder, on_hit) =
                (shared.clone(), root.clone(), folder.clone(), on_hit.clone());
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                    let (shared, root, folder, on_hit) =
                        (shared.clone(), root.clone(), folder.clone(), on_hit.clone());
                    async move {
                        let method = req.method().as_str().to_uppercase();
                        let path = req.uri().path().to_string();
                        let query = req.uri().query().unwrap_or("").to_string();
                        let prefer = req
                            .headers()
                            .get("prefer")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let _ = req.into_body().collect().await;
                        let current = {
                            let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
                            if guard.0.elapsed() > REFRESH {
                                if let Ok(fresh) = routes(&root, &folder) {
                                    *guard = (Instant::now(), fresh);
                                }
                            }
                            guard.1.clone()
                        };
                        let (response, mut hit) = answer(&current, &method, &path, &query, &prefer);
                        hit.at_ms = started.elapsed().as_millis() as u64;
                        on_hit(hit);
                        Ok::<_, Infallible>(response)
                    }
                });
                let io = hyper_util::rt::TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    });
    Ok(Mock {
        address,
        stop: Some(tx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{init_workspace, save_request, RequestEntry, Scripts};
    use crate::examples::{save_example, Example};
    use crate::http::HttpRequest;
    use crate::HttpMethod;

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

    fn example(root: &Path, id: &str, name: &str, status: u16, body: &str) {
        save_example(
            root,
            id,
            &Example {
                id: String::new(),
                name: name.into(),
                status,
                headers: vec![Header::new("Content-Type", "application/json")],
                body: body.into(),
            },
        )
        .unwrap();
    }

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Shop").unwrap();
        request(
            root,
            "01-order.pulpo.yaml",
            "Get order",
            HttpMethod::Get,
            "{{base}}/orders/:id",
        );
        example(
            root,
            "01-order.pulpo.yaml",
            "Found",
            200,
            r#"{"id": 7, "status": "PAID"}"#,
        );
        example(
            root,
            "01-order.pulpo.yaml",
            "Missing",
            404,
            r#"{"error": "no order"}"#,
        );
        request(
            root,
            "02-new.pulpo.yaml",
            "New order form",
            HttpMethod::Get,
            "https://api.example.com/orders/new",
        );
        example(root, "02-new.pulpo.yaml", "Form", 200, r#"{"fields": []}"#);
        request(
            root,
            "03-no-examples.pulpo.yaml",
            "Untested",
            HttpMethod::Get,
            "{{base}}/health",
        );
        dir
    }

    #[test]
    fn turns_examples_into_routes_and_picks_answers() {
        let dir = workspace();
        let all = routes(dir.path(), "").unwrap();
        let paths: Vec<String> = all
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        assert_eq!(paths, ["GET /orders/new", "GET /orders/:id"]);
        let order = find(&all, "GET", "/orders/42").unwrap();
        assert_eq!(order.name, "Get order");
        assert_eq!(
            find(&all, "GET", "/orders/new").unwrap().name,
            "New order form"
        );
        assert!(find(&all, "POST", "/orders/42").is_none());
        assert_eq!(choose(order, "", "").name, "Found");
        assert_eq!(choose(order, "code=404", "").name, "Missing");
        assert_eq!(choose(order, "example=missing", "").status, 404);
        assert_eq!(choose(order, "", "a=1&__example=Missing").status, 404);
        assert_eq!(
            route_path("{{base}}/users/{{userId}}/posts/{id}?x=1"),
            "/users/:userId/posts/:id"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn serves_examples_over_http() {
        let dir = workspace();
        let hits = Arc::new(Mutex::new(Vec::<Hit>::new()));
        let log = hits.clone();
        let mut mock = start(dir.path(), "", 0, move |h| log.lock().unwrap().push(h))
            .await
            .unwrap();
        let base = format!("http://{}", mock.address);
        let client = reqwest::Client::new();
        let ok = client.get(format!("{base}/orders/7")).send().await.unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.headers()["access-control-allow-origin"], "*");
        assert_eq!(ok.headers()["x-pulpo-mock"], "Get order / Found");
        assert_eq!(header_text("País"), "Pa%C3%ADs");
        assert!(ok.text().await.unwrap().contains("PAID"));
        let missing = client
            .get(format!("{base}/orders/7"))
            .header("Prefer", "code=404")
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);
        let none = client.get(format!("{base}/nothing")).send().await.unwrap();
        assert_eq!(none.status(), 404);
        assert!(none
            .text()
            .await
            .unwrap()
            .contains("no mock for GET /nothing"));
        let preflight = client
            .request(reqwest::Method::OPTIONS, format!("{base}/orders/7"))
            .send()
            .await
            .unwrap();
        assert_eq!(preflight.status(), 204);
        // Edits apply without a restart.
        example(
            dir.path(),
            "03-no-examples.pulpo.yaml",
            "Up",
            200,
            r#"{"up": true}"#,
        );
        tokio::time::sleep(REFRESH + Duration::from_millis(100)).await;
        let _ = client.get(format!("{base}/health")).send().await.unwrap();
        let health = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(health.status(), 200);
        mock.stop();
        let seen: Vec<String> = hits
            .lock()
            .unwrap()
            .iter()
            .map(|h| format!("{} {} {}", h.method, h.path, h.status))
            .collect();
        assert_eq!(seen[0], "GET /orders/7 200");
        assert_eq!(seen[2], "GET /nothing 404");
    }

    #[test]
    fn fills_in_from_a_linked_openapi() {
        let dir = workspace();
        let root = dir.path();
        std::fs::write(
            root.join("api.yaml"),
            "openapi: 3.0.0\ninfo: { title: Shop, version: '1' }\npaths:\n  /orders/{id}:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              example: { id: 1 }\n  /stores:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              example: [{ name: Centro }]\n",
        )
        .unwrap();
        collection::set_folder_source(root, "", Some("api.yaml")).unwrap();
        let all = routes(root, "").unwrap();
        let stores = find(&all, "GET", "/stores").unwrap();
        assert_eq!(stores.source, "openapi");
        assert!(stores.answers[0].body.contains("Centro"));
        // The saved example wins over the OpenAPI one for the same route.
        assert_eq!(find(&all, "GET", "/orders/3").unwrap().source, "example");
    }
}
