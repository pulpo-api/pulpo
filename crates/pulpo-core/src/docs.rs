//! Documentation of a collection as one self-contained HTML page (no server, no CDN),
//! ready for GitHub Pages: a tree to browse, a search box, and for each request what it
//! sends (method, URL, auth, headers, body or operation), the tests it runs and the
//! responses saved as examples. Variables stay as `{{name}}`: values never go in.

use crate::collection::{RequestEntry, TreeNode};
use crate::export::Inherited;
use crate::http::Auth;
use regex::Regex;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::OnceLock;

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn anchor(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn auth_label(auth: &Auth) -> &'static str {
    match auth {
        Auth::None => "none",
        Auth::Bearer { .. } => "Bearer token",
        Auth::Basic { .. } => "Basic",
        Auth::ApiKey { .. } => "API key",
        Auth::OAuth2(_) => "OAuth 2.0",
        Auth::Digest(_) => "Digest",
        Auth::AwsV4(_) => "AWS Signature v4",
        Auth::Hawk(_) => "Hawk",
        Auth::Ntlm(_) => "NTLM",
    }
}

/// `pm.test("name", …)` names of a test script.
fn test_names(script: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"pm\.test\(\s*["'`]([^"'`]+)["'`]"#).expect("valid regex"))
        .captures_iter(script)
        .map(|c| c[1].to_string())
        .collect()
}

fn pretty(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body.trim()) {
        Ok(v) if body.trim_start().starts_with(['{', '[']) => {
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string())
        }
        _ => body.to_string(),
    }
}

fn badge(entry: &RequestEntry) -> (String, String) {
    let r = &entry.request;
    if r.mqtt.is_some() || crate::mqtt::is_mqtt_url(&r.url) {
        return ("MQTT".into(), "p".into());
    }
    if r.socketio.is_some() {
        return ("SIO".into(), "p".into());
    }
    if crate::websocket::is_websocket_url(&r.url) {
        return ("WS".into(), "p".into());
    }
    if crate::grpc::is_grpc_url(&r.url) {
        return ("gRPC".into(), "p".into());
    }
    if r.graphql.is_some() {
        return ("GQL".into(), "p".into());
    }
    if r.soap.is_some() {
        return ("SOAP".into(), "p".into());
    }
    if crate::sse::is_event_stream(r) {
        return ("SSE".into(), "p".into());
    }
    let m = r.method.to_string();
    (m.clone(), m.to_lowercase())
}

struct Page<'a> {
    root: &'a Path,
    nav: String,
    main: String,
    count: usize,
}

impl Page<'_> {
    fn walk(&mut self, nodes: &[TreeNode], inherited: &Inherited, depth: usize) {
        for node in nodes {
            match node {
                TreeNode::Folder {
                    id,
                    name,
                    children,
                    auth,
                    headers,
                    ..
                } => {
                    let _ = write!(self.nav, "<details open><summary>{}</summary>", esc(name));
                    let _ = write!(
                        self.main,
                        "<h{h} class=\"folder\" id=\"f-{a}\">{n}</h{h}>",
                        h = (depth + 2).min(4),
                        a = anchor(id),
                        n = esc(name)
                    );
                    self.walk(children, &inherited.enter(name, auth, headers), depth + 1);
                    self.nav.push_str("</details>");
                }
                TreeNode::Request(entry) => self.request(entry, inherited),
            }
        }
    }

    fn request(&mut self, entry: &RequestEntry, inherited: &Inherited) {
        self.count += 1;
        let r = &entry.request;
        let (label, class) = badge(entry);
        let a = format!("r-{}", anchor(&entry.id));
        let _ = write!(
            self.nav,
            "<a href=\"#{a}\" data-search=\"{s}\"><span class=\"m {class}\">{label}</span>{n}</a>",
            s = esc(&format!("{} {} {}", label, entry.name, r.url).to_lowercase()),
            n = esc(&entry.name)
        );
        let (auth, headers) = inherited.apply(r);
        let mut s = String::new();
        let _ = write!(
            s,
            "<section class=\"req\" id=\"{a}\" data-search=\"{search}\"><h3><span class=\"m {class}\">{label}</span>{name}</h3><pre class=\"url\">{url}</pre>",
            search = esc(&format!("{} {} {}", label, entry.name, r.url).to_lowercase()),
            name = esc(&entry.name),
            url = esc(&r.url)
        );
        if let Some(auth) = auth.as_ref().filter(|a| !matches!(a, Auth::None)) {
            let _ = write!(s, "<p class=\"meta\">Auth: <b>{}</b></p>", auth_label(auth));
        }
        let enabled: Vec<_> = headers
            .iter()
            .filter(|h| h.enabled && !h.name.trim().is_empty())
            .collect();
        if !enabled.is_empty() {
            s.push_str("<h4>Headers</h4><table>");
            for h in enabled {
                let _ = write!(
                    s,
                    "<tr><td>{}</td><td><code>{}</code></td></tr>",
                    esc(&h.name),
                    esc(&h.value)
                );
            }
            s.push_str("</table>");
        }
        if !r.path_params.is_empty() {
            s.push_str("<h4>Path parameters</h4><table>");
            for p in &r.path_params {
                let _ = write!(
                    s,
                    "<tr><td>:{}</td><td><code>{}</code></td></tr>",
                    esc(&p.name),
                    esc(&p.value)
                );
            }
            s.push_str("</table>");
        }
        // What the request sends, by protocol.
        if let Some(g) = &r.graphql {
            let _ = write!(s, "<h4>GraphQL</h4><pre>{}</pre>", esc(&g.query));
            if !g.variables.trim().is_empty() {
                let _ = write!(
                    s,
                    "<h4>Variables</h4><pre>{}</pre>",
                    esc(&pretty(&g.variables))
                );
            }
        } else if let Some(g) = &r.grpc {
            let _ = write!(
                s,
                "<p class=\"meta\">Method: <code>{}</code></p>",
                esc(&g.method)
            );
        } else if let Some(t) = &r.mqtt {
            if !t.subscribe.is_empty() {
                let _ = write!(
                    s,
                    "<p class=\"meta\">Subscribes to: <code>{}</code></p>",
                    esc(&t.subscribe.join(", "))
                );
            }
        } else if let Some(t) = &r.socketio {
            let _ = write!(
                s,
                "<p class=\"meta\">Namespace: <code>{}</code></p>",
                esc(&t.namespace)
            );
        } else if let Some(t) = &r.soap {
            let _ = write!(
                s,
                "<p class=\"meta\">Operation: <code>{}</code></p>",
                esc(&t.operation)
            );
        }
        if let Some(form) = &r.form {
            s.push_str("<h4>Form</h4><table>");
            for f in form.fields.iter().filter(|f| f.enabled) {
                let value = if f.file {
                    format!("file: {}", f.value)
                } else {
                    f.value.clone()
                };
                let _ = write!(
                    s,
                    "<tr><td>{}</td><td><code>{}</code></td></tr>",
                    esc(&f.name),
                    esc(&value)
                );
            }
            s.push_str("</table>");
        } else if let Some(file) = r.body_file.as_deref().filter(|f| !f.is_empty()) {
            let _ = write!(
                s,
                "<p class=\"meta\">Body: the file <code>{}</code></p>",
                esc(file)
            );
        } else if let Some(body) = r.body.as_deref().filter(|b| !b.trim().is_empty()) {
            let title = if r.grpc.is_some() {
                "Message"
            } else if r.soap.is_some() {
                "Envelope"
            } else {
                "Body"
            };
            let _ = write!(s, "<h4>{title}</h4><pre>{}</pre>", esc(&pretty(body)));
        }
        if !r.messages.is_empty() {
            let title = if r.mqtt.is_some() {
                "Publications"
            } else if r.socketio.is_some() {
                "Events"
            } else {
                "Messages"
            };
            let _ = write!(s, "<h4>{title}</h4>");
            for m in &r.messages {
                let _ = write!(
                    s,
                    "<p class=\"meta\"><b>{}</b></p><pre>{}</pre>",
                    esc(&m.name),
                    esc(&pretty(&m.body))
                );
            }
        }
        let tests = test_names(&entry.scripts.test);
        if !tests.is_empty() {
            s.push_str("<h4>Tests</h4><ul class=\"tests\">");
            for t in tests {
                let _ = write!(s, "<li>{}</li>", esc(&t));
            }
            s.push_str("</ul>");
        }
        let examples = crate::examples::list_examples(self.root, &entry.id).unwrap_or_default();
        if !examples.is_empty() {
            s.push_str("<h4>Responses</h4>");
            for (i, e) in examples.iter().enumerate() {
                let ok = if (200..400).contains(&e.status) {
                    "ok"
                } else {
                    "bad"
                };
                let _ = write!(
                    s,
                    "<details{open}><summary><span class=\"st {ok}\">{}</span> {}</summary><pre>{}</pre></details>",
                    e.status,
                    esc(&e.name),
                    esc(&pretty(&e.body)),
                    open = if i == 0 { " open" } else { "" }
                );
            }
        }
        s.push_str("</section>");
        self.main.push_str(&s);
    }
}

const STYLE: &str = r#"
:root{--bg:#fbfbfa;--fg:#1d1f23;--muted:#6b7079;--line:#e4e4e1;--card:#fff;--code:#f2f2ef;--accent:#e2553f;--get:#1f9d68;--post:#d9822b;--put:#2f7de1;--del:#e0464f;--p:#8a5cf6}
@media (prefers-color-scheme:dark){:root{--bg:#15171b;--fg:#e8e9ec;--muted:#9aa0aa;--line:#2b2f36;--card:#1c1f24;--code:#23272e}}
*{box-sizing:border-box}body{margin:0;font:14px/1.55 system-ui,-apple-system,Segoe UI,sans-serif;background:var(--bg);color:var(--fg)}
.layout{display:grid;grid-template-columns:300px minmax(0,1fr);min-height:100vh}
nav{border-right:1px solid var(--line);padding:16px;position:sticky;top:0;height:100vh;overflow:auto}
nav h1{font-size:17px;margin:0 0 8px}nav input{width:100%;padding:6px 8px;border:1px solid var(--line);border-radius:6px;background:var(--card);color:var(--fg);margin-bottom:10px}
nav details{margin-left:8px}nav summary{cursor:pointer;color:var(--muted);font-weight:600;margin:4px 0}
nav a{display:flex;gap:6px;align-items:baseline;color:var(--fg);text-decoration:none;padding:2px 4px;border-radius:4px}nav a:hover{background:var(--code)}
main{padding:24px 32px;max-width:980px}.intro{color:var(--muted)}
.folder{margin:28px 0 8px;border-bottom:1px solid var(--line);padding-bottom:4px}
.req{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:14px 18px;margin:14px 0}
.req h3{margin:0 0 6px;display:flex;gap:8px;align-items:baseline}.req h4{margin:12px 0 4px;font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted)}
pre{background:var(--code);padding:8px 10px;border-radius:6px;overflow:auto;font:12px/1.5 ui-monospace,Consolas,monospace;margin:4px 0;white-space:pre-wrap;word-break:break-word}
.url{font-weight:600}code{font:12px ui-monospace,Consolas,monospace}
table{border-collapse:collapse;font-size:13px}td{border-top:1px solid var(--line);padding:3px 12px 3px 0;vertical-align:top}
.meta{margin:4px 0;color:var(--muted)}.tests{margin:0;padding-left:18px}
.m{font:700 11px ui-monospace,Consolas,monospace;min-width:38px;color:var(--p)}.get{color:var(--get)}.post{color:var(--post)}.put,.patch{color:var(--put)}.delete{color:var(--del)}
.st{font:700 12px ui-monospace,Consolas,monospace}.st.ok{color:var(--get)}.st.bad{color:var(--del)}
details>summary{cursor:pointer}.hidden{display:none!important}
footer{color:var(--muted);font-size:12px;margin:32px 0}
@media (max-width:760px){.layout{grid-template-columns:1fr}nav{position:static;height:auto;border-right:0;border-bottom:1px solid var(--line)}main{padding:16px}}
"#;

const SCRIPT: &str = r#"
const q=document.getElementById('q');
q.addEventListener('input',()=>{const t=q.value.trim().toLowerCase();
document.querySelectorAll('[data-search]').forEach(e=>e.classList.toggle('hidden',!!t&&!e.dataset.search.includes(t)));});
"#;

/// The documentation page of `nodes` (a folder's or the whole workspace's).
pub(crate) fn render(
    root: &Path,
    title: &str,
    nodes: &[TreeNode],
    top: &Inherited,
    count: &mut usize,
) -> String {
    let mut page = Page {
        root,
        nav: String::new(),
        main: String::new(),
        count: 0,
    };
    page.walk(nodes, top, 0);
    *count = page.count;
    let environments: Vec<String> =
        crate::environment::list_environments(root, &crate::environment::MemoryStore::default())
            .unwrap_or_default()
            .into_iter()
            .map(|e| {
                let names: Vec<String> = e
                    .variables
                    .iter()
                    .map(|v| format!("<code>{{{{{}}}}}</code>", esc(&v.name)))
                    .collect();
                format!("<li><b>{}</b>: {}</li>", esc(&e.name), names.join(" "))
            })
            .collect();
    let env_block = if environments.is_empty() {
        String::new()
    } else {
        format!(
            "<h4>Environments</h4><p class=\"intro\">Values are not published; ask the team for them.</p><ul>{}</ul>",
            environments.join("")
        )
    };
    let date = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let days = secs / 86_400;
        let (y, m, d) = civil(days as i64);
        format!("{y:04}-{m:02}-{d:02}")
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{t}</title><style>{STYLE}</style></head><body><div class=\"layout\"><nav><h1>{t}</h1><input id=\"q\" type=\"search\" placeholder=\"Search requests\" aria-label=\"Search requests\">{nav}</nav><main><h2>{t}</h2><p class=\"intro\">{n} requests. Variables like <code>{{{{baseUrl}}}}</code> come from your environment.</p>{env}{main}<footer>Made with Pulpo from the collection on {date}.</footer></main></div><script>{SCRIPT}</script></body></html>\n",
        t = esc(title),
        n = page.count,
        nav = page.nav,
        env = env_block,
        main = page.main,
    )
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tests_and_dates() {
        assert_eq!(
            test_names("pm.test(\"is 200\", () => {});\npm.test('has id', fn);"),
            ["is 200", "has id"]
        );
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_407), (2025, 11, 15));
        assert_eq!(esc("<a href=\"x\">&"), "&lt;a href=&quot;x&quot;&gt;&amp;");
    }

    #[test]
    fn documents_requests_tests_and_examples_without_values() {
        use crate::collection::{init_workspace, save_request, RequestEntry, Scripts};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_workspace(root, "Shop <API>").unwrap();
        let mut request =
            crate::http::HttpRequest::new(crate::HttpMethod::Post, "{{baseUrl}}/orders");
        request.body = Some("{\"item\":\"ink\"}".into());
        request.auth = Some(Auth::Bearer {
            token: "{{token}}".into(),
        });
        save_request(
            root,
            &RequestEntry {
                id: "01-create.pulpo.yaml".into(),
                name: "Create order".into(),
                protocol: crate::Protocol::Http,
                request,
                scripts: Scripts {
                    pre: String::new(),
                    test: "pm.test(\"is 201\", () => {});".into(),
                },
            },
        )
        .unwrap();
        crate::examples::save_example(
            root,
            "01-create.pulpo.yaml",
            &crate::examples::Example {
                id: String::new(),
                name: "Created".into(),
                status: 201,
                headers: vec![],
                body: "{\"id\":8}".into(),
            },
        )
        .unwrap();
        std::fs::create_dir_all(root.join("environments")).unwrap();
        std::fs::write(
            root.join("environments/staging.yaml"),
            "version: 1
name: \"Staging\"
variables:
  - name: \"baseUrl\"
    value: \"https://internal.example.com\"
",
        )
        .unwrap();
        let out = crate::export::export(root, "", crate::export::ExportFormat::Html).unwrap();
        assert_eq!(out.file_name, "shop-api.html");
        assert_eq!(out.requests, 1);
        let html = out.text;
        assert!(html.contains("<title>Shop &lt;API&gt;</title>"));
        assert!(html.contains("{{baseUrl}}/orders"));
        assert!(html.contains("Auth: <b>Bearer token</b>"));
        assert!(html.contains("<li>is 201</li>"));
        assert!(html.contains("201</span> Created"));
        assert!(
            html.contains("&quot;item&quot;: &quot;ink&quot;"),
            "pretty JSON body"
        );
        assert!(
            html.contains("<code>{{baseUrl}}</code>"),
            "variable names are listed"
        );
        assert!(
            !html.contains("internal.example.com"),
            "values are never published"
        );
    }
}
