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

/// Repeat a request until a condition holds (a payment that settles, a job that ends).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    /// A condition like `{{state}} == PAID` or `{{$status}} == 200`.
    pub until: String,
    #[serde(default = "five")]
    pub times: u32,
    #[serde(default = "one_second")]
    pub every: String,
}

fn five() -> u32 {
    5
}

fn one_second() -> String {
    "1s".into()
}

/// What any step can add: run only `if` a condition holds, or once per item of a list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    #[serde(default, rename = "if", skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// A list: a variable holding a JSON array (`{{ids}}`) or `a, b, c`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<String>,
    /// The variable each item goes to (`item` when left out).
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

/// One step: a request of the collection, another flow, or a pause.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<Retry>,
        #[serde(flatten)]
        control: Control,
    },
    Flow {
        /// Flow id (the file name without `.flow.yaml`).
        flow: String,
        #[serde(flatten)]
        control: Control,
    },
    Wait {
        /// `500ms`, `2s`, `1m`.
        #[serde(deserialize_with = "text_or_number")]
        wait: String,
        #[serde(flatten)]
        control: Control,
    },
}

impl Step {
    pub fn control(&self) -> &Control {
        match self {
            Step::Request { control, .. }
            | Step::Flow { control, .. }
            | Step::Wait { control, .. } => control,
        }
    }

    /// A step that runs a request, with nothing else set.
    pub fn request(id: &str) -> Step {
        Step::Request {
            request: id.to_string(),
            name: None,
            save: BTreeMap::new(),
            expect: None,
            retry: None,
            control: Control::default(),
        }
    }
}

fn text_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum TextOrNumber {
        Text(String),
        Number(u64),
    }
    Ok(match TextOrNumber::deserialize(d)? {
        TextOrNumber::Text(t) => t,
        TextOrNumber::Number(n) => format!("{n}ms"),
    })
}

/// `500ms`, `2s`, `1m` (a bare number is milliseconds); at most 10 minutes.
pub fn duration(text: &str) -> Result<std::time::Duration, String> {
    let t = text.trim();
    let (number, unit) = match t.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(i) => (&t[..i], t[i..].trim()),
        None => (t, "ms"),
    };
    let n: f64 = number
        .parse()
        .map_err(|_| format!("{text} is not a duration (500ms, 2s, 1m)"))?;
    let ms = match unit {
        "ms" => n,
        "s" => n * 1000.0,
        "m" | "min" => n * 60_000.0,
        _ => return Err(format!("{text} is not a duration (500ms, 2s, 1m)")),
    };
    if ms > 600_000.0 {
        return Err(format!("{text} is too long (10 minutes at most)"));
    }
    Ok(std::time::Duration::from_millis(ms as u64))
}

/// `{{name}}` → its value, or nothing when it has none.
fn interpolate(text: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("}}") {
            Some(end) => {
                let name = rest[start + 2..start + 2 + end].trim();
                out.push_str(vars.get(name).map(String::as_str).unwrap_or_default());
                rest = &rest[start + 2 + end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn truthy(value: &str) -> bool {
    let v = value.trim();
    !(v.is_empty() || ["false", "0", "null", "undefined", "[]"].contains(&v))
}

fn unquote(text: &str) -> &str {
    let t = text.trim();
    for q in ['\'', '"'] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            return &t[1..t.len() - 1];
        }
    }
    t
}

/// A condition: `{{a}} == b`, `!=`, `>`, `>=`, `<`, `<=`, `contains`, or just `{{a}}` (has a
/// value that is not false, 0 or null) and `!{{a}}`. Numbers compare as numbers.
pub fn holds(condition: &str, vars: &HashMap<String, String>) -> bool {
    let c = condition.trim();
    for op in [
        " not contains ",
        " contains ",
        "==",
        "!=",
        ">=",
        "<=",
        ">",
        "<",
    ] {
        if let Some(i) = c.find(op) {
            let left = interpolate(unquote(&c[..i]), vars);
            let right = interpolate(unquote(&c[i + op.len()..]), vars);
            let numbers = left
                .trim()
                .parse::<f64>()
                .ok()
                .zip(right.trim().parse::<f64>().ok());
            let order = match numbers {
                Some((a, b)) => a.partial_cmp(&b),
                None => Some(left.as_str().cmp(right.as_str())),
            };
            return match op {
                " contains " => left.contains(&right),
                " not contains " => !left.contains(&right),
                "==" => order == Some(std::cmp::Ordering::Equal),
                "!=" => order != Some(std::cmp::Ordering::Equal),
                ">=" => order.is_some_and(|o| o.is_ge()),
                "<=" => order.is_some_and(|o| o.is_le()),
                ">" => order.is_some_and(|o| o.is_gt()),
                _ => order.is_some_and(|o| o.is_lt()),
            };
        }
    }
    match c.strip_prefix('!') {
        Some(rest) => !truthy(&interpolate(unquote(rest), vars)),
        None => truthy(&interpolate(unquote(c), vars)),
    }
}

/// The items of a `forEach`: a JSON array, or a list separated by commas.
fn items_of(list: &str, vars: &HashMap<String, String>) -> Result<Vec<String>, String> {
    let text = interpolate(list, vars);
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("forEach {list} has no value"));
    }
    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str(text) {
        return Ok(items.iter().map(value_text).collect());
    }
    Ok(text
        .split(',')
        .map(|s| unquote(s).to_string())
        .filter(|s| !s.is_empty())
        .collect())
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
        let (first, value) = match step {
            Step::Request { request, .. } => ("request", request),
            Step::Flow { flow, .. } => ("flow", flow),
            Step::Wait { wait, .. } => ("wait", wait),
        };
        out.push_str(&format!("  - {first}: {}\n", quote(value)));
        let control = step.control();
        if let Some(when) = &control.when {
            out.push_str(&format!("    if: {}\n", quote(when)));
        }
        if let Some(list) = &control.for_each {
            out.push_str(&format!("    forEach: {}\n", quote(list)));
        }
        if let Some(item) = &control.item {
            out.push_str(&format!("    as: {}\n", quote(item)));
        }
        match step {
            Step::Request {
                name,
                save,
                expect,
                retry,
                ..
            } => {
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
                if let Some(retry) = retry {
                    out.push_str(&format!(
                        "    retry:\n      until: {}\n      times: {}\n      every: {}\n",
                        quote(&retry.until),
                        retry.times,
                        quote(&retry.every)
                    ));
                }
            }
            Step::Flow { .. } | Step::Wait { .. } => {}
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    /// `"2"`, `"1.3"`: position in the flow and its subflows.
    pub path: String,
    pub depth: usize,
    /// `request`, `flow` (a subflow header), `wait` or `missing`.
    pub kind: String,
    /// `if` of the step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// `forEach` of the step, as `{{ids}} as id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<String>,
    /// `retry` of the step, as `until …, 5 times every 1s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
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
        let control = step.control();
        let base = PlanNode {
            path: path.clone(),
            depth,
            condition: control.when.clone(),
            for_each: control
                .for_each
                .as_ref()
                .map(|list| format!("{list} as {}", control.item.as_deref().unwrap_or("item"))),
            ..Default::default()
        };
        match step {
            Step::Wait { wait, .. } => out.push(PlanNode {
                kind: "wait".into(),
                target: wait.clone(),
                name: format!("Wait {wait}"),
                problem: duration(wait).err(),
                ..base
            }),
            Step::Request {
                request,
                name,
                save,
                expect,
                retry,
                ..
            } => {
                let found = collection::load_request(root, request).ok();
                let bad_retry = retry.as_ref().and_then(|r| duration(&r.every).err());
                out.push(PlanNode {
                    retry: retry
                        .as_ref()
                        .map(|r| format!("until {}, {} times every {}", r.until, r.times, r.every)),
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
                        .then(|| format!("no request {request} in the collection"))
                        .or(bad_retry),
                    ..base
                });
            }
            Step::Flow { flow: sub, .. } => {
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
                    ..base
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// `2/5` inside a `forEach`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<String>,
    /// Why it was skipped or failed without a response, or how many attempts it took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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

type OnStep<'s> = &'s mut (dyn FnMut(&StepEvent) + Send);
type Running<'s> = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 's>>;

struct Runner<'a> {
    root: &'a Path,
    session: Session,
    /// Values saved by the flow and the current `forEach` items.
    vars: crate::data::Row,
    nodes: Vec<PlanNode>,
    report: FlowReport,
    failed: bool,
    last_status: Option<u16>,
}

impl Runner<'_> {
    fn emit(&mut self, on_step: OnStep<'_>, event: StepEvent) {
        on_step(&event);
        if event.state != "running" {
            if event.state == "failed" {
                self.failed = true;
                self.report.passed = false;
            }
            self.report.steps.push(event);
        }
    }

    /// What conditions see: the environment, then the flow's values and `$status`.
    fn condition_vars(&self) -> HashMap<String, String> {
        let mut vars = self.session.variables();
        vars.extend(self.vars.clone());
        if let Some(status) = self.last_status {
            vars.insert("$status".into(), status.to_string());
        }
        vars
    }

    /// Marks a step and everything under it as skipped.
    fn skip(&mut self, on_step: OnStep<'_>, path: &str, note: &str) {
        let inside: Vec<String> = self
            .nodes
            .iter()
            .filter(|n| n.kind == "request" || n.kind == "wait")
            .filter(|n| n.path == path || n.path.starts_with(&format!("{path}.")))
            .map(|n| n.path.clone())
            .collect();
        for p in inside {
            self.emit(
                on_step,
                StepEvent {
                    path: p,
                    state: "skipped".into(),
                    note: Some(note.to_string()),
                    ..Default::default()
                },
            );
        }
    }

    fn steps<'s>(&'s mut self, on_step: OnStep<'s>, flow: Flow, prefix: String) -> Running<'s> {
        Box::pin(async move {
            for (i, step) in flow.steps.iter().enumerate() {
                if self.failed {
                    return;
                }
                let path = if prefix.is_empty() {
                    format!("{}", i + 1)
                } else {
                    format!("{prefix}.{}", i + 1)
                };
                let control = step.control().clone();
                if let Some(when) = &control.when {
                    if !holds(when, &self.condition_vars()) {
                        self.skip(on_step, &path, &format!("if {when}: no"));
                        continue;
                    }
                }
                let items: Vec<Option<String>> = match &control.for_each {
                    None => vec![None],
                    Some(list) => match items_of(list, &self.condition_vars()) {
                        Ok(items) => items.into_iter().map(Some).collect(),
                        Err(e) => {
                            self.emit(
                                on_step,
                                StepEvent {
                                    path: path.clone(),
                                    state: "failed".into(),
                                    note: Some(e),
                                    ..Default::default()
                                },
                            );
                            return;
                        }
                    },
                };
                if items.is_empty() {
                    self.skip(on_step, &path, "forEach: the list is empty");
                    continue;
                }
                let total = items.len();
                for (n, item) in items.into_iter().enumerate() {
                    if self.failed {
                        return;
                    }
                    let iteration = item.as_ref().map(|_| format!("{}/{total}", n + 1));
                    if let Some(item) = item {
                        let name = control.item.clone().unwrap_or_else(|| "item".into());
                        self.vars.insert(name, item);
                    }
                    match step {
                        Step::Request { .. } => {
                            self.request(&mut *on_step, step, &path, iteration).await
                        }
                        Step::Wait { wait, .. } => {
                            self.wait(&mut *on_step, wait, &path, iteration).await
                        }
                        Step::Flow { flow: sub, .. } => {
                            // `plan` already refused cycles and missing flows.
                            if let Ok(subflow) = load_flow(self.root, sub) {
                                self.steps(&mut *on_step, subflow, path.clone()).await;
                            }
                        }
                    }
                }
            }
        })
    }

    async fn wait(
        &mut self,
        on_step: OnStep<'_>,
        text: &str,
        path: &str,
        iteration: Option<String>,
    ) {
        let event = StepEvent {
            path: path.to_string(),
            iteration,
            ..Default::default()
        };
        match duration(text) {
            Ok(d) => {
                on_step(&StepEvent {
                    state: "running".into(),
                    ..event.clone()
                });
                tokio::time::sleep(d).await;
                self.emit(
                    on_step,
                    StepEvent {
                        state: "passed".into(),
                        note: Some(format!("waited {text}")),
                        ..event
                    },
                );
            }
            Err(e) => self.emit(
                on_step,
                StepEvent {
                    state: "failed".into(),
                    note: Some(e),
                    ..event
                },
            ),
        }
    }

    async fn request(
        &mut self,
        on_step: OnStep<'_>,
        step: &Step,
        path: &str,
        iteration: Option<String>,
    ) {
        let Step::Request {
            request,
            save,
            expect,
            retry,
            ..
        } = step
        else {
            return;
        };
        let event = StepEvent {
            path: path.to_string(),
            iteration: iteration.clone(),
            ..Default::default()
        };
        let entry: RequestEntry = match collection::load_request(self.root, request) {
            Ok(entry) => entry,
            Err(e) => {
                return self.emit(
                    on_step,
                    StepEvent {
                        state: "failed".into(),
                        note: Some(e.to_string()),
                        ..event
                    },
                )
            }
        };
        let times = retry.as_ref().map(|r| r.times.max(1)).unwrap_or(1);
        let every = retry
            .as_ref()
            .and_then(|r| duration(&r.every).ok())
            .unwrap_or_default();
        let mut attempt = 0;
        loop {
            attempt += 1;
            on_step(&StepEvent {
                state: "running".into(),
                note: retry.as_ref().map(|_| format!("attempt {attempt}/{times}")),
                ..event.clone()
            });
            let options = RunOptions {
                data: self.vars.clone(),
                ..Default::default()
            };
            let mut run_report = run::run_in(self.root, &entry, &mut self.session, &options).await;
            run_report.name = match &iteration {
                Some(i) => format!("{path} · {} ({i})", entry.name),
                None => format!("{path} · {}", entry.name),
            };
            self.last_status = run_report.response.as_ref().map(|r| r.status);
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
            let mut saved = BTreeMap::new();
            if let Some(response) = &run_report.response {
                for (var, json_path) in save {
                    match crate::query::json_path(&response.body, json_path) {
                        Ok(values) if !values.is_empty() => {
                            // Several matches keep them all, as a list for `forEach`.
                            let text = if values.len() == 1 {
                                value_text(&values[0])
                            } else {
                                serde_json::Value::Array(values).to_string()
                            };
                            self.vars.insert(var.clone(), text.clone());
                            saved.insert(var.clone(), text);
                        }
                        Ok(_) => run_report.tests.push(TestResult {
                            name: format!("save {var}"),
                            passed: false,
                            error: Some(format!("{json_path} found nothing in the response")),
                        }),
                        Err(e) => run_report.tests.push(TestResult {
                            name: format!("save {var}"),
                            passed: false,
                            error: Some(format!("{json_path}: {e}")),
                        }),
                    }
                }
            }
            let mut note = None;
            if let Some(retry) = retry {
                let done = holds(&retry.until, &self.condition_vars());
                if !done && attempt < times {
                    tokio::time::sleep(every).await;
                    continue;
                }
                if !done {
                    run_report.tests.push(TestResult {
                        name: format!("until {}", retry.until),
                        passed: false,
                        error: Some(format!("still not true after {times} attempts")),
                    });
                }
                note = Some(format!(
                    "{attempt} attempt{}",
                    if attempt == 1 { "" } else { "s" }
                ));
            }
            let passed = run_report.passed();
            return self.emit(
                on_step,
                StepEvent {
                    state: if passed { "passed" } else { "failed" }.into(),
                    report: Some(run_report),
                    saved,
                    note,
                    ..event
                },
            );
        }
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
    let session = Session::open(root, environment_id, store, false)?.detached();
    let mut runner = Runner {
        root,
        session,
        vars: HashMap::new(),
        nodes,
        report: FlowReport {
            flow: flow.name.clone(),
            passed: true,
            steps: Vec::new(),
        },
        failed: false,
        last_status: None,
    };
    runner.steps(&mut *on_step, flow, String::new()).await;
    // What never ran because an earlier step failed.
    let pending: Vec<String> = runner
        .nodes
        .iter()
        .filter(|n| n.kind == "request" || n.kind == "wait")
        .filter(|n| !runner.report.steps.iter().any(|s| s.path == n.path))
        .map(|n| n.path.clone())
        .collect();
    for path in pending {
        runner.emit(
            on_step,
            StepEvent {
                path,
                state: "skipped".into(),
                ..Default::default()
            },
        );
    }
    Ok(runner.report)
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
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
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
                } else if text.starts_with("GET /orders ") {
                    (
                        "200 OK",
                        r#"{"items":[{"id":11},{"id":12},{"id":13}],"role":"admin"}"#.to_string(),
                    )
                } else if let Some(rest) = text.strip_prefix("GET /orders/") {
                    let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    ("200 OK", format!(r#"{{"id":{id}}}"#))
                } else if text.starts_with("GET /job") {
                    // Running twice, then done.
                    let n = polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let state = if n < 2 { "RUNNING" } else { "DONE" };
                    ("200 OK", format!(r#"{{"state":"{state}"}}"#))
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
                    retry: None,
                    control: Control::default(),
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
                        control: Control::default(),
                    },
                    Step::Request {
                        request: "02-me.pulpo.yaml".into(),
                        name: Some("Profile".into()),
                        save: BTreeMap::new(),
                        expect: Some(Expect { status: Some(200) }),
                        retry: None,
                        control: Control::default(),
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
                        retry: None,
                        control: Control::default(),
                    },
                    Step::Request {
                        request: "02-me.pulpo.yaml".into(),
                        name: None,
                        save: BTreeMap::new(),
                        expect: None,
                        retry: None,
                        control: Control::default(),
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
                    control: Control::default(),
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
                    retry: None,
                    control: Control::default(),
                }],
            },
        )
        .unwrap();
        assert_eq!(plan(root, "gone").unwrap()[0].kind, "missing");
        assert!(load_flow(root, "../etc").is_err());
    }

    #[test]
    fn conditions_and_durations() {
        let mut vars = HashMap::new();
        vars.insert("role".to_string(), "admin".to_string());
        vars.insert("total".to_string(), "12.5".to_string());
        vars.insert("$status".to_string(), "201".to_string());
        assert!(holds("{{role}} == admin", &vars));
        assert!(holds("{{role}} == 'admin'", &vars));
        assert!(!holds("{{role}} != admin", &vars));
        assert!(holds("{{total}} > 9", &vars));
        assert!(holds("{{total}} <= 12.5", &vars));
        assert!(holds("{{$status}} >= 200", &vars));
        assert!(holds("{{role}} contains dm", &vars));
        assert!(holds("{{role}}", &vars));
        assert!(!holds("{{missing}}", &vars));
        assert!(holds("!{{missing}}", &vars));
        assert_eq!(duration("500ms").unwrap().as_millis(), 500);
        assert_eq!(duration("2s").unwrap().as_millis(), 2000);
        assert_eq!(duration("1m").unwrap().as_secs(), 60);
        assert_eq!(duration("250").unwrap().as_millis(), 250);
        assert!(duration("soon").is_err());
        assert!(duration("1h").is_err());
        assert_eq!(items_of("a, 'b' ,c", &vars).unwrap(), vec!["a", "b", "c"]);
        assert!(items_of("{{nothing}}", &vars).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_conditions_loops_retries_and_waits() {
        let base = api().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Shop").unwrap();
        request(
            root,
            "01-orders.pulpo.yaml",
            "Orders",
            HttpMethod::Get,
            &format!("{base}/orders"),
            false,
        );
        request(
            root,
            "02-order.pulpo.yaml",
            "Order",
            HttpMethod::Get,
            &format!("{base}/orders/{{{{orderId}}}}"),
            false,
        );
        request(
            root,
            "03-job.pulpo.yaml",
            "Job",
            HttpMethod::Get,
            &format!("{base}/job"),
            false,
        );
        let yaml = r#"version: 1
name: "Everything"
steps:
  - request: "01-orders.pulpo.yaml"
    save:
      ids: "$.items[*].id"
      role: "$.role"
  - request: "02-order.pulpo.yaml"
    forEach: "{{ids}}"
    as: "orderId"
    save:
      seen: "$.id"
  - request: "02-order.pulpo.yaml"
    if: "{{role}} == guest"
  - wait: 20ms
    if: "{{role}} == admin"
  - request: "03-job.pulpo.yaml"
    retry:
      until: "{{state}} == DONE"
      times: 5
      every: 10ms
    save:
      state: "$.state"
"#;
        fs::create_dir_all(root.join(FLOWS_DIR)).unwrap();
        fs::write(root.join("flows/everything.flow.yaml"), yaml).unwrap();
        let flow = load_flow(root, "everything").unwrap();
        // Written back without losing anything.
        let again = save_flow(root, &flow).unwrap();
        assert_eq!(load_flow(root, &again.id).unwrap().steps, flow.steps);
        let nodes = plan(root, "everything").unwrap();
        assert_eq!(nodes[1].for_each.as_deref(), Some("{{ids}} as orderId"));
        assert_eq!(nodes[2].condition.as_deref(), Some("{{role}} == guest"));
        assert_eq!(nodes[3].kind, "wait");
        assert!(nodes[4]
            .retry
            .as_deref()
            .unwrap()
            .starts_with("until {{state}} == DONE"));

        let report = run_flow(
            root,
            "everything",
            None,
            &MemoryStore::default(),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert!(report.passed, "{report:#?}");
        let summary: Vec<String> = report
            .steps
            .iter()
            .map(|s| {
                format!(
                    "{} {} {} {}",
                    s.path,
                    s.state,
                    s.iteration.as_deref().unwrap_or("-"),
                    s.note.as_deref().unwrap_or("-")
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                "1 passed - -",
                "2 passed 1/3 -",
                "2 passed 2/3 -",
                "2 passed 3/3 -",
                "3 skipped - if {{role}} == guest: no",
                "4 passed - waited 20ms",
                "5 passed - 3 attempts",
            ]
        );
        assert_eq!(report.steps[0].saved["ids"], "[11,12,13]");
        assert_eq!(report.steps[3].saved["seen"], "13");

        // A retry that never gets there fails the step.
        let yaml = "version: 1
name: Never
steps:
  - request: \"03-job.pulpo.yaml\"
    retry:
      until: \"{{$status}} == 500\"
      times: 2
      every: 1ms
  - wait: 1ms
";
        fs::write(root.join("flows/never.flow.yaml"), yaml).unwrap();
        let report = run_flow(root, "never", None, &MemoryStore::default(), &mut |_| {})
            .await
            .unwrap();
        assert!(!report.passed);
        let tests = &report.steps[0].report.as_ref().unwrap().tests;
        assert_eq!(
            tests[0].error.as_deref(),
            Some("still not true after 2 attempts")
        );
        assert_eq!(report.steps[1].state, "skipped");

        // A bad duration is caught before anything runs.
        fs::write(
            root.join("flows/bad.flow.yaml"),
            "version: 1
name: Bad
steps:
  - wait: soon
",
        )
        .unwrap();
        let error = run_flow(root, "bad", None, &MemoryStore::default(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(error.contains("not a duration"), "{error}");
    }
}
