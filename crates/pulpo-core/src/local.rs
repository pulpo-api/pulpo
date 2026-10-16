//! Values set by scripts (`pm.environment.set`), kept per environment in
//! `.pulpo/local.json`. They are personal and change on every run (tokens, ids),
//! so they never go to git: `.pulpo/` ignores itself.

use crate::collection::CollectionError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

pub const LOCAL_DIR: &str = ".pulpo";
const LOCAL_FILE: &str = "local.json";
/// Key used when no environment is selected.
pub const NO_ENVIRONMENT: &str = "_none";

#[derive(Debug, Default, Serialize, Deserialize)]
struct LocalFile {
    version: u32,
    environments: BTreeMap<String, BTreeMap<String, String>>,
}

fn io_error(path: &Path, err: std::io::Error) -> CollectionError {
    CollectionError::Io(format!("{}: {err}", path.display()))
}

fn file_path(root: &Path) -> PathBuf {
    root.join(LOCAL_DIR).join(LOCAL_FILE)
}

fn read(root: &Path) -> LocalFile {
    fs::read_to_string(file_path(root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(LocalFile {
            version: 1,
            environments: BTreeMap::new(),
        })
}

/// Makes sure `.pulpo/` exists and is ignored by git.
pub fn ensure_local_dir(root: &Path) -> Result<PathBuf, CollectionError> {
    let dir = root.join(LOCAL_DIR);
    fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, "# Local Pulpo state: never commit it.\n*\n")
            .map_err(|e| io_error(&ignore, e))?;
    }
    Ok(dir)
}

/// Local values of an environment (`None` = no environment selected).
pub fn load_local(root: &Path, environment_id: Option<&str>) -> HashMap<String, String> {
    read(root)
        .environments
        .remove(environment_id.unwrap_or(NO_ENVIRONMENT))
        .map(|m| m.into_iter().collect())
        .unwrap_or_default()
}

/// Applies the changes made by scripts: `Some` sets a value, `None` removes it.
pub fn apply_changes(
    root: &Path,
    environment_id: Option<&str>,
    changes: &HashMap<String, Option<String>>,
) -> Result<(), CollectionError> {
    if changes.is_empty() {
        return Ok(());
    }
    ensure_local_dir(root)?;
    let mut file = read(root);
    file.version = 1;
    let values = file
        .environments
        .entry(environment_id.unwrap_or(NO_ENVIRONMENT).to_string())
        .or_default();
    for (name, value) in changes {
        match value {
            Some(v) => {
                values.insert(name.clone(), v.clone());
            }
            None => {
                values.remove(name);
            }
        }
    }
    let path = file_path(root);
    let text =
        serde_json::to_string_pretty(&file).map_err(|e| CollectionError::Io(e.to_string()))?;
    fs::write(&path, text).map_err(|e| io_error(&path, e))
}

/// Forgets every local value of an environment.
pub fn clear_local(root: &Path, environment_id: Option<&str>) -> Result<(), CollectionError> {
    let mut file = read(root);
    if file
        .environments
        .remove(environment_id.unwrap_or(NO_ENVIRONMENT))
        .is_none()
    {
        return Ok(());
    }
    let path = file_path(root);
    let text =
        serde_json::to_string_pretty(&file).map_err(|e| CollectionError::Io(e.to_string()))?;
    fs::write(&path, text).map_err(|e| io_error(&path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_values_per_environment_and_ignores_itself() {
        let dir = tempfile::tempdir().unwrap();
        let changes = HashMap::from([("token".to_string(), Some("abc".to_string()))]);
        apply_changes(dir.path(), Some("environments/local.yaml"), &changes).unwrap();
        apply_changes(
            dir.path(),
            None,
            &HashMap::from([("x".to_string(), Some("1".to_string()))]),
        )
        .unwrap();

        assert_eq!(
            load_local(dir.path(), Some("environments/local.yaml"))["token"],
            "abc"
        );
        assert_eq!(load_local(dir.path(), None)["x"], "1");
        assert!(load_local(dir.path(), Some("environments/dev.yaml")).is_empty());
        let ignore = fs::read_to_string(dir.path().join(".pulpo/.gitignore")).unwrap();
        assert!(ignore.lines().any(|l| l == "*"));

        apply_changes(
            dir.path(),
            Some("environments/local.yaml"),
            &HashMap::from([("token".to_string(), None)]),
        )
        .unwrap();
        assert!(load_local(dir.path(), Some("environments/local.yaml")).is_empty());
        clear_local(dir.path(), None).unwrap();
        assert!(load_local(dir.path(), None).is_empty());
    }

    #[test]
    fn a_broken_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        ensure_local_dir(dir.path()).unwrap();
        fs::write(dir.path().join(".pulpo/local.json"), "{ not json").unwrap();
        assert!(load_local(dir.path(), None).is_empty());
    }
}
