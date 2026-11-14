//! `pulpo://` links to a request, to share in a chat. No cloud: the link names the git
//! repository (its `origin` remote), the workspace folder inside it and the request, and
//! each person's Pulpo finds the matching folder on their own machine.

use crate::collection;
use crate::git;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEME: &str = "pulpo://";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    /// `github.com/org/repo`; empty when the workspace is not in a repository with a
    /// remote (then only the name can match).
    pub repo: String,
    /// The workspace folder inside the repository (`""` or `api`).
    pub workspace: String,
    /// The workspace's name, to show and to match without a repository.
    pub name: String,
    /// Request id (its path inside the workspace).
    pub request: String,
}

/// `git@github.com:Org/Repo.git`, `https://user@github.com/Org/Repo` → `github.com/org/repo`.
pub fn normalize_remote(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = match url.split_once("://") {
        Some((_, rest)) => rest.to_string(),
        // scp-like `user@host:path`
        None => match url.split_once(':') {
            Some((host, path)) if !host.contains('/') => format!("{host}/{path}"),
            _ => url.to_string(),
        },
    };
    let url = match url.split_once('/') {
        // Drop the user, password and port; keep the host.
        Some((authority, path)) => {
            let host = authority.rsplit('@').next().unwrap_or(authority);
            let host = host.split(':').next().unwrap_or(host);
            format!("{host}/{path}")
        }
        None => url,
    };
    // Hosts and most forges ignore case.
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .to_lowercase()
}

/// `org/repo` of a normalized remote: SSH host aliases (`github-work`) still match.
fn repo_path(repo: &str) -> &str {
    repo.split_once('/').map(|(_, path)| path).unwrap_or(repo)
}

/// Repository and folder of a workspace.
fn identity(root: &Path) -> (String, String) {
    if !git::is_repo(root) {
        return (String::new(), String::new());
    }
    let repo = git::git(root, &["remote", "get-url", "origin"])
        .map(|u| normalize_remote(&u))
        .unwrap_or_default();
    let prefix = git::git(root, &["rev-parse", "--show-prefix"])
        .map(|p| p.trim().trim_end_matches('/').to_string())
        .unwrap_or_default();
    (repo, prefix)
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The link to a request of the workspace at `root`.
pub fn make(root: &Path, request_id: &str) -> Result<String, String> {
    let entry = collection::load_request(root, request_id).map_err(|e| e.to_string())?;
    let workspace = collection::load_workspace(root).map_err(|e| e.to_string())?;
    let (repo, prefix) = identity(root);
    let mut link = format!("{SCHEME}open?");
    if !repo.is_empty() {
        link.push_str(&format!("repo={}&", encode(&repo)));
    }
    if !prefix.is_empty() {
        link.push_str(&format!("ws={}&", encode(&prefix)));
    }
    link.push_str(&format!(
        "request={}&name={}",
        encode(&entry.id),
        encode(&workspace.name)
    ));
    Ok(link)
}

/// Reads a link (it may come with spaces or inside `<…>` from a chat).
pub fn parse(text: &str) -> Result<Link, String> {
    let text = text.trim().trim_matches(|c| c == '<' || c == '>');
    let rest = text
        .strip_prefix(SCHEME)
        .ok_or_else(|| format!("not a Pulpo link (they start with {SCHEME})"))?;
    let query = rest
        .strip_prefix("open")
        .map(|r| r.trim_start_matches('/'))
        .and_then(|r| r.strip_prefix('?'))
        .ok_or("not a Pulpo link to a request")?;
    let mut link = Link::default();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode(value);
        match key {
            "repo" => link.repo = value,
            "ws" => link.workspace = value,
            "name" => link.name = value,
            "request" => link.request = value,
            _ => {}
        }
    }
    if link.request.is_empty() || link.request.contains("..") {
        return Err("the link does not name a request".into());
    }
    Ok(link)
}

/// Whether the workspace at `root` is the one the link points to.
pub fn matches(root: &Path, link: &Link) -> bool {
    if !root.join(collection::WORKSPACE_FILE).is_file() {
        return false;
    }
    let (repo, prefix) = identity(root);
    if !link.repo.is_empty() {
        return !repo.is_empty()
            && repo_path(&repo) == repo_path(&link.repo)
            && prefix == link.workspace;
    }
    collection::load_workspace(root).is_ok_and(|w| w.name == link.name)
}

/// The first of `roots` the link points to.
pub fn find<'a>(roots: &'a [String], link: &Link) -> Option<&'a str> {
    roots
        .iter()
        .map(String::as_str)
        .find(|root| matches(Path::new(root), link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{init_workspace, save_request, RequestEntry, Scripts};
    use crate::http::HttpRequest;
    use crate::HttpMethod;

    #[test]
    fn normalizes_remotes() {
        for url in [
            "git@github.com:Pulpo-API/pulpo.git",
            "https://github.com/pulpo-api/pulpo",
            "https://user:token@github.com/pulpo-api/pulpo.git",
            "ssh://git@github.com:22/pulpo-api/pulpo.git",
            "git@github-llillototote:pulpo-api/pulpo.git",
        ] {
            let normalized = normalize_remote(url);
            assert!(
                normalized.ends_with("/pulpo-api/pulpo"),
                "{url} → {normalized}"
            );
        }
        assert_eq!(
            normalize_remote("https://github.com/pulpo-api/pulpo/"),
            "github.com/pulpo-api/pulpo"
        );
    }

    #[test]
    fn links_a_request_and_finds_the_workspace_again() {
        let dir = git::tests::repo();
        git::git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:Shop/API.git"],
        )
        .unwrap();
        let ws = dir.path().join("collections");
        std::fs::create_dir_all(&ws).unwrap();
        init_workspace(&ws, "Shop API").unwrap();
        save_request(
            &ws,
            &RequestEntry {
                id: "01-list users.pulpo.yaml".into(),
                name: "List users".into(),
                protocol: crate::Protocol::Http,
                request: HttpRequest::new(HttpMethod::Get, "{{base}}/users"),
                scripts: Scripts::default(),
            },
        )
        .unwrap();
        let link = make(&ws, "01-list users.pulpo.yaml").unwrap();
        assert_eq!(
            link,
            "pulpo://open?repo=github.com/shop/api&ws=collections&request=01-list%20users.pulpo.yaml&name=Shop%20API"
        );
        let parsed = parse(&format!("  <{link}> ")).unwrap();
        assert_eq!(parsed.request, "01-list users.pulpo.yaml");
        assert_eq!(parsed.name, "Shop API");

        // A clone elsewhere (same remote, same folder) matches; another folder does not.
        let roots = vec![dir.path().display().to_string(), ws.display().to_string()];
        assert_eq!(find(&roots, &parsed), Some(roots[1].as_str()));
        let other = Link {
            workspace: "other".into(),
            ..parsed.clone()
        };
        assert_eq!(find(&roots, &other), None);
        assert!(parse("https://example.com").is_err());
        assert!(parse("pulpo://open?request=../../etc").is_err());
    }
}
