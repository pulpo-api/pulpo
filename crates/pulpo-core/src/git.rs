//! Git, through the `git` command: what the collection looked like at a commit, and the
//! status, commit, branch, pull and push of the app's git panel.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Runs git in `dir`. It never asks for credentials on a terminal: without a credential
/// helper, pull and push fail with git's message instead of hanging.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git is not available: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(if err.is_empty() { out } else { err })
    }
}

/// Whether the workspace is inside a git repository.
pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|o| o.trim() == "true")
}

/// The workspace folder relative to the repository root (`""` or `"api/"`).
fn prefix(dir: &Path) -> Result<String, String> {
    Ok(git(dir, &["rev-parse", "--show-prefix"])?
        .trim()
        .to_string())
}

/// A temporary copy of the workspace as it was at a commit; removed when dropped.
pub struct Snapshot {
    dir: PathBuf,
}

impl Snapshot {
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Writes the workspace files of `git_ref` to a temporary folder.
pub fn snapshot(dir: &Path, git_ref: &str) -> Result<Snapshot, String> {
    if git_ref.starts_with('-') {
        return Err(format!("invalid revision {git_ref}"));
    }
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{git_ref}^{{commit}}"),
        ],
    )
    .map_err(|_| format!("{git_ref} is not a commit, branch or tag of this repository"))?;
    let prefix = prefix(dir)?;
    let listing = git(
        dir,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--full-name",
            "--name-only",
            git_ref,
            "--",
            ".",
        ],
    )?;
    let paths: Vec<&str> = listing.split('\0').filter(|p| !p.is_empty()).collect();
    let target = std::env::temp_dir().join(format!("pulpo-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    let snapshot = Snapshot { dir: target };

    // One `git cat-file --batch` for every file, fed from another thread.
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("git is not available: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("git cat-file: no stdin")?;
    let requests: String = paths.iter().map(|p| format!("{git_ref}:{p}\n")).collect();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(requests.as_bytes());
    });
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or("git cat-file: no stdout")?
        .read_to_end(&mut output)
        .map_err(|e| e.to_string())?;
    let _ = writer.join();
    let _ = child.wait();

    let mut at = 0;
    for path in paths {
        let Some(end) = output[at..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&output[at..at + end]).into_owned();
        at += end + 1;
        let size: usize = match header.split(' ').collect::<Vec<_>>().as_slice() {
            [_, "blob", size] => size.parse().unwrap_or(0),
            _ => continue,
        };
        let content = &output[at..(at + size).min(output.len())];
        at += size + 1;
        let relative = path.strip_prefix(prefix.as_str()).unwrap_or(path);
        let file = snapshot.dir.join(relative);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&file, content).map_err(|e| e.to_string())?;
    }
    Ok(snapshot)
}

// --------------------------------------------------------------------- the git panel

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStatus {
    /// Relative to the workspace.
    pub path: String,
    /// `added`, `modified`, `deleted`, `renamed` or `untracked`.
    pub state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub repo: bool,
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Changed files of the workspace (not the rest of the repository).
    pub files: Vec<FileStatus>,
}

/// Branch, upstream and the workspace's changed files.
pub fn status(dir: &Path) -> Result<Status, String> {
    if !is_repo(dir) {
        return Ok(Status::default());
    }
    let prefix = prefix(dir)?;
    let text = git(
        dir,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )?;
    let mut status = Status {
        repo: true,
        ..Default::default()
    };
    let mut entries = text.split('\0').peekable();
    while let Some(entry) = entries.next() {
        if let Some(head) = entry.strip_prefix("# branch.head ") {
            status.branch = head.to_string();
        } else if let Some(upstream) = entry.strip_prefix("# branch.upstream ") {
            status.upstream = Some(upstream.to_string());
        } else if let Some(ab) = entry.strip_prefix("# branch.ab ") {
            for part in ab.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    status.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    status.behind = n.parse().unwrap_or(0);
                }
            }
        } else if entry.starts_with("1 ") || entry.starts_with("2 ") {
            let fields: Vec<&str> = entry
                .splitn(if entry.starts_with('1') { 9 } else { 10 }, ' ')
                .collect();
            let xy = fields.get(1).copied().unwrap_or("..");
            let path = fields.last().copied().unwrap_or_default();
            let state = if entry.starts_with('2') {
                // The original path follows as its own entry.
                entries.next();
                "renamed"
            } else if xy.contains('D') {
                "deleted"
            } else if xy.contains('A') {
                "added"
            } else {
                "modified"
            };
            status.files.push(FileStatus {
                path: path
                    .strip_prefix(prefix.as_str())
                    .unwrap_or(path)
                    .to_string(),
                state: state.into(),
            });
        } else if let Some(path) = entry.strip_prefix("? ") {
            status.files.push(FileStatus {
                path: path
                    .strip_prefix(prefix.as_str())
                    .unwrap_or(path)
                    .to_string(),
                state: "untracked".into(),
            });
        }
    }
    Ok(status)
}

/// Local branches, the current one first.
pub fn branches(dir: &Path) -> Result<Vec<String>, String> {
    let text = git(dir, &["branch", "--format=%(HEAD) %(refname:short)"])?;
    let mut current = Vec::new();
    let mut others = Vec::new();
    for line in text.lines() {
        match line.strip_prefix("* ") {
            Some(name) => current.push(name.to_string()),
            None => others.push(line.trim().to_string()),
        }
    }
    current.extend(others);
    Ok(current)
}

/// Switches to a branch, creating it from the current commit when `create`.
pub fn switch(dir: &Path, branch: &str, create: bool) -> Result<(), String> {
    if branch.trim().is_empty() || branch.starts_with('-') {
        return Err("invalid branch name".into());
    }
    if create {
        git(dir, &["switch", "-c", branch])?;
    } else {
        git(dir, &["switch", branch])?;
    }
    Ok(())
}

/// Commits every change of the workspace (and only of the workspace).
pub fn commit(dir: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("write a commit message".into());
    }
    git(dir, &["add", "--all", "--", "."])?;
    git(dir, &["commit", "-m", message.trim(), "--", "."])?;
    Ok(git(dir, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string())
}

/// Fast-forward pull of the current branch.
pub fn pull(dir: &Path) -> Result<String, String> {
    git(dir, &["pull", "--ff-only"]).map(|o| o.trim().to_string())
}

/// Pushes the current branch, setting its upstream the first time.
pub fn push(dir: &Path) -> Result<String, String> {
    let current = status(dir)?;
    let result = if current.upstream.is_some() {
        git(dir, &["push"])
    } else {
        git(dir, &["push", "--set-upstream", "origin", &current.branch])
    };
    result.map(|o| o.trim().to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A repository with the workspace in `api/`, committed once.
    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| git(dir.path(), args).unwrap();
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        dir
    }

    #[test]
    fn snapshots_a_workspace_at_a_commit() {
        let dir = repo();
        let ws = dir.path().join("api");
        fs::create_dir_all(ws.join("sub")).unwrap();
        fs::write(ws.join("a.txt"), "one").unwrap();
        fs::write(ws.join("sub/b.txt"), "two\nlines").unwrap();
        fs::write(dir.path().join("outside.txt"), "x").unwrap();
        git(dir.path(), &["add", "."]).unwrap();
        git(dir.path(), &["commit", "-q", "-m", "first"]).unwrap();
        fs::write(ws.join("a.txt"), "changed").unwrap();

        let snap = snapshot(&ws, "HEAD").unwrap();
        assert_eq!(
            fs::read_to_string(snap.path().join("a.txt")).unwrap(),
            "one"
        );
        assert_eq!(
            fs::read_to_string(snap.path().join("sub/b.txt")).unwrap(),
            "two\nlines"
        );
        assert!(!snap.path().join("outside.txt").exists());
        let kept = snap.path().to_path_buf();
        drop(snap);
        assert!(!kept.exists());
        assert!(snapshot(&ws, "nope").is_err());
    }

    #[test]
    fn reports_status_and_commits_only_the_workspace() {
        let dir = repo();
        let ws = dir.path().join("api");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("a.txt"), "one").unwrap();
        fs::write(dir.path().join("outside.txt"), "x").unwrap();
        let before = status(&ws).unwrap();
        assert!(before.repo);
        assert_eq!(before.files.len(), 1, "{before:?}");
        assert_eq!(before.files[0].path, "a.txt");
        assert_eq!(before.files[0].state, "untracked");
        let id = commit(&ws, "add a").unwrap();
        assert!(!id.is_empty());
        assert!(status(&ws).unwrap().files.is_empty());
        // The file outside the workspace is still not committed.
        assert!(git(dir.path(), &["status", "--porcelain"])
            .unwrap()
            .contains("outside.txt"));
        switch(&ws, "feature", true).unwrap();
        assert_eq!(branches(&ws).unwrap()[0], "feature");
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(!status(elsewhere.path()).unwrap().repo);
    }
}
