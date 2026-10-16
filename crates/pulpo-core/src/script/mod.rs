//! Pre-request and test scripts, run in a sandboxed QuickJS runtime.
//!
//! The `pm` API lives in `prelude.js`. Rust only passes the state in as JSON and reads
//! the result back as JSON, so the binding surface stays tiny.

use crate::http::{HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("prelude.js");
const TIME_LIMIT: Duration = Duration::from_secs(5);
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

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

    let runtime = rquickjs::Runtime::new().map_err(|e| format!("script engine: {e}"))?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    let started = Instant::now();
    runtime.set_interrupt_handler(Some(Box::new(move || started.elapsed() > TIME_LIMIT)));
    let context = rquickjs::Context::full(&runtime).map_err(|e| format!("script engine: {e}"))?;
    let result: Result<String, String> = context.with(|ctx| {
        ctx.eval::<String, _>(program.as_str()).map_err(|e| {
            if started.elapsed() > TIME_LIMIT {
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
                source: "pm.sendRequest('x', () => {});".into(),
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
        assert!(outcome.errors[0].starts_with("_folder.pre.js: pm.sendRequest is not supported"));
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
