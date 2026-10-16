//! Pre-request and test scripts, run in a sandboxed QuickJS runtime.
//!
//! The `pm` API lives in `prelude.js`. Rust only passes the state in as JSON and reads
//! the result back as JSON, so the binding surface stays tiny.

mod crypto;

use crate::cookies::SharedJar;
use crate::http::{self, HttpRequest, HttpResponse, SendContext};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("prelude.js");
/// Script time, not counting the time spent waiting for `pm.sendRequest`.
const TIME_LIMIT: Duration = Duration::from_secs(5);
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// Most `pm.sendRequest` calls allowed in one phase.
const MAX_SUB_REQUESTS: usize = 50;
/// Timeout of a `pm.sendRequest` call when the script does not set one.
const SUB_REQUEST_TIMEOUT_MS: u64 = 15_000;

/// Sends a request for `pm.sendRequest`. Scripts run synchronously, so the request runs
/// on its own thread and runtime; this works whether or not the caller is async.
fn send_blocking(request_json: String, cookies: Option<Arc<SharedJar>>) -> String {
    let outcome = std::thread::spawn(move || {
        let mut request: HttpRequest = match serde_json::from_str(&request_json) {
            Ok(request) => request,
            Err(e) => {
                return serde_json::json!({ "ok": false, "error": format!("bad request: {e}") })
            }
        };
        if request.timeout_ms.is_none() {
            request.timeout_ms = Some(SUB_REQUEST_TIMEOUT_MS);
        }
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => return serde_json::json!({ "ok": false, "error": e.to_string() }),
        };
        let context = SendContext {
            root: None,
            cookies,
        };
        match runtime.block_on(http::send_with(&request, &context)) {
            Ok(response) => serde_json::json!({ "ok": true, "response": response }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    })
    .join()
    .unwrap_or_else(|_| serde_json::json!({ "ok": false, "error": "request thread panicked" }));
    outcome.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    PreRequest,
    Test,
}

/// One script to run, with a label used in errors and logs (e.g. `01-orders/_folder.test.js`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub label: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub error: Option<String>,
}

/// What the scripts of one phase did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptOutcome {
    /// Variables set (`Some`) or unset (`None`) by the scripts.
    pub changes: HashMap<String, Option<String>>,
    pub tests: Vec<TestResult>,
    pub logs: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Input<'a> {
    phase: Phase,
    request_name: &'a str,
    environment_name: &'a str,
    env: &'a HashMap<String, String>,
    local: &'a HashMap<String, String>,
    request: &'a HttpRequest,
    response: Option<&'a HttpResponse>,
    /// Cookies of the jar that match the request URL, for `pm.cookies`.
    cookies: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct Output {
    changes: HashMap<String, Option<String>>,
    request: HttpRequest,
    tests: Vec<TestResult>,
    logs: Vec<String>,
    errors: Vec<String>,
}

/// Everything a phase needs to see.
pub struct ScriptContext<'a> {
    pub request_name: &'a str,
    pub environment_name: &'a str,
    /// Environment values, secrets included.
    pub env: &'a HashMap<String, String>,
    /// Local values set by earlier scripts; they win over `env`.
    pub local: &'a HashMap<String, String>,
    /// The environment's cookie jar, shared with `pm.sendRequest`.
    pub cookies: Option<Arc<SharedJar>>,
}

/// Runs the scripts of a phase in order. Pre-request scripts may change the request,
/// which is returned updated. A failing script is reported and the next one still runs.
pub fn run_phase(
    phase: Phase,
    scripts: &[Script],
    context: &ScriptContext<'_>,
    request: &HttpRequest,
    response: Option<&HttpResponse>,
) -> (HttpRequest, ScriptOutcome) {
    if scripts.iter().all(|s| s.source.trim().is_empty()) {
        return (request.clone(), ScriptOutcome::default());
    }
    match execute(phase, scripts, context, request, response) {
        Ok(output) => (
            if phase == Phase::PreRequest {
                output.request
            } else {
                request.clone()
            },
            ScriptOutcome {
                changes: output.changes,
                tests: output.tests,
                logs: output.logs,
                errors: output.errors,
            },
        ),
        Err(error) => (
            request.clone(),
            ScriptOutcome {
                errors: vec![error],
                ..Default::default()
            },
        ),
    }
}

fn execute(
    phase: Phase,
    scripts: &[Script],
    context: &ScriptContext<'_>,
    request: &HttpRequest,
    response: Option<&HttpResponse>,
) -> Result<Output, String> {
    let input = Input {
        phase,
        request_name: context.request_name,
        environment_name: context.environment_name,
        env: context.env,
        local: context.local,
        request,
        response,
        cookies: context
            .cookies
            .as_ref()
            .map(|jar| jar.pairs_for(&request.url))
            .unwrap_or_default(),
    };
    let mut program = format!(
        "var __input = {};\n{PRELUDE}\n",
        serde_json::to_string(&input).map_err(|e| e.to_string())?
    );
    for script in scripts.iter().filter(|s| !s.source.trim().is_empty()) {
        program.push_str(&format!(
            "__run({}, {});\n",
            serde_json::to_string(&script.label).map_err(|e| e.to_string())?,
            serde_json::to_string(&script.source).map_err(|e| e.to_string())?
        ));
    }
    program.push_str("__finish();\n");

    let script_cookies = context.cookies.clone();
    let runtime = rquickjs::Runtime::new().map_err(|e| format!("script engine: {e}"))?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    let started = Instant::now();
    // Time spent inside pm.sendRequest does not count against the script limit.
    let waited_ms = Arc::new(AtomicU64::new(0));
    let script_time = {
        let waited_ms = waited_ms.clone();
        move || {
            started
                .elapsed()
                .saturating_sub(Duration::from_millis(waited_ms.load(Ordering::Relaxed)))
        }
    };
    let over_limit = {
        let script_time = script_time.clone();
        move || script_time() > TIME_LIMIT
    };
    runtime.set_interrupt_handler(Some(Box::new(over_limit.clone())));
    let context = rquickjs::Context::full(&runtime).map_err(|e| format!("script engine: {e}"))?;
    let sub_requests = Arc::new(AtomicUsize::new(0));
    let result: Result<String, String> = context.with(|ctx| {
        let globals = ctx.globals();
        let http_waited = waited_ms.clone();
        let http_count = sub_requests.clone();
        let http_cookies = script_cookies.clone();
        let http_bridge = rquickjs::Function::new(ctx.clone(), move |request_json: String| -> String {
            if http_count.fetch_add(1, Ordering::Relaxed) >= MAX_SUB_REQUESTS {
                return serde_json::json!({
                    "ok": false,
                    "error": format!("more than {MAX_SUB_REQUESTS} pm.sendRequest calls in one script phase")
                })
                .to_string();
            }
            let t = Instant::now();
            let out = send_blocking(request_json, http_cookies.clone());
            http_waited.fetch_add(t.elapsed().as_millis() as u64, Ordering::Relaxed);
            out
        })
        .map_err(|e| format!("script engine: {e}"))?;
        globals.set("__pulpo_http", http_bridge).map_err(|e| format!("script engine: {e}"))?;
        let crypto_bridge = rquickjs::Function::new(ctx.clone(), |algorithm: String, key_hex: String, data_hex: String| -> String {
            crypto::digest(&algorithm, &key_hex, &data_hex).unwrap_or_else(|e| format!("error:{e}"))
        })
        .map_err(|e| format!("script engine: {e}"))?;
        globals.set("__pulpo_crypto", crypto_bridge).map_err(|e| format!("script engine: {e}"))?;

        ctx.eval::<String, _>(program.as_str()).map_err(|e| {
            if over_limit() {
                format!(
                    "scripts stopped after {} s (infinite loop?)",
                    TIME_LIMIT.as_secs()
                )
            } else {
                let detail = ctx.catch();
                match detail.as_exception() {
                    Some(ex) => format!("script engine: {}", ex.message().unwrap_or_default()),
                    None => format!("script engine: {e}"),
                }
            }
        })
    });
    serde_json::from_str(&result?).map_err(|e| format!("script engine: bad result: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Header;
    use crate::HttpMethod;

    fn context<'a>(
        env: &'a HashMap<String, String>,
        local: &'a HashMap<String, String>,
    ) -> ScriptContext<'a> {
        ScriptContext {
            request_name: "Create order",
            environment_name: "Local",
            env,
            local,
            cookies: None,
        }
    }

    fn script(source: &str) -> Vec<Script> {
        vec![Script {
            label: "test.js".into(),
            source: source.into(),
        }]
    }

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            status_text: "Created".into(),
            http_version: "HTTP/1.1".into(),
            headers: vec![
                Header::new("ETag", "\"3\""),
                Header::new("content-type", "application/json"),
            ],
            body: body.into(),
            body_is_text: true,
            size_bytes: body.len(),
            duration_ms: 42,
        }
    }

    #[test]
    fn pre_request_scripts_change_the_request_and_set_variables() {
        let env = HashMap::from([("base".to_string(), "https://api".to_string())]);
        let local = HashMap::new();
        let mut request = HttpRequest::new(HttpMethod::Post, "{{base}}/orders");
        request.body = Some("{\"n\": 1}".into());
        let (request, outcome) = run_phase(
            Phase::PreRequest,
            &script(
                "pm.environment.set('key', 'k-' + pm.variables.get('base'));
                 pm.request.headers.upsert({ key: 'X-Key', value: pm.variables.get('key') });
                 pm.request.body.raw = pm.variables.replaceIn('{\"base\": \"{{base}}\", \"id\": \"{{$guid}}\"}');
                 console.log('ready', { n: 1 });",
            ),
            &context(&env, &local),
            &request,
            None,
        );
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.changes["key"].as_deref(), Some("k-https://api"));
        assert_eq!(request.headers[0], Header::new("X-Key", "k-https://api"));
        let body: serde_json::Value =
            serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["base"], "https://api");
        assert_eq!(body["id"].as_str().unwrap().len(), 36);
        assert_eq!(outcome.logs, ["[test.js] ready {\"n\":1}"]);
    }

    #[test]
    fn test_scripts_read_the_response_and_collect_results() {
        let env = HashMap::new();
        let local = HashMap::from([("old".to_string(), "x".to_string())]);
        let request = HttpRequest::new(HttpMethod::Post, "https://api/orders");
        let (_, outcome) = run_phase(
            Phase::Test,
            &script(
                "pm.test('created', () => pm.response.to.have.status(201));
                 pm.test('fast', () => pm.expect(pm.response.responseTime).to.be.below(1000));
                 const j = pm.response.json();
                 pm.test('has id', () => pm.expect(j).to.have.property('orderId'));
                 pm.test('wrong', () => pm.expect(j.items).to.have.lengthOf(3));
                 pm.test('etag', () => pm.expect(pm.response.headers.get('etag')).to.eql('\"3\"'));
                 pm.test('one of', () => pm.expect(pm.response.code).to.be.oneOf([200, 201]));
                 pm.test('not', () => pm.expect(j.status).to.not.equal('FAILED'));
                 pm.environment.set('orderId', j.orderId);
                 pm.environment.unset('old');",
            ),
            &context(&env, &local),
            &request,
            Some(&response(
                201,
                r#"{"orderId": "o-1", "status": "CREATED", "items": [1, 2]}"#,
            )),
        );
        let summary: Vec<_> = outcome
            .tests
            .iter()
            .map(|t| (t.name.as_str(), t.passed))
            .collect();
        assert_eq!(
            summary,
            [
                ("created", true),
                ("fast", true),
                ("has id", true),
                ("wrong", false),
                ("etag", true),
                ("one of", true),
                ("not", true)
            ]
        );
        assert!(outcome.tests[3]
            .error
            .as_deref()
            .unwrap()
            .contains("length 2 to be 3"));
        assert_eq!(outcome.changes["orderId"].as_deref(), Some("o-1"));
        assert_eq!(outcome.changes["old"], None);
    }

    #[test]
    fn a_failing_script_does_not_stop_the_next_one() {
        let env = HashMap::new();
        let local = HashMap::new();
        let scripts = vec![
            Script {
                label: "_folder.pre.js".into(),
                source: "notDefinedAnywhere();".into(),
            },
            Script {
                label: "broken.pre.js".into(),
                source: "this is not javascript".into(),
            },
            Script {
                label: "request.pre.js".into(),
                source: "pm.variables.set('ran', true);".into(),
            },
        ];
        let (_, outcome) = run_phase(
            Phase::PreRequest,
            &scripts,
            &context(&env, &local),
            &HttpRequest::new(HttpMethod::Get, "x"),
            None,
        );
        assert_eq!(outcome.errors.len(), 2, "{:?}", outcome.errors);
        assert!(
            outcome.errors[0].starts_with("_folder.pre.js: "),
            "{:?}",
            outcome.errors
        );
        assert!(outcome.errors[1].starts_with("broken.pre.js:"));
        assert_eq!(outcome.changes["ran"].as_deref(), Some("true"));
    }

    #[test]
    fn infinite_loops_are_stopped() {
        let env = HashMap::new();
        let local = HashMap::new();
        let started = Instant::now();
        let (_, outcome) = run_phase(
            Phase::PreRequest,
            &script("while (true) {}"),
            &context(&env, &local),
            &HttpRequest::new(HttpMethod::Get, "x"),
            None,
        );
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(outcome.errors.len(), 1);
    }

    /// A tiny blocking HTTP server on its own thread, so it keeps answering while the
    /// (synchronous) script waits for it.
    fn blocking_server(count: usize) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..count {
                let (mut socket, _) = listener.accept().unwrap();
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).unwrap();
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                let line = text.lines().next().unwrap_or("").to_string();
                let body_in = text
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();
                let body =
                    serde_json::json!({ "line": line, "body": body_in, "access_token": "tok-1" })
                        .to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).unwrap();
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn send_request_works_like_postman() {
        let base = blocking_server(2);
        let env = HashMap::from([("base".to_string(), base)]);
        let local = HashMap::new();
        let (request, outcome) = run_phase(
            Phase::PreRequest,
            &script(
                "pm.sendRequest({
                   url: pm.variables.get('base') + '/token', method: 'POST',
                   header: { 'Content-Type': 'application/x-www-form-urlencoded' },
                   body: { mode: 'urlencoded', urlencoded: [{ key: 'grant_type', value: 'password' }, { key: 'user', value: 'ana' }] }
                 }, (err, res) => {
                   if (err) throw err;
                   pm.environment.set('token', res.json().access_token);
                   pm.environment.set('sent', res.json().body);
                   pm.environment.set('code', res.code);
                 });
                 const again = pm.sendRequest('{{base}}/ping');
                 pm.request.headers.upsert({ key: 'Authorization', value: 'Bearer ' + pm.environment.get('token') });
                 pm.environment.set('line', again.json().line);",
            ),
            &context(&env, &local),
            &HttpRequest::new(HttpMethod::Get, "x"),
            None,
        );
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.changes["token"].as_deref(), Some("tok-1"));
        assert_eq!(
            outcome.changes["sent"].as_deref(),
            Some("grant_type=password&user=ana")
        );
        assert_eq!(outcome.changes["code"].as_deref(), Some("200"));
        assert_eq!(
            outcome.changes["line"].as_deref(),
            Some("GET /ping HTTP/1.1")
        );
        assert_eq!(
            request.headers[0],
            Header::new("Authorization", "Bearer tok-1")
        );
    }

    #[test]
    fn send_request_reports_errors_to_the_callback() {
        let env = HashMap::new();
        let local = HashMap::new();
        let (_, outcome) = run_phase(
            Phase::PreRequest,
            &script("pm.sendRequest('http://127.0.0.1:1/x', (err, res) => { console.log(err); pm.environment.set('err', String(!!err) + '|' + String(res)); });"),
            &context(&env, &local),
            &HttpRequest::new(HttpMethod::Get, "x"),
            None,
        );
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.changes["err"].as_deref(), Some("true|undefined"));
        assert!(
            outcome.logs[0].starts_with("[test.js] Error: could not connect"),
            "{:?}",
            outcome.logs
        );
    }

    #[test]
    fn crypto_js_matches_known_digests() {
        let env = HashMap::new();
        let local = HashMap::new();
        let (_, outcome) = run_phase(
            Phase::PreRequest,
            &script(
                "pm.variables.set('sha256', CryptoJS.SHA256('abc').toString());
                 pm.variables.set('md5', CryptoJS.MD5('abc').toString());
                 pm.variables.set('sha1', CryptoJS.SHA1('abc').toString(CryptoJS.enc.Hex));
                 pm.variables.set('hmac', CryptoJS.HmacSHA256('The quick brown fox jumps over the lazy dog', 'key').toString());
                 pm.variables.set('hmac64', CryptoJS.HmacSHA256('ñ', 'key').toString(CryptoJS.enc.Base64));
                 pm.variables.set('utf8', CryptoJS.enc.Base64.stringify(CryptoJS.enc.Utf8.parse('ñandú')));
                 pm.variables.set('back', CryptoJS.enc.Base64.parse('w7FhbmTDug==').toString(CryptoJS.enc.Utf8));
                 pm.variables.set('hexkey', CryptoJS.HmacSHA256('x', CryptoJS.enc.Hex.parse('6b6579')).toString());",
            ),
            &context(&env, &local),
            &HttpRequest::new(HttpMethod::Get, "x"),
            None,
        );
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let get = |k: &str| outcome.changes[k].clone().unwrap();
        assert_eq!(
            get("sha256"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(get("md5"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(get("sha1"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            get("hmac"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        assert_eq!(get("utf8"), "w7FhbmTDug==");
        assert_eq!(get("back"), "ñandú");
        let direct = outcome.changes["hexkey"].clone().unwrap();
        assert_eq!(direct.len(), 64);
        assert_eq!(get("hmac64").len(), 44);
    }

    #[test]
    fn local_values_win_over_the_environment() {
        let env = HashMap::from([("token".to_string(), "from-env".to_string())]);
        let local = HashMap::from([("token".to_string(), "from-script".to_string())]);
        let (_, outcome) = run_phase(
            Phase::PreRequest,
            &script("pm.variables.set('seen', pm.environment.get('token') + '|' + atob(btoa('ok')) + '|' + pm.info.requestName);"),
            &context(&env, &local),
            &HttpRequest::new(HttpMethod::Get, "x"),
            None,
        );
        assert_eq!(
            outcome.changes["seen"].as_deref(),
            Some("from-script|ok|Create order")
        );
    }

    #[test]
    fn empty_scripts_skip_the_engine() {
        let env = HashMap::new();
        let local = HashMap::new();
        let request = HttpRequest::new(HttpMethod::Get, "x");
        let (same, outcome) = run_phase(
            Phase::Test,
            &script("  \n"),
            &context(&env, &local),
            &request,
            None,
        );
        assert_eq!(same, request);
        assert_eq!(outcome, ScriptOutcome::default());
    }
}
