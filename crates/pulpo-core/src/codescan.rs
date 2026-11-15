//! The collection from the code itself, when there is no contract: endpoints of Spring
//! (Java/Kotlin), NestJS, Express/Fastify, FastAPI, Next.js and Nuxt, found by reading
//! the source. Grouped by feature (controller, router file, tag), not by file path.
//!
//! It reads text, it does not compile: anything computed at run time (paths built from
//! constants, routers mounted conditionally) is skipped or reported.

use crate::collection::Scripts;
use crate::environment::{Environment, Variable};
use crate::http::{Header, HttpRequest};
use crate::import::{ImportFormat, ImportNode, ImportPlan};
use crate::HttpMethod;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;

const SKIPPED_DIRS: [&str; 18] = [
    ".git",
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "bin",
    "obj",
    "vendor",
    ".gradle",
    ".idea",
    ".pulpo",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".output",
];
const MANIFESTS: [&str; 8] = [
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "setup.py",
    "go.mod",
];
const MAX_FILE: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub method: String,
    /// With `{param}` path parameters.
    pub path: String,
    pub name: String,
    /// The feature it belongs to: controller, router file or tag.
    pub group: String,
    pub framework: String,
    /// `file:line`, relative to the module.
    pub at: String,
    pub query: Vec<String>,
    pub headers: Vec<String>,
    pub body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Module {
    pub dir: PathBuf,
    pub name: String,
    pub framework: String,
    pub endpoints: Vec<Endpoint>,
    pub base_url: String,
    pub warnings: Vec<String>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

/// "getUserById" / "get_user_by_id" → "Get user by id".
pub fn humanize(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let boundary = c.is_uppercase()
            && !current.is_empty()
            && (chars[i - 1].is_lowercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
        if boundary {
            words.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut text = words
        .iter()
        .map(|w| {
            if w.chars().all(|c| c.is_uppercase()) && w.len() > 1 {
                w.clone()
            } else {
                w.to_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(first) = text.get(0..1) {
        text = first.to_uppercase() + &text[1..];
    }
    text
}

/// The text inside the parentheses that open at `open` (which must be `(`).
fn balanced(text: &str, open: usize) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = open;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' {
                    i += 1;
                } else if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' | b'`' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((i, &text[open + 1..i]));
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// String literals in a piece of code, in order.
fn literals(text: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r#""((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|`([^`$]*)`"#)
        .captures_iter(text)
        .filter_map(|c| {
            c.get(1)
                .or(c.get(2))
                .or(c.get(3))
                .map(|m| m.as_str().to_string())
        })
        .collect()
}

/// The value of `name = "x"` (Java/Kotlin/Python) or `name: 'x'` (JS) in annotation
/// arguments, else the first bare literal.
fn named_or_first(args: &str, names: &[&str]) -> Option<String> {
    for name in names {
        let pattern =
            format!(r#"\b{name}\s*[=:]\s*(?:\{{\s*|\[\s*)?(?:"((?:[^"\\]|\\.)*)"|'([^']*)')"#);
        if let Some(c) = Regex::new(&pattern).ok()?.captures(args) {
            return c.get(1).or(c.get(2)).map(|m| m.as_str().to_string());
        }
    }
    // A positional value comes before any `name =`.
    let first_named = Regex::new(r"\b\w+\s*[=:]")
        .ok()?
        .find(args)
        .map(|m| m.start())
        .unwrap_or(args.len());
    literals(&args[..first_named]).into_iter().next()
}

fn join_paths(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        for segment in part.split('/').filter(|s| !s.is_empty()) {
            out.push('/');
            out.push_str(segment);
        }
    }
    if out.is_empty() {
        "/".into()
    } else {
        out
    }
}

/// `:id` and `<id>` / `<int:id>` as `{id}`.
fn normalize_params(path: &str) -> String {
    path.split('/')
        .map(|s| {
            if let Some(name) = s.strip_prefix(':') {
                format!("{{{}}}", name.trim_end_matches('?'))
            } else if s.starts_with('<') && s.ends_with('>') {
                format!("{{{}}}", s[1..s.len() - 1].rsplit(':').next().unwrap_or(s))
            } else {
                s.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].matches('\n').count() + 1
}

/// Top-level comma split (parentheses, brackets, braces, generics and quotes respected).
fn split_args(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '(' | '[' | '{' | '<' => depth += 1,
                ')' | ']' | '}' | '>' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out.into_iter().filter(|a| !a.trim().is_empty()).collect()
}

fn last_ident(text: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"([A-Za-z_]\w*)\s*$")
        .captures(text.trim())
        .map(|c| c[1].to_string())
}

/// Where a declaration ends: the first `{`, `;` or `=` outside parentheses and quotes.
fn top_level_end(decl: &str) -> usize {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in decl.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                '{' | ';' | '=' if depth == 0 => return i,
                _ => {}
            },
        }
    }
    decl.len()
}

// --------------------------------------------------------------------- Spring

fn spring(file: &str, text: &str, out: &mut Vec<Endpoint>) {
    if !(text.contains("@RestController")
        || text.contains("@Controller")
        || text.contains("@RequestMapping"))
    {
        return;
    }
    static CLASS: OnceLock<Regex> = OnceLock::new();
    static MAP: OnceLock<Regex> = OnceLock::new();
    static ANNOT: OnceLock<Regex> = OnceLock::new();
    let class_re = re(&CLASS, r"\b(class|interface)\s+(\w+)");
    let Some(class) = class_re.captures(text) else {
        return;
    };
    let class_at = class.get(0).map(|m| m.start()).unwrap_or(0);
    let class_name = class[2].to_string();
    let head = &text[..class_at];
    let annot = |name: &str, scope: &str| -> Option<String> {
        let i = scope.rfind(&format!("@{name}("))?;
        balanced(scope, i + name.len() + 1).map(|(_, a)| a.to_string())
    };
    let prefix = annot("RequestMapping", head)
        .and_then(|a| named_or_first(&a, &["value", "path"]))
        .unwrap_or_default();
    let group = annot("Tag", head)
        .and_then(|a| named_or_first(&a, &["name"]))
        .unwrap_or_else(|| {
            humanize(
                class_name
                    .trim_end_matches("Controller")
                    .trim_end_matches("Resource"),
            )
        });
    let map_re = re(&MAP, r"@(Get|Post|Put|Delete|Patch|Request)Mapping\b");
    let annot_re = re(&ANNOT, r"@[\w.]+");
    let body = &text[class_at..];
    let matches: Vec<_> = map_re.find_iter(body).collect();
    // Where the previous method's declaration ended: its annotations are behind it.
    let mut prev_end = 0;
    for (n, m) in matches.iter().enumerate() {
        let kind = &body[m.start() + 1..m.end() - "Mapping".len()];
        let after = m.end();
        let (args, rest_at) = match body[after..].find(|c: char| !c.is_whitespace()) {
            Some(off) if body.as_bytes()[after + off] == b'(' => {
                match balanced(body, after + off) {
                    Some((close, a)) => (a.to_string(), close + 1),
                    None => continue,
                }
            }
            _ => (String::new(), after),
        };
        let method = if kind == "Request" {
            static RM: OnceLock<Regex> = OnceLock::new();
            re(&RM, r"RequestMethod\.(\w+)")
                .captures(&args)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "GET".into())
        } else {
            kind.to_uppercase()
        };
        let path = named_or_first(&args, &["value", "path"]).unwrap_or_default();
        if !args.is_empty()
            && literals(&args).is_empty()
            && !args.contains("method")
            && !args.contains("produces")
            && !args.contains("consumes")
        {
            // A path built from a constant: not readable here.
            continue;
        }
        // The signature: up to the method body or the end of the declaration.
        let end = matches.get(n + 1).map(|x| x.start()).unwrap_or(body.len());
        let decl = &body[rest_at..end];
        let sig_end = top_level_end(decl);
        let signature = &decl[..sig_end];
        // Strip annotations to find `name(`.
        let mut plain = String::new();
        let mut params_at = None;
        let mut i = 0;
        let sbytes = signature.as_bytes();
        while i < signature.len() {
            if sbytes[i] == b'@' {
                if let Some(a) = annot_re.find_at(signature, i).filter(|a| a.start() == i) {
                    i = a.end();
                    let ws = signature[i..].len() - signature[i..].trim_start().len();
                    if signature[i + ws..].starts_with('(') {
                        if let Some((close, _)) = balanced(signature, i + ws) {
                            i = close + 1;
                            continue;
                        }
                    }
                    continue;
                }
            }
            if sbytes[i] == b'(' && params_at.is_none() {
                params_at = Some(i);
            }
            plain.push(sbytes[i] as char);
            i += 1;
        }
        static NAME: OnceLock<Regex> = OnceLock::new();
        let name = re(&NAME, r"(?:fun\s+)?(\w+)\s*\(")
            .captures(&plain)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let params = params_at
            .and_then(|p| balanced(signature, p))
            .map(|(_, p)| p.to_string())
            .unwrap_or_default();
        let (mut query, mut headers, mut has_body) = (Vec::new(), Vec::new(), false);
        for param in split_args(&params) {
            let optional = param.contains("required = false")
                || param.contains("required=false")
                || param.contains("defaultValue");
            let arg_of = |a: &str| {
                param.find(&format!("@{a}")).and_then(|i| {
                    let j = i + a.len() + 1;
                    balanced(param, j)
                        .map(|(_, x)| x.to_string())
                        .or(Some(String::new()))
                })
            };
            let ident = last_ident(param.split(':').next().unwrap_or(param)).unwrap_or_default();
            if let Some(a) = arg_of("RequestParam") {
                if !optional {
                    query.push(named_or_first(&a, &["value", "name"]).unwrap_or(ident));
                }
            } else if let Some(a) = arg_of("RequestHeader") {
                if !optional {
                    headers.push(named_or_first(&a, &["value", "name"]).unwrap_or(ident));
                }
            } else if param.contains("@RequestBody") {
                has_body = true;
            }
        }
        // `@Operation(summary = "…")` just before the mapping.
        // `@Operation(summary = "…")` before or after the mapping, before the method.
        let this_end = rest_at + sig_end;
        let before = &body[prev_end.min(m.start())..this_end.max(m.start())];
        prev_end = this_end;
        let summary = before
            .rfind("@Operation(")
            .and_then(|i| balanced(before, i + "@Operation".len()))
            .and_then(|(_, a)| named_or_first(a, &["summary"]));
        out.push(Endpoint {
            method,
            path: join_paths(&[&prefix, &path]),
            name: summary.unwrap_or_else(|| humanize(&name)),
            group: group.clone(),
            framework: "spring".into(),
            at: format!("{file}:{}", line_of(text, class_at + m.start())),
            query,
            headers,
            body: has_body,
        });
    }
}

// --------------------------------------------------------------------- NestJS

fn nest(file: &str, text: &str, out: &mut Vec<Endpoint>) {
    let Some(ctrl) = text.find("@Controller(") else {
        return;
    };
    let prefix = balanced(text, ctrl + "@Controller".len())
        .and_then(|(_, a)| named_or_first(a, &["path"]))
        .unwrap_or_default();
    static CLASS: OnceLock<Regex> = OnceLock::new();
    let class_name = re(&CLASS, r"class\s+(\w+)")
        .captures(&text[ctrl..])
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    let group = humanize(class_name.trim_end_matches("Controller"));
    static M: OnceLock<Regex> = OnceLock::new();
    for m in re(&M, r"@(Get|Post|Put|Delete|Patch)\(").find_iter(text) {
        let method = text[m.start() + 1..m.end() - 1].to_uppercase();
        let Some((close, args)) = balanced(text, m.end() - 1) else {
            continue;
        };
        let path = literals(args).into_iter().next().unwrap_or_default();
        let rest = &text[close + 1..];
        // Skip other decorators; the next `name(` is the handler.
        static NAME: OnceLock<Regex> = OnceLock::new();
        let mut name = String::new();
        let mut at = 0;
        while let Some(c) = re(&NAME, r"(@?)(\w+)\s*\(").captures(&rest[at..]) {
            let whole = c.get(0).unwrap();
            if c[1].is_empty() && &c[2] != "async" {
                name = c[2].to_string();
                at += whole.end() - 1;
                break;
            }
            match balanced(rest, at + whole.end() - 1) {
                Some((close, _)) => at = close + 1,
                None => break,
            }
        }
        let params = balanced(rest, at).map(|(_, p)| p).unwrap_or_default();
        out.push(Endpoint {
            method,
            path: join_paths(&[&prefix, &normalize_params(&path)]),
            name: humanize(&name),
            group: group.clone(),
            framework: "nestjs".into(),
            at: format!("{file}:{}", line_of(text, m.start())),
            query: Vec::new(),
            headers: Vec::new(),
            body: params.contains("@Body("),
        });
    }
}

// --------------------------------------------------------------------- Express / Fastify

/// One JS/TS file: routes on routers, mounts of other routers, imports.
#[derive(Default)]
struct JsFile {
    routes: Vec<(String, String, String, usize)>, // method, path, name, line
    mounts: Vec<(String, String)>,                // prefix, identifier
    imports: HashMap<String, String>,             // identifier → module spec
}

fn express_file(text: &str) -> Option<JsFile> {
    let uses_server = [
        "from 'express'",
        "from \"express\"",
        "require('express')",
        "require(\"express\")",
        "fastify",
        "Router()",
        "koa-router",
    ]
    .iter()
    .any(|m| text.contains(m));
    if !uses_server {
        return None;
    }
    static DECL: OnceLock<Regex> = OnceLock::new();
    let mut receivers: Vec<String> = re(&DECL, r"(?:const|let|var)\s+(\w+)\s*(?::\s*[\w.<>]+\s*)?=\s*(?:express\s*\(\s*\)|express\.Router\s*\(|Router\s*\(|(?:F|f)astify\s*\(|new\s+Router\s*\()")
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect();
    for name in ["app", "router", "server", "fastify"] {
        if !receivers.iter().any(|r| r == name) {
            receivers.push(name.to_string());
        }
    }
    let alt = receivers.join("|");
    let route_re = Regex::new(&format!(
        r#"\b({alt})\s*\.\s*(get|post|put|delete|patch)\s*\(\s*['"`](/[^'"`]*)['"`]"#
    ))
    .ok()?;
    let chained = Regex::new(&format!(
        r#"\b({alt})\s*\.\s*route\s*\(\s*['"`](/[^'"`]*)['"`]\s*\)"#
    ))
    .ok()?;
    let mount_re = Regex::new(&format!(r#"\b({alt})\s*\.\s*(?:use|register)\s*\(\s*['"`](/[^'"`]*)['"`]\s*,\s*(?:[\w.]+\s*,\s*)*(\w+)"#)).ok()?;
    let mut file = JsFile::default();
    for c in route_re.captures_iter(text) {
        let whole = c.get(0).unwrap();
        // The handler: `controller.estimate(` or the last bare argument.
        let rest = balanced(text, text[..whole.end()].rfind('(').unwrap_or(0))
            .map(|(_, a)| a.to_string())
            .unwrap_or_default();
        static CALL: OnceLock<Regex> = OnceLock::new();
        let name = re(&CALL, r"\.(\w+)\s*\(\s*req")
            .captures(&rest)
            .map(|x| x[1].to_string())
            .or_else(|| {
                split_args(&rest).last().and_then(|a| {
                    let a = a.trim();
                    a.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
                        .then(|| a.rsplit('.').next().unwrap_or(a).to_string())
                })
            })
            .unwrap_or_default();
        file.routes.push((
            c[2].to_uppercase(),
            c[3].to_string(),
            name,
            line_of(text, whole.start()),
        ));
    }
    for c in chained.captures_iter(text) {
        let whole = c.get(0).unwrap();
        static NEXT: OnceLock<Regex> = OnceLock::new();
        let next = re(&NEXT, r"^\s*\.\s*(get|post|put|delete|patch)\s*\(");
        let tail = &text[whole.end()..(whole.end() + 600).min(text.len())];
        let mut at = 0;
        while let Some(x) = next.captures(&tail[at..]) {
            let open = at + x.get(0).unwrap().end() - 1;
            file.routes.push((
                x[1].to_uppercase(),
                c[2].to_string(),
                String::new(),
                line_of(text, whole.start()),
            ));
            match balanced(tail, open) {
                Some((close, _)) => at = close + 1,
                None => break,
            }
        }
    }
    for c in mount_re.captures_iter(text) {
        file.mounts.push((c[2].to_string(), c[3].to_string()));
    }
    static IMP: OnceLock<Regex> = OnceLock::new();
    for c in re(&IMP, r#"import\s+(\w+)\s+from\s+['"]([^'"]+)['"]|(?:const|let|var)\s+(\w+)\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)"#).captures_iter(text) {
        let (id, spec) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
            (Some(i), Some(s), _, _) | (_, _, Some(i), Some(s)) => (i.as_str(), s.as_str()),
            _ => continue,
        };
        file.imports.insert(id.to_string(), spec.to_string());
    }
    (!file.routes.is_empty() || !file.mounts.is_empty()).then_some(file)
}

/// The file an import points to, among the module's JS/TS files (relative paths and
/// aliases such as `@app/routes/x` or `~/routes/x` resolved by their ending).
fn resolve_import(from: &Path, spec: &str, files: &[PathBuf]) -> Option<PathBuf> {
    let without_ext = |p: &Path| p.with_extension("").to_string_lossy().replace('\\', "/");
    if spec.starts_with('.') {
        let base = from.parent()?.join(spec);
        let target = without_ext(&base);
        let target = normalize_dots(&target);
        return files
            .iter()
            .find(|f| {
                let f = without_ext(f);
                f == target || f == format!("{target}/index")
            })
            .cloned();
    }
    let tail = spec
        .split_once('/')
        .map(|(_, t)| t)
        .filter(|_| spec.starts_with('@') || spec.starts_with('~'))?;
    files
        .iter()
        .find(|f| {
            let f = without_ext(f);
            f.ends_with(&format!("/{tail}")) || f.ends_with(&format!("/{tail}/index"))
        })
        .cloned()
}

fn normalize_dots(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "." | "" if !out.is_empty() => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

fn express(module: &Path, files: &[(PathBuf, String)], out: &mut Vec<Endpoint>) {
    let parsed: Vec<(PathBuf, JsFile)> = files
        .iter()
        .filter_map(|(p, t)| express_file(t).map(|f| (p.clone(), f)))
        .collect();
    if parsed.is_empty() {
        return;
    }
    let all: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
    // Who mounts whom: child file → (parent file, prefix).
    let mut parent: HashMap<PathBuf, (PathBuf, String)> = HashMap::new();
    for (path, f) in &parsed {
        for (prefix, ident) in &f.mounts {
            if let Some(child) = f
                .imports
                .get(ident)
                .and_then(|spec| resolve_import(path, spec, &all))
            {
                parent
                    .entry(child)
                    .or_insert((path.clone(), prefix.clone()));
            }
        }
    }
    let prefix_of = |path: &PathBuf| {
        let mut parts = Vec::new();
        let mut current = path.clone();
        for _ in 0..10 {
            match parent.get(&current) {
                Some((p, prefix)) => {
                    parts.push(prefix.clone());
                    current = p.clone();
                }
                None => break,
            }
        }
        parts.reverse();
        parts
    };
    for (path, f) in &parsed {
        let prefixes = prefix_of(path);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let group_name = stem
            .trim_end_matches(".routes")
            .trim_end_matches(".router")
            .trim_end_matches("Routes")
            .trim_end_matches("Router")
            .trim_end_matches("routes")
            .trim_end_matches("router")
            .trim_end_matches(['.', '-', '_']);
        let group = if group_name.is_empty() || group_name == "index" {
            prefixes
                .last()
                .map(|p| humanize(p.trim_matches('/')))
                .unwrap_or_else(|| "Routes".into())
        } else {
            humanize(group_name)
        };
        let rel = path
            .strip_prefix(module)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for (method, route, name, line) in &f.routes {
            let mut parts: Vec<&str> = prefixes.iter().map(String::as_str).collect();
            parts.push(route);
            let full = normalize_params(&join_paths(&parts));
            out.push(Endpoint {
                method: method.clone(),
                name: if name.is_empty() {
                    format!("{method} {full}")
                } else {
                    humanize(name)
                },
                path: full,
                group: group.clone(),
                framework: "express".into(),
                at: format!("{rel}:{line}"),
                query: Vec::new(),
                headers: Vec::new(),
                body: matches!(method.as_str(), "POST" | "PUT" | "PATCH"),
            });
        }
    }
}

// --------------------------------------------------------------------- FastAPI

fn tag_of(args: &str) -> Option<String> {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    re(&TAGS, r#"tags\s*=\s*\[\s*["']([^"']+)"#)
        .captures(args)
        .map(|t| t[1].to_string())
}

fn fastapi(files: &[(PathBuf, String)], module: &Path, out: &mut Vec<Endpoint>) {
    static ROUTER: OnceLock<Regex> = OnceLock::new();
    static INCLUDE: OnceLock<Regex> = OnceLock::new();
    static ROUTE: OnceLock<Regex> = OnceLock::new();
    // Router variables and their prefixes, per file; include_router prefixes by module name.
    let mut included: HashMap<String, String> = HashMap::new();
    for (_, text) in files {
        for c in re(&INCLUDE, r"include_router\(\s*([\w.]+)\s*(?:,([^)]*))?\)").captures_iter(text)
        {
            let target = c[1].to_string();
            let prefix = c
                .get(2)
                .and_then(|a| named_or_first(a.as_str(), &["prefix"]))
                .unwrap_or_default();
            let key = target.split('.').next().unwrap_or(&target).to_string();
            included.insert(key, prefix);
        }
    }
    for (path, text) in files {
        if !text.contains("fastapi") && !text.contains("APIRouter") && !text.contains("FastAPI") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut routers: HashMap<String, (String, Option<String>)> = HashMap::new();
        for c in re(&ROUTER, r"(\w+)\s*=\s*(APIRouter|FastAPI)\(").captures_iter(text) {
            let open = c.get(0).unwrap().end() - 1;
            let args = balanced(text, open).map(|(_, a)| a).unwrap_or_default();
            let prefix = named_or_first(args, &["prefix"])
                .filter(|_| args.contains("prefix"))
                .unwrap_or_default();
            let tag = tag_of(args);
            routers.insert(c[1].to_string(), (prefix, tag));
        }
        let outer = included.get(&stem).cloned().unwrap_or_default();
        let rel = path
            .strip_prefix(module)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for c in re(&ROUTE, r"@(\w+)\.(get|post|put|delete|patch)\(").captures_iter(text) {
            let whole = c.get(0).unwrap();
            let Some((close, args)) = balanced(text, whole.end() - 1) else {
                continue;
            };
            let Some(route) = literals(args).into_iter().next() else {
                continue;
            };
            let (prefix, router_tag) = routers.get(&c[1]).cloned().unwrap_or_default();
            let tag = tag_of(args).or(router_tag);
            let summary = named_or_first(args, &["summary"]).filter(|_| args.contains("summary"));
            static DEF: OnceLock<Regex> = OnceLock::new();
            let rest = &text[close..];
            let (name, params) = match re(&DEF, r"def\s+(\w+)\s*\(").captures(rest) {
                Some(d) => {
                    let open = d.get(0).unwrap().end() - 1;
                    (
                        d[1].to_string(),
                        balanced(rest, open)
                            .map(|(_, p)| p.to_string())
                            .unwrap_or_default(),
                    )
                }
                None => (String::new(), String::new()),
            };
            let method = c[2].to_uppercase();
            let mut query = Vec::new();
            let mut has_body = false;
            for p in split_args(&params) {
                let p = p.trim();
                let pname = p.split(':').next().unwrap_or(p).trim();
                if p.contains("Query(...") {
                    query.push(pname.to_string());
                }
                let annotation = p
                    .split_once(':')
                    .map(|(_, t)| t.split('=').next().unwrap_or(t).trim())
                    .unwrap_or("");
                if matches!(method.as_str(), "POST" | "PUT" | "PATCH")
                    && annotation.chars().next().is_some_and(char::is_uppercase)
                    && ![
                        "Request",
                        "Response",
                        "Session",
                        "BackgroundTasks",
                        "Depends",
                    ]
                    .iter()
                    .any(|x| annotation.starts_with(x))
                {
                    has_body = true;
                }
            }
            let full = normalize_params(&join_paths(&[&outer, &prefix, &route]));
            out.push(Endpoint {
                method,
                name: summary.unwrap_or_else(|| humanize(&name)),
                path: full,
                group: tag.map(|t| humanize(&t)).unwrap_or_else(|| humanize(&stem)),
                framework: "fastapi".into(),
                at: format!("{rel}:{}", line_of(text, whole.start())),
                query,
                headers: Vec::new(),
                body: has_body,
            });
        }
    }
}

// --------------------------------------------------------------------- Next.js / Nuxt

fn file_routes(module: &Path, files: &[(PathBuf, String)], out: &mut Vec<Endpoint>) {
    static EXPORT: OnceLock<Regex> = OnceLock::new();
    static METHOD_CHECK: OnceLock<Regex> = OnceLock::new();
    for (path, text) in files {
        let rel = path
            .strip_prefix(module)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let segments: Vec<&str> = rel.split('/').collect();
        let param = |s: &str| {
            if s.starts_with("[...") || s.starts_with("[[...") {
                format!("{{{}}}", s.trim_matches(['[', ']', '.']))
            } else if s.starts_with('[') {
                format!("{{{}}}", s.trim_matches(['[', ']']))
            } else {
                s.to_string()
            }
        };
        let (framework, route, methods): (&str, String, Vec<String>) = if let Some(i) = segments
            .iter()
            .rposition(|s| *s == "app")
            .filter(|_| rel.ends_with("/route.ts") || rel.ends_with("/route.js"))
        {
            let route: Vec<String> = segments[i + 1..segments.len() - 1]
                .iter()
                .filter(|s| !(s.starts_with('(') || s.starts_with('@')))
                .map(|s| param(s))
                .collect();
            let methods = re(
                &EXPORT,
                r"export\s+(?:async\s+)?(?:function|const)\s+(GET|POST|PUT|PATCH|DELETE)\b",
            )
            .captures_iter(text)
            .map(|c| c[1].to_string())
            .collect();
            ("nextjs", format!("/{}", route.join("/")), methods)
        } else if let Some(i) = segments.windows(2).position(|w| w == ["pages", "api"]) {
            let mut route: Vec<String> = segments[i + 1..]
                .iter()
                .map(|s| param(s.split('.').next().unwrap_or(s)))
                .collect();
            if route.last().is_some_and(|l| l == "index") {
                route.pop();
            }
            let mut methods: Vec<String> =
                re(&METHOD_CHECK, r#"req\.method\s*===?\s*['"](\w+)['"]"#)
                    .captures_iter(text)
                    .map(|c| c[1].to_uppercase())
                    .collect();
            methods.dedup();
            if methods.is_empty() {
                methods.push("GET".into());
            }
            ("nextjs", format!("/{}", route.join("/")), methods)
        } else if let Some(i) = segments
            .windows(2)
            .position(|w| w[0] == "server" && (w[1] == "api" || w[1] == "routes"))
        {
            let mut route: Vec<String> = Vec::new();
            if segments[i + 1] == "api" {
                route.push("api".into());
            }
            let mut method = "GET".to_string();
            for (k, s) in segments[i + 2..].iter().enumerate() {
                let is_last = i + 2 + k == segments.len() - 1;
                if is_last {
                    let parts: Vec<&str> = s.split('.').collect();
                    if parts.len() >= 3 {
                        method = parts[parts.len() - 2].to_uppercase();
                    }
                    if parts[0] != "index" {
                        route.push(param(parts[0]));
                    }
                } else {
                    route.push(param(s));
                }
            }
            ("nuxt", format!("/{}", route.join("/")), vec![method])
        } else {
            continue;
        };
        let group = route
            .trim_start_matches("/api")
            .split('/')
            .find(|s| !s.is_empty() && !s.starts_with('{'))
            .map(humanize)
            .unwrap_or_else(|| "Api".into());
        for method in methods {
            if HttpMethod::from_str(&method).is_err() {
                continue;
            }
            out.push(Endpoint {
                name: format!("{method} {route}"),
                body: matches!(method.as_str(), "POST" | "PUT" | "PATCH"),
                method,
                path: route.clone(),
                group: group.clone(),
                framework: framework.into(),
                at: rel.clone(),
                query: Vec::new(),
                headers: Vec::new(),
            });
        }
    }
}

// --------------------------------------------------------------------- modules

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 12 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            let test_dir = matches!(name.as_str(), "test" | "tests" | "__tests__" | "e2e");
            if !SKIPPED_DIRS.contains(&name.as_str()) && !test_dir {
                walk(&path, depth + 1, out);
            }
        } else {
            out.push(path);
        }
    }
}

fn module_root(file: &Path, stop: &Path) -> PathBuf {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if MANIFESTS.iter().any(|m| d.join(m).is_file()) {
            return d.to_path_buf();
        }
        if d == stop {
            break;
        }
        dir = d.parent();
    }
    stop.to_path_buf()
}

fn base_url(module: &Path, framework: &str) -> String {
    let read = |p: PathBuf| fs::read_to_string(p).unwrap_or_default();
    let port_in = |text: &str, re_src: &str| {
        Regex::new(re_src)
            .ok()
            .and_then(|r| r.captures(text))
            .map(|c| c[1].to_string())
    };
    // Outside Spring, the port of the `.env` files wins over the default in the code.
    if framework != "spring" {
        let env_port = [".env", ".env.local", ".env.development", ".env.example"]
            .iter()
            .find_map(|f| {
                port_in(
                    &read(module.join(f)),
                    r"(?m)^\s*(?:export\s+)?PORT\s*=\s*['\x22]?(\d{2,5})",
                )
            });
        if let Some(port) = env_port {
            return format!("http://localhost:{port}");
        }
    }
    match framework {
        "spring" => {
            let resources = module.join("src/main/resources");
            let mut port = None;
            let mut context = String::new();
            for name in [
                "application.yml",
                "application.yaml",
                "application.properties",
            ] {
                let text = read(resources.join(name));
                if text.is_empty() {
                    continue;
                }
                port = port
                    .or_else(|| port_in(&text, r"server\.port\s*[=:]\s*\$?\{?[\w.]*:?(\d{2,5})"))
                    .or_else(|| {
                        port_in(
                            &text,
                            r"(?m)^server:\s*\n(?:\s+.*\n)*?\s+port:\s*\$?\{?[\w.]*:?(\d{2,5})",
                        )
                    });
                if let Some(c) = port_in(&text, r"context-path\s*[=:]\s*([/\w-]+)") {
                    context = c;
                }
            }
            format!(
                "http://localhost:{}{}",
                port.unwrap_or_else(|| "8080".into()),
                context.trim_end_matches('/')
            )
        }
        "fastapi" => "http://localhost:8000".into(),
        "nextjs" | "nuxt" => "http://localhost:3000".into(),
        _ => {
            let mut found = None;
            for (path, _) in source_files(module, &["ts", "js"]) {
                let text = read(path);
                found = port_in(&text, r"PORT\s*(?:\|\||\?\?)\s*['\x22]?(\d{2,5})")
                    .or_else(|| port_in(&text, r"\.listen\(\s*(\d{2,5})"));
                if found.is_some() {
                    break;
                }
            }
            format!(
                "http://localhost:{}",
                found.unwrap_or_else(|| "3000".into())
            )
        }
    }
}

fn source_files(dir: &Path, exts: &[&str]) -> Vec<(PathBuf, String)> {
    let mut paths = Vec::new();
    walk(dir, 0, &mut paths);
    paths
        .into_iter()
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            p.extension()
                .is_some_and(|e| exts.contains(&e.to_string_lossy().as_ref()))
                && !name.contains(".spec.")
                && !name.contains(".test.")
                && !name.ends_with(".d.ts")
                && !name.ends_with("Test.java")
                && !name.ends_with("Tests.java")
                && fs::metadata(p)
                    .map(|m| m.len() <= MAX_FILE)
                    .unwrap_or(false)
        })
        .filter_map(|p| fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect()
}

/// The modules (a directory with a build manifest) of `dir` that expose endpoints.
pub fn find_modules(dir: &Path) -> Vec<Module> {
    let files = source_files(dir, &["java", "kt", "ts", "js", "mjs", "py"]);
    let mut by_module: HashMap<PathBuf, Vec<(PathBuf, String)>> = HashMap::new();
    for (path, text) in files {
        by_module
            .entry(module_root(&path, dir))
            .or_default()
            .push((path, text));
    }
    let mut out = Vec::new();
    for (module, files) in by_module {
        let mut endpoints = Vec::new();
        for (path, text) in &files {
            let rel = path
                .strip_prefix(&module)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            match path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .as_deref()
            {
                Some("java" | "kt") => spring(&rel, text, &mut endpoints),
                Some("ts" | "js" | "mjs") if text.contains("@nestjs/") => {
                    nest(&rel, text, &mut endpoints)
                }
                _ => {}
            }
        }
        let js: Vec<(PathBuf, String)> = files
            .iter()
            .filter(|(p, t)| {
                p.extension()
                    .is_some_and(|e| e != "py" && e != "java" && e != "kt")
                    && !t.contains("@nestjs/")
            })
            .cloned()
            .collect();
        let routes_by_path: Vec<(PathBuf, String)> = js
            .iter()
            .filter(|(p, _)| {
                let s = p.to_string_lossy().replace('\\', "/");
                s.contains("/server/api/")
                    || s.contains("/server/routes/")
                    || s.contains("/pages/api/")
                    || s.ends_with("/route.ts")
                    || s.ends_with("/route.js")
            })
            .cloned()
            .collect();
        file_routes(&module, &routes_by_path, &mut endpoints);
        let others: Vec<(PathBuf, String)> = js
            .into_iter()
            .filter(|f| !routes_by_path.iter().any(|r| r.0 == f.0))
            .collect();
        express(&module, &others, &mut endpoints);
        let py: Vec<(PathBuf, String)> = files
            .iter()
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "py"))
            .cloned()
            .collect();
        fastapi(&py, &module, &mut endpoints);
        if endpoints.is_empty() {
            continue;
        }
        // The framework most endpoints come from names the module.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for e in &endpoints {
            *counts.entry(e.framework.as_str()).or_default() += 1;
        }
        let framework = counts
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .map(|(f, _)| f.to_string())
            .unwrap_or_default();
        endpoints.sort_by(|a, b| {
            (a.group.as_str(), a.path.as_str(), a.method.as_str()).cmp(&(
                b.group.as_str(),
                b.path.as_str(),
                b.method.as_str(),
            ))
        });
        endpoints.dedup_by(|a, b| a.method == b.method && a.path == b.path);
        let name = module
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "api".into());
        out.push(Module {
            base_url: base_url(&module, &framework),
            dir: module,
            name,
            framework,
            endpoints,
            warnings: Vec::new(),
        });
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

fn template(path: &str) -> String {
    path.split('/')
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') {
                format!("{{{{{}}}}}", &s[1..s.len() - 1])
            } else {
                s.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A module as an import plan: one folder per feature, `{{baseUrl}}` in an environment.
pub fn plan(module: &Module) -> ImportPlan {
    let mut groups: Vec<(String, Vec<ImportNode>)> = Vec::new();
    for e in &module.endpoints {
        let method = HttpMethod::from_str(&e.method).unwrap_or(HttpMethod::Get);
        let mut url = format!("{{{{baseUrl}}}}{}", template(&e.path));
        if !e.query.is_empty() {
            url.push('?');
            url.push_str(
                &e.query
                    .iter()
                    .map(|q| format!("{q}={{{{{q}}}}}"))
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }
        let mut request = HttpRequest::new(method, url);
        request.headers = e
            .headers
            .iter()
            .map(|h| Header::new(h.clone(), format!("{{{{{h}}}}}")))
            .collect();
        if e.body {
            request
                .headers
                .push(Header::new("Content-Type", "application/json"));
            request.body = Some("{}".into());
        }
        let node = ImportNode::Request {
            name: e.name.clone(),
            request,
            scripts: Scripts {
                pre: String::new(),
                test: format!("// From the code: {} ({})\n", e.at, e.framework),
            },
        };
        match groups.iter_mut().find(|(g, _)| *g == e.group) {
            Some((_, nodes)) => nodes.push(node),
            None => groups.push((e.group.clone(), vec![node])),
        }
    }
    ImportPlan {
        format: ImportFormat::Code,
        folder: Some(module.name.clone()),
        folder_scripts: Scripts::default(),
        folder_auth: None,
        nodes: groups
            .into_iter()
            .map(|(name, children)| ImportNode::Folder {
                name,
                children,
                scripts: Scripts::default(),
                auth: None,
            })
            .collect(),
        environments: vec![Environment {
            id: String::new(),
            name: format!("{} local", module.name),
            variables: vec![Variable {
                name: "baseUrl".into(),
                value: module.base_url.clone(),
                secret: false,
                has_value: false,
            }],
        }],
        warnings: module.warnings.clone(),
    }
}

/// The plan of the module at `dir` (the directory itself or its only module).
pub fn plan_dir(dir: &Path) -> Option<ImportPlan> {
    let modules = find_modules(dir);
    let module = modules
        .iter()
        .find(|m| m.dir == dir)
        .or_else(|| (modules.len() == 1).then(|| &modules[0]))?;
    Some(plan(module))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn humanizes_names() {
        assert_eq!(humanize("getUserById"), "Get user by id");
        assert_eq!(humanize("list_orders"), "List orders");
        assert_eq!(humanize("AccountConsents"), "Account consents");
        assert_eq!(humanize("parseHTTPHeader"), "Parse HTTP header");
    }

    #[test]
    fn reads_spring_controllers() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "consents/pom.xml", "<project/>");
        write(
            dir.path(),
            "consents/src/main/resources/application.yml",
            "server:\n  port: ${PORT:8085}\n  servlet:\n    context-path: /svc\n",
        );
        write(
            dir.path(),
            "consents/src/main/java/x/AccountConsentsController.java",
            r#"
@RestController
@RequestMapping("/api/v1/consents")
@Tag(name = "Cuenta", description = "x")
public class AccountConsentsController {
    @Operation(summary = "Sesión de aceptación")
    @PostMapping("/sessions")
    public SessionView session() { return null; }

    @GetMapping("/documents/{code}")
    public StatusView status(@PathVariable String code, @RequestParam(required = false) String lang,
                             @RequestParam("page") int page,
                             @RequestHeader(value = "X-Tenant") String tenant) { return null; }

    @RequestMapping(value = "/acceptances", method = RequestMethod.PUT)
    public void accept(@Valid @RequestBody AcceptRequest request) {}

    @GetMapping(ApiPaths.SECRET)
    public void hidden() {}
}
"#,
        );
        write(
            dir.path(),
            "consents/src/test/java/x/FakeController.java",
            "@RestController class FakeController { @GetMapping(\"/nope\") void x() {} }",
        );
        let modules = find_modules(dir.path());
        assert_eq!(modules.len(), 1, "{modules:#?}");
        let m = &modules[0];
        assert_eq!(m.framework, "spring");
        assert_eq!(m.base_url, "http://localhost:8085/svc");
        let e: Vec<(String, String, String)> = m
            .endpoints
            .iter()
            .map(|e| (e.method.clone(), e.path.clone(), e.name.clone()))
            .collect();
        assert_eq!(
            e,
            vec![
                (
                    "PUT".into(),
                    "/api/v1/consents/acceptances".into(),
                    "Accept".into()
                ),
                (
                    "GET".into(),
                    "/api/v1/consents/documents/{code}".into(),
                    "Status".into()
                ),
                (
                    "POST".into(),
                    "/api/v1/consents/sessions".into(),
                    "Sesión de aceptación".into()
                ),
            ]
        );
        assert!(m.endpoints.iter().all(|e| e.group == "Cuenta"));
        let status = &m.endpoints[1];
        assert_eq!(status.query, vec!["page".to_string()]);
        assert_eq!(status.headers, vec!["X-Tenant".to_string()]);
        assert!(m.endpoints[0].body);
        assert!(
            status.at.ends_with("AccountConsentsController.java:10"),
            "{}",
            status.at
        );
    }

    #[test]
    fn names_a_spring_method_from_an_operation_after_its_mapping() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "pay/pom.xml", "<project/>");
        write(
            dir.path(),
            "pay/src/main/java/x/MethodsController.java",
            "@RestController\n@RequestMapping(\"/methods\")\nclass MethodsController {\n  @GetMapping\n  @Operation(summary = \"First\")\n  List<X> list() { return null; }\n  @PutMapping\n  @Operation(summary = \"Second\")\n  void replace(@RequestBody X x, @RequestParam String code) {}\n}\n",
        );
        let m = &find_modules(dir.path())[0];
        let names: Vec<(String, String)> = m
            .endpoints
            .iter()
            .map(|e| (e.method.clone(), e.name.clone()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("GET".into(), "First".into()),
                ("PUT".into(), "Second".into())
            ]
        );
        assert_eq!(m.endpoints[1].query, vec!["code".to_string()]);
        assert!(m.endpoints[1].body);
    }

    #[test]
    fn follows_express_mounts_across_files() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "svc/package.json", "{}");
        write(r, "svc/src/server.ts", "import express from 'express';\nimport v1Routes from '@svc/interfaces/routes/v1Routes';\nconst app = express();\napp.use(express.json());\napp.use('/api/v1', v1Routes);\napp.get('/health', (req, res) => res.send('ok'));\nconst PORT = process.env.PORT || 4100;\n");
        write(r, "svc/src/interfaces/routes/v1Routes.ts", "import { Router } from 'express';\nimport buyerRoutes from './buyerRoutes';\nconst router = Router();\nrouter.use('/buyer', buyerRoutes);\nexport default router;\n");
        write(r, "svc/src/interfaces/routes/buyerRoutes.ts", "import express from 'express';\nconst router = express.Router();\nrouter.get('/orders/:orderId/shipment', requireBuyer, (req, res) => controller.get(req, res));\nrouter.post('/orders/:orderId/confirm', confirmOrder);\nconst x = axios.get('/not/a/route');\nexport default router;\n");
        let modules = find_modules(r);
        let m = &modules[0];
        assert_eq!(m.framework, "express");
        assert_eq!(m.base_url, "http://localhost:4100");
        write(
            r,
            "svc/.env.example",
            "NODE_ENV=dev\nPORT=3333\nDB_PORT=5432\n",
        );
        assert_eq!(find_modules(r)[0].base_url, "http://localhost:3333");
        let e: Vec<(String, String, String, String)> = m
            .endpoints
            .iter()
            .map(|e| {
                (
                    e.method.clone(),
                    e.path.clone(),
                    e.name.clone(),
                    e.group.clone(),
                )
            })
            .collect();
        assert!(
            e.contains(&(
                "GET".into(),
                "/api/v1/buyer/orders/{orderId}/shipment".into(),
                "Get".into(),
                "Buyer".into()
            )),
            "{e:#?}"
        );
        assert!(e.contains(&(
            "POST".into(),
            "/api/v1/buyer/orders/{orderId}/confirm".into(),
            "Confirm order".into(),
            "Buyer".into()
        )));
        assert!(e.iter().any(|x| x.1 == "/health"));
        assert!(!e.iter().any(|x| x.1.contains("not/a/route")));
    }

    #[test]
    fn reads_nest_fastapi_next_and_nuxt() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "nest/package.json", "{}");
        write(r, "nest/src/users.controller.ts", "import { Controller, Get, Post, Body, Param } from '@nestjs/common';\n@Controller('users')\nexport class UsersController {\n  @Get(':id')\n  findOne(@Param('id') id: string) {}\n  @Post()\n  @HttpCode(201)\n  async create(@Body() dto: CreateUserDto) {}\n}\n");
        write(r, "py/pyproject.toml", "");
        write(r, "py/app/main.py", "from fastapi import FastAPI\nfrom .routers import items\napp = FastAPI()\napp.include_router(items.router, prefix=\"/api\")\n@app.get(\"/health\")\ndef health():\n    return {}\n");
        write(r, "py/app/routers/items.py", "from fastapi import APIRouter, Query\nrouter = APIRouter(prefix=\"/items\", tags=[\"items\"])\n@router.get(\"/{item_id}\", summary=\"Read an item\")\nasync def read_item(item_id: int, q: str = Query(...)):\n    return {}\n@router.post(\"/\")\nasync def create_item(item: Item):\n    return item\n");
        write(r, "web/package.json", "{}");
        write(
            r,
            "web/app/api/users/[id]/route.ts",
            "export async function GET(req) {}\nexport async function DELETE(req) {}\n",
        );
        write(
            r,
            "web/app/(marketing)/page.tsx",
            "export default function Page() {}",
        );
        write(r, "shop/package.json", "{}");
        write(
            r,
            "shop/server/api/tenants/search.get.ts",
            "export default defineEventHandler(() => [])",
        );
        write(
            r,
            "shop/server/api/auth/login.post.ts",
            "export default defineEventHandler(() => ({}))",
        );
        let modules = find_modules(r);
        let get = |name: &str| modules.iter().find(|m| m.name == name).unwrap();
        let nest = get("nest");
        assert_eq!(nest.framework, "nestjs");
        let ne: Vec<(String, String, bool)> = nest
            .endpoints
            .iter()
            .map(|e| (e.method.clone(), e.path.clone(), e.body))
            .collect();
        assert!(
            ne.contains(&("GET".into(), "/users/{id}".into(), false)),
            "{ne:?}"
        );
        assert!(ne.contains(&("POST".into(), "/users".into(), true)));
        assert_eq!(nest.endpoints[0].group, "Users");
        let py = get("py");
        let pe: Vec<(String, String, String)> = py
            .endpoints
            .iter()
            .map(|e| (e.method.clone(), e.path.clone(), e.name.clone()))
            .collect();
        assert!(
            pe.contains(&(
                "GET".into(),
                "/api/items/{item_id}".into(),
                "Read an item".into()
            )),
            "{pe:?}"
        );
        assert!(pe.contains(&("POST".into(), "/api/items".into(), "Create item".into())));
        assert!(pe.contains(&("GET".into(), "/health".into(), "Health".into())));
        let read = py
            .endpoints
            .iter()
            .find(|e| e.path == "/api/items/{item_id}")
            .unwrap();
        assert_eq!(read.query, vec!["q".to_string()]);
        assert_eq!(read.group, "Items");
        assert!(
            py.endpoints
                .iter()
                .find(|e| e.method == "POST")
                .unwrap()
                .body
        );
        let web = get("web");
        let we: Vec<(String, String)> = web
            .endpoints
            .iter()
            .map(|e| (e.method.clone(), e.path.clone()))
            .collect();
        assert_eq!(
            we,
            vec![
                ("DELETE".into(), "/api/users/{id}".into()),
                ("GET".into(), "/api/users/{id}".into())
            ]
        );
        let shop = get("shop");
        let se: Vec<(String, String)> = shop
            .endpoints
            .iter()
            .map(|e| (e.method.clone(), e.path.clone()))
            .collect();
        assert!(
            se.contains(&("GET".into(), "/api/tenants/search".into())),
            "{se:?}"
        );
        assert!(se.contains(&("POST".into(), "/api/auth/login".into())));

        let plan = plan(nest);
        assert_eq!(plan.format, ImportFormat::Code);
        let ImportNode::Folder { name, children, .. } = &plan.nodes[0] else {
            panic!()
        };
        assert_eq!(name, "Users");
        let ImportNode::Request { request, .. } = &children[0] else {
            panic!()
        };
        assert!(
            request.url.starts_with("{{baseUrl}}/users"),
            "{}",
            request.url
        );
        assert_eq!(
            plan.environments[0].variables[0].value,
            "http://localhost:3000"
        );
    }
}
