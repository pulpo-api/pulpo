//! What changed in a collection between two versions, request by request ("the URL of
//! Create order changed"), not line by line. Used by `pulpo diff` and the git panel.

use crate::collection::{self, RequestEntry, TreeNode};
use crate::environment::{self, MemoryStore};
use crate::git;
use crate::http::{Auth, Header, HttpRequest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub name: String,
    /// Folder names, outermost first.
    pub folder: Vec<String>,
    pub method: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub item: Item,
    /// Set when the request moved or was renamed: where it was.
    pub was: Option<Item>,
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDiff {
    pub from: String,
    pub to: String,
    pub added: Vec<Item>,
    pub removed: Vec<Item>,
    pub changed: Vec<Changed>,
    /// Folder (and workspace) auth, headers and settings; one line each.
    pub folders: Vec<String>,
    /// Environments and their variables; one line each.
    pub environments: Vec<String>,
}

impl CollectionDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self.folders.is_empty()
            && self.environments.is_empty()
    }
}

struct Loaded {
    requests: BTreeMap<String, (RequestEntry, Vec<String>)>,
    folders: BTreeMap<String, (String, Option<Auth>, Vec<Header>, String)>,
    environments: Vec<environment::Environment>,
}

fn load(root: &Path) -> Result<Loaded, String> {
    let workspace = collection::load_workspace(root).map_err(|e| e.to_string())?;
    let mut loaded = Loaded {
        requests: BTreeMap::new(),
        folders: BTreeMap::new(),
        environments: environment::list_environments(root, &MemoryStore::default())
            .unwrap_or_default(),
    };
    loaded.folders.insert(
        String::new(),
        (
            "the workspace".into(),
            workspace.auth.clone(),
            workspace.headers.clone(),
            format!("{:?}", workspace.settings),
        ),
    );
    fn walk(nodes: &[TreeNode], path: &[String], loaded: &mut Loaded) {
        for node in nodes {
            match node {
                TreeNode::Folder {
                    id,
                    name,
                    children,
                    auth,
                    headers,
                    settings,
                } => {
                    let mut inner = path.to_vec();
                    inner.push(name.clone());
                    loaded.folders.insert(
                        id.clone(),
                        (
                            format!("folder {}", inner.join(" / ")),
                            auth.clone(),
                            headers.clone(),
                            format!("{settings:?}"),
                        ),
                    );
                    walk(children, &inner, loaded);
                }
                TreeNode::Request(entry) => {
                    loaded
                        .requests
                        .insert(entry.id.clone(), (entry.clone(), path.to_vec()));
                }
            }
        }
    }
    walk(&workspace.tree, &[], &mut loaded);
    Ok(loaded)
}

fn item(entry: &RequestEntry, folder: &[String]) -> Item {
    Item {
        id: entry.id.clone(),
        name: entry.name.clone(),
        folder: folder.to_vec(),
        method: entry.request.method.to_string(),
        url: entry.request.url.clone(),
    }
}

fn short(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 60 {
        format!("{}…", one_line.chars().take(60).collect::<String>())
    } else {
        one_line
    }
}

fn auth_line(auth: &Option<Auth>) -> String {
    match auth {
        None => "inherited".into(),
        Some(a) => a.label().to_string(),
    }
}

fn header_changes(old: &[Header], new: &[Header], out: &mut Vec<String>) {
    for h in new {
        match old.iter().find(|o| o.name.eq_ignore_ascii_case(&h.name)) {
            None => out.push(format!("header {} added", h.name)),
            Some(o) if o.value != h.value => out.push(format!(
                "header {}: `{}` → `{}`",
                h.name,
                short(&o.value),
                short(&h.value)
            )),
            Some(o) if o.enabled != h.enabled => out.push(format!(
                "header {} {}",
                h.name,
                if h.enabled { "enabled" } else { "disabled" }
            )),
            _ => {}
        }
    }
    for o in old {
        if !new.iter().any(|h| h.name.eq_ignore_ascii_case(&o.name)) {
            out.push(format!("header {} removed", o.name));
        }
    }
}

fn request_changes(old: &RequestEntry, new: &RequestEntry) -> Vec<String> {
    let mut out = Vec::new();
    let (a, b): (&HttpRequest, &HttpRequest) = (&old.request, &new.request);
    if a.method != b.method {
        out.push(format!("method {} → {}", a.method, b.method));
    }
    if a.url != b.url {
        out.push(format!("URL `{}` → `{}`", a.url, b.url));
    }
    header_changes(&a.headers, &b.headers, &mut out);
    if a.disabled_params != b.disabled_params || a.path_params != b.path_params {
        out.push("params changed".into());
    }
    if a.body != b.body || a.form != b.form || a.body_file != b.body_file {
        let kind = |r: &HttpRequest| {
            if r.form.is_some() {
                "form"
            } else if r.body_file.is_some() {
                "file"
            } else if r.body.as_deref().is_some_and(|b| !b.is_empty()) {
                "raw"
            } else {
                "none"
            }
        };
        if kind(a) != kind(b) {
            out.push(format!("body {} → {}", kind(a), kind(b)));
        } else {
            out.push("body changed".into());
        }
    }
    if a.auth != b.auth {
        let (x, y) = (auth_line(&a.auth), auth_line(&b.auth));
        if x == y {
            out.push(format!("{y} auth settings changed"));
        } else {
            out.push(format!("auth {x} → {y}"));
        }
    }
    if a.settings != b.settings || a.cookies != b.cookies {
        out.push("settings changed".into());
    }
    if old.scripts.pre != new.scripts.pre {
        out.push("pre-request script changed".into());
    }
    if old.scripts.test != new.scripts.test {
        out.push("test script changed".into());
    }
    out
}

/// Compares two workspace folders.
pub fn compare(from: &Path, to: &Path) -> Result<CollectionDiff, String> {
    let old = load(from)?;
    let new = load(to)?;
    let mut diff = CollectionDiff::default();

    let mut removed: Vec<&(RequestEntry, Vec<String>)> = Vec::new();
    for (id, before) in &old.requests {
        match new.requests.get(id) {
            Some(after) => {
                let mut changes = request_changes(&before.0, &after.0);
                if before.0.name != after.0.name {
                    changes.insert(0, format!("renamed from {}", before.0.name));
                }
                if !changes.is_empty() {
                    diff.changed.push(Changed {
                        item: item(&after.0, &after.1),
                        was: None,
                        changes,
                    });
                }
            }
            None => removed.push(before),
        }
    }
    let mut added: Vec<&(RequestEntry, Vec<String>)> = new
        .requests
        .iter()
        .filter(|(id, _)| !old.requests.contains_key(*id))
        .map(|(_, v)| v)
        .collect();
    // A request that left one place and arrived at another with the same name or the same
    // method and URL was moved (or renamed in place), not deleted and created.
    removed.retain(|before| {
        let found = added.iter().position(|after| {
            after.0.name == before.0.name
                || (after.0.request.method == before.0.request.method
                    && after.0.request.url == before.0.request.url)
        });
        match found {
            Some(index) => {
                let after = added.remove(index);
                let mut changes = request_changes(&before.0, &after.0);
                if before.0.name != after.0.name {
                    changes.insert(0, format!("renamed from {}", before.0.name));
                }
                if before.1 != after.1 {
                    changes.insert(
                        0,
                        format!(
                            "moved from {}",
                            if before.1.is_empty() {
                                "the top level".to_string()
                            } else {
                                before.1.join(" / ")
                            }
                        ),
                    );
                }
                diff.changed.push(Changed {
                    item: item(&after.0, &after.1),
                    was: Some(item(&before.0, &before.1)),
                    changes,
                });
                false
            }
            None => true,
        }
    });
    diff.added = added.into_iter().map(|(e, f)| item(e, f)).collect();
    diff.removed = removed.into_iter().map(|(e, f)| item(e, f)).collect();

    for (id, (label, auth, headers, settings)) in &new.folders {
        let Some((_, old_auth, old_headers, old_settings)) = old.folders.get(id) else {
            continue;
        };
        if old_auth != auth {
            let (x, y) = (auth_line(old_auth), auth_line(auth));
            diff.folders.push(if x == y {
                format!("{label}: {y} auth settings changed")
            } else {
                format!("{label}: auth {x} → {y}")
            });
        }
        let mut lines = Vec::new();
        header_changes(old_headers, headers, &mut lines);
        diff.folders
            .extend(lines.into_iter().map(|l| format!("{label}: {l}")));
        if old_settings != settings {
            diff.folders.push(format!("{label}: settings changed"));
        }
    }

    for env in &new.environments {
        match old.environments.iter().find(|e| e.id == env.id) {
            None => diff
                .environments
                .push(format!("environment {} added", env.name)),
            Some(before) => {
                for v in &env.variables {
                    match before.variables.iter().find(|o| o.name == v.name) {
                        None => diff
                            .environments
                            .push(format!("{}: variable {} added", env.name, v.name)),
                        Some(o) if o.secret != v.secret => diff.environments.push(format!(
                            "{}: {} is {} secret",
                            env.name,
                            v.name,
                            if v.secret { "now" } else { "no longer" }
                        )),
                        Some(o) if !v.secret && o.value != v.value => {
                            diff.environments.push(format!(
                                "{}: {} `{}` → `{}`",
                                env.name,
                                v.name,
                                short(&o.value),
                                short(&v.value)
                            ))
                        }
                        _ => {}
                    }
                }
                for o in &before.variables {
                    if !env.variables.iter().any(|v| v.name == o.name) {
                        diff.environments
                            .push(format!("{}: variable {} removed", env.name, o.name));
                    }
                }
            }
        }
    }
    for env in &old.environments {
        if !new.environments.iter().any(|e| e.id == env.id) {
            diff.environments
                .push(format!("environment {} removed", env.name));
        }
    }
    Ok(diff)
}

/// Compares the workspace at `from` (a commit, branch or tag) with `to` (another one, or
/// the files on disk when `None`).
pub fn diff_refs(root: &Path, from: &str, to: Option<&str>) -> Result<CollectionDiff, String> {
    if !git::is_repo(root) {
        return Err("the workspace is not inside a git repository".into());
    }
    let before = git::snapshot(root, from)?;
    let mut diff = match to {
        Some(to) => {
            let after = git::snapshot(root, to)?;
            compare(before.path(), after.path())?
        }
        None => compare(before.path(), root)?,
    };
    diff.from = from.to_string();
    diff.to = to.unwrap_or("working tree").to_string();
    Ok(diff)
}

fn label(item: &Item) -> String {
    if item.folder.is_empty() {
        item.name.clone()
    } else {
        format!("{} / {}", item.folder.join(" / "), item.name)
    }
}

/// Plain text for the terminal.
pub fn to_text(diff: &CollectionDiff) -> String {
    if diff.is_empty() {
        return format!(
            "No changes in the collection ({} → {}).\n",
            diff.from, diff.to
        );
    }
    let mut out = format!(
        "Collection changes ({} → {}): {} added, {} removed, {} changed\n",
        diff.from,
        diff.to,
        diff.added.len(),
        diff.removed.len(),
        diff.changed.len()
    );
    for i in &diff.added {
        out.push_str(&format!("+ {:<7} {}  {}\n", i.method, label(i), i.url));
    }
    for i in &diff.removed {
        out.push_str(&format!("- {:<7} {}  {}\n", i.method, label(i), i.url));
    }
    for c in &diff.changed {
        out.push_str(&format!("~ {:<7} {}\n", c.item.method, label(&c.item)));
        for change in &c.changes {
            out.push_str(&format!("      {change}\n"));
        }
    }
    for line in diff.folders.iter().chain(&diff.environments) {
        out.push_str(&format!("~ {line}\n"));
    }
    out
}

/// Markdown for a pull request comment.
pub fn to_markdown(diff: &CollectionDiff) -> String {
    let mut out = format!(
        "## API collection changes\n\n`{}` → `{}`",
        diff.from, diff.to
    );
    if diff.is_empty() {
        out.push_str(": no changes.\n");
        return out;
    }
    out.push_str(&format!(
        " · **{} added · {} removed · {} changed**\n",
        diff.added.len(),
        diff.removed.len(),
        diff.changed.len()
    ));
    let section = |title: &str, items: &[Item], out: &mut String| {
        if items.is_empty() {
            return;
        }
        out.push_str(&format!("\n### {title}\n\n"));
        for i in items {
            out.push_str(&format!(
                "- `{}` **{}** — `{}`\n",
                i.method,
                label(i).replace('|', "\\|"),
                i.url
            ));
        }
    };
    section("Added", &diff.added, &mut out);
    section("Removed", &diff.removed, &mut out);
    if !diff.changed.is_empty() {
        out.push_str("\n### Changed\n\n");
        for c in &diff.changed {
            out.push_str(&format!("- `{}` **{}**\n", c.item.method, label(&c.item)));
            for change in &c.changes {
                out.push_str(&format!("  - {change}\n"));
            }
        }
    }
    if !diff.folders.is_empty() || !diff.environments.is_empty() {
        out.push_str("\n### Folders and environments\n\n");
        for line in diff.folders.iter().chain(&diff.environments) {
            out.push_str(&format!("- {line}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{create_folder, init_workspace, save_request, Scripts};
    use crate::HttpMethod;
    use std::fs;

    fn save(root: &Path, id: &str, name: &str, method: HttpMethod, url: &str) {
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

    #[test]
    fn describes_changes_between_commits_request_by_request() {
        let repo = git::tests::repo();
        let ws = repo.path().join("api");
        fs::create_dir_all(&ws).unwrap();
        init_workspace(&ws, "Shop").unwrap();
        create_folder(&ws, "", "Users").unwrap();
        create_folder(&ws, "", "Admin").unwrap();
        save(
            &ws,
            "01-users/01-list.pulpo.yaml",
            "List users",
            HttpMethod::Get,
            "{{base}}/users",
        );
        save(
            &ws,
            "01-users/02-old.pulpo.yaml",
            "Old",
            HttpMethod::Delete,
            "{{base}}/old",
        );
        save(
            &ws,
            "01-users/03-get.pulpo.yaml",
            "Get user",
            HttpMethod::Get,
            "{{base}}/users/:id",
        );
        git::git(&ws, &["add", "."]).unwrap();
        git::git(&ws, &["commit", "-q", "-m", "first"]).unwrap();

        // Change a URL and a header, delete one, add one, move another.
        let mut list = HttpRequest::new(HttpMethod::Get, "{{base}}/users?page=1");
        list.headers.push(Header::new("X-Tenant", "t1"));
        save_request(
            &ws,
            &RequestEntry {
                id: "01-users/01-list.pulpo.yaml".into(),
                name: "List users".into(),
                protocol: crate::Protocol::Http,
                request: list,
                scripts: Scripts {
                    pre: String::new(),
                    test: "pm.test('ok', () => {});".into(),
                },
            },
        )
        .unwrap();
        collection::delete_request(&ws, "01-users/02-old.pulpo.yaml").unwrap();
        save(
            &ws,
            "01-users/04-new.pulpo.yaml",
            "Create user",
            HttpMethod::Post,
            "{{base}}/users",
        );
        collection::delete_request(&ws, "01-users/03-get.pulpo.yaml").unwrap();
        save(
            &ws,
            "02-admin/01-get.pulpo.yaml",
            "Get user",
            HttpMethod::Get,
            "{{base}}/users/:id",
        );

        let diff = diff_refs(&ws, "HEAD", None).unwrap();
        assert_eq!(diff.added.len(), 1, "{diff:#?}");
        assert_eq!(diff.added[0].name, "Create user");
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].name, "Old");
        let list = diff
            .changed
            .iter()
            .find(|c| c.item.name == "List users")
            .unwrap();
        assert_eq!(
            list.changes,
            vec![
                "URL `{{base}}/users` → `{{base}}/users?page=1`".to_string(),
                "header X-Tenant added".to_string(),
                "test script changed".to_string(),
            ]
        );
        let moved = diff
            .changed
            .iter()
            .find(|c| c.item.name == "Get user")
            .unwrap();
        assert_eq!(moved.changes, vec!["moved from Users".to_string()]);
        assert_eq!(moved.item.folder, vec!["Admin".to_string()]);

        let markdown = to_markdown(&diff);
        assert!(
            markdown.contains("**1 added · 1 removed · 2 changed**"),
            "{markdown}"
        );
        assert!(markdown.contains("- `POST` **Users / Create user** — `{{base}}/users`"));
        let text = to_text(&diff);
        assert!(
            text.contains("~ GET     Users / List users\n      URL"),
            "{text}"
        );

        // Comparing a commit with itself: nothing.
        let none = diff_refs(&ws, "HEAD", Some("HEAD")).unwrap();
        assert!(none.is_empty());
        assert!(to_text(&none).starts_with("No changes"));
    }
}
