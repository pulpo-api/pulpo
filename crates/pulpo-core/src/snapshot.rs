//! Snapshot tests: `pm.response.to.matchSnapshot()` keeps the response (status and body)
//! in `<request>.snapshot.json` the first time and fails when a later one differs.
//! Fields that change on every call (dates, ids) are left out with JSONPath `ignore`s.

use crate::collection::{io_error, request_path, stem_of, CollectionError};
use crate::http::HttpResponse;
use crate::script::TestResult;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use serde_json_path::JsonPath;
use std::fs;
use std::path::{Path, PathBuf};

pub const SNAPSHOT_SUFFIX: &str = ".snapshot.json";
const FORMAT_VERSION: u64 = 1;
/// Placeholder written instead of an ignored value.
const IGNORED: &str = "<ignored>";
/// At most this many differences are listed in a failure.
const MAX_DIFFERENCES: usize = 5;

/// A `matchSnapshot` call made by a test script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCheck {
    pub name: String,
    #[serde(default)]
    pub ignore: Vec<String>,
}

/// The snapshot file of a request file.
pub fn snapshot_file(request_file: &Path) -> PathBuf {
    request_file.with_file_name(format!("{}{SNAPSHOT_SUFFIX}", stem_of(request_file)))
}

/// What a snapshot keeps of a response, with the ignored fields replaced.
pub fn capture(response: &HttpResponse, ignore: &[String]) -> Result<Value, String> {
    let mut body = match serde_json::from_str::<Value>(&response.body) {
        Ok(value) => value,
        Err(_) => Value::String(response.body.clone()),
    };
    for expression in ignore {
        let path = JsonPath::parse(expression).map_err(|e| format!("ignore {expression}: {e}"))?;
        let pointers: Vec<String> = path
            .query_located(&body)
            .locations()
            .map(|location| location.to_json_pointer())
            .collect();
        for pointer in pointers {
            if let Some(slot) = body.pointer_mut(&pointer) {
                *slot = Value::String(IGNORED.into());
            }
        }
    }
    Ok(json!({ "status": response.status, "body": body }))
}

fn read(file: &Path) -> Result<Map<String, Value>, CollectionError> {
    if !file.exists() {
        return Ok(Map::new());
    }
    let text = fs::read_to_string(file).map_err(|e| io_error(file, e))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| CollectionError::InvalidFile(format!("{}: {e}", file.display())))?;
    if value.get("version").and_then(Value::as_u64) != Some(FORMAT_VERSION) {
        return Err(CollectionError::InvalidFile(format!(
            "{}: unsupported version",
            file.display()
        )));
    }
    Ok(value
        .get("snapshots")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

fn write(file: &Path, snapshots: Map<String, Value>) -> Result<(), CollectionError> {
    let text = serde_json::to_string_pretty(&json!({
        "version": FORMAT_VERSION,
        "snapshots": snapshots,
    }))
    .map_err(|e| CollectionError::InvalidFile(e.to_string()))?;
    fs::write(file, text + "\n").map_err(|e| io_error(file, e))
}

/// Compares the response with the request's snapshots, saving new ones (or all of them
/// with `update`). One test result per check.
pub fn check(
    root: &Path,
    request_id: &str,
    response: &HttpResponse,
    checks: &[SnapshotCheck],
    update: bool,
) -> Vec<TestResult> {
    let result = |name: &str, error: Option<String>| TestResult {
        name: format!("Snapshot \"{name}\""),
        passed: error.is_none(),
        error,
    };
    let file = match request_path(root, request_id) {
        Ok(path) => snapshot_file(&path),
        Err(e) => {
            return checks
                .iter()
                .map(|c| result(&c.name, Some(e.to_string())))
                .collect()
        }
    };
    let mut snapshots = match read(&file) {
        Ok(snapshots) => snapshots,
        Err(e) => {
            return checks
                .iter()
                .map(|c| result(&c.name, Some(e.to_string())))
                .collect()
        }
    };
    let mut changed = false;
    let mut out = Vec::new();
    for check in checks {
        let current = match capture(response, &check.ignore) {
            Ok(value) => value,
            Err(e) => {
                out.push(result(&check.name, Some(e)));
                continue;
            }
        };
        match snapshots.get(&check.name) {
            Some(saved) if !update => {
                let mut differences = Vec::new();
                diff("$", saved, &current, &mut differences);
                if differences.is_empty() {
                    out.push(result(&check.name, None));
                } else {
                    let more = differences.len().saturating_sub(MAX_DIFFERENCES);
                    differences.truncate(MAX_DIFFERENCES);
                    let mut message = format!("changed: {}", differences.join("; "));
                    if more > 0 {
                        message.push_str(&format!(" (and {more} more)"));
                    }
                    out.push(result(&check.name, Some(message)));
                }
            }
            existing => {
                let existed = existing.is_some();
                if existing != Some(&current) {
                    snapshots.insert(check.name.clone(), current);
                    changed = true;
                }
                out.push(TestResult {
                    name: format!(
                        "Snapshot \"{}\" {}",
                        check.name,
                        if existed { "updated" } else { "saved" }
                    ),
                    passed: true,
                    error: None,
                });
            }
        }
    }
    if changed {
        if let Err(e) = write(&file, snapshots) {
            out.push(TestResult {
                name: "Snapshot file".into(),
                passed: false,
                error: Some(e.to_string()),
            });
        }
    }
    out
}

fn short(value: &Value) -> String {
    let text = match value {
        Value::Object(_) => "{…}".to_string(),
        Value::Array(items) => format!("[{} items]", items.len()),
        other => other.to_string(),
    };
    if text.chars().count() > 40 {
        format!("{}…", text.chars().take(40).collect::<String>())
    } else {
        text
    }
}

/// Paths where `after` differs from `before`, as `$.a[0].b: 1 → 2`.
fn diff(path: &str, before: &Value, after: &Value, out: &mut Vec<String>) {
    match (before, after) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let child = format!("{path}.{key}");
                match b.get(key) {
                    Some(other) => diff(&child, value, other, out),
                    None => out.push(format!("{child} removed")),
                }
            }
            for key in b.keys().filter(|k| !a.contains_key(*k)) {
                out.push(format!("{path}.{key} added"));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}[{i}]"), x, y, out);
            }
            if a.len() != b.len() {
                out.push(format!("{path}: {} items → {}", a.len(), b.len()));
            }
        }
        (a, b) if a != b => out.push(format!("{path}: {} → {}", short(a), short(b))),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{create_request, init_workspace};
    use crate::http::Header;

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            status_text: String::new(),
            http_version: "HTTP/1.1".into(),
            headers: vec![Header::new("content-type", "application/json")],
            body: body.into(),
            body_is_text: true,
            body_base64: None,
            size_bytes: body.len(),
            duration_ms: 1,
            url: String::new(),
            redirects: Vec::new(),
            timings: None,
            sent: None,
        }
    }

    #[test]
    fn saves_then_compares_ignoring_volatile_fields() {
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let entry = create_request(ws.path(), "", "Get order").unwrap();
        let check = SnapshotCheck {
            name: "order".into(),
            ignore: vec!["$.updatedAt".into(), "$.items[*].id".into()],
        };
        let run = |body: &str, update: bool| {
            check_one(ws.path(), &entry.id, &response(200, body), &check, update)
        };

        let first = run(
            r#"{"id":1,"updatedAt":"t1","items":[{"id":"a","qty":2}]}"#,
            false,
        );
        assert_eq!(first.name, "Snapshot \"order\" saved");
        let file = ws.path().join("01-get-order.snapshot.json");
        assert!(fs::read_to_string(&file).unwrap().contains("\"<ignored>\""));

        let same = run(
            r#"{"id":1,"updatedAt":"t2","items":[{"id":"b","qty":2}]}"#,
            false,
        );
        assert!(same.passed, "{same:?}");

        let changed = run(
            r#"{"id":1,"updatedAt":"t3","items":[{"id":"c","qty":3}],"x":true}"#,
            false,
        );
        assert!(!changed.passed);
        assert_eq!(
            changed.error.as_deref(),
            Some("changed: $.body.items[0].qty: 2 → 3; $.body.x added")
        );

        let updated = run(
            r#"{"id":1,"updatedAt":"t3","items":[{"id":"c","qty":3}],"x":true}"#,
            true,
        );
        assert_eq!(updated.name, "Snapshot \"order\" updated");
        assert!(
            run(
                r#"{"id":1,"updatedAt":"t4","items":[{"id":"d","qty":3}],"x":true}"#,
                false
            )
            .passed
        );
    }

    #[test]
    fn keeps_text_bodies_and_the_status() {
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let entry = create_request(ws.path(), "", "Health").unwrap();
        let check = SnapshotCheck {
            name: "health".into(),
            ignore: Vec::new(),
        };
        assert!(check_one(ws.path(), &entry.id, &response(200, "ok"), &check, false).passed);
        let failed = check_one(ws.path(), &entry.id, &response(503, "ok"), &check, false);
        assert_eq!(
            failed.error.as_deref(),
            Some("changed: $.status: 200 → 503")
        );
    }

    fn check_one(
        root: &Path,
        id: &str,
        response: &HttpResponse,
        check: &SnapshotCheck,
        update: bool,
    ) -> TestResult {
        super::check(root, id, response, std::slice::from_ref(check), update)
            .pop()
            .unwrap()
    }
}
