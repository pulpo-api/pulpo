//! Environments stored in `environments/<slug>.yaml`.
//!
//! Secret values never touch the files: the file only says that a variable is secret,
//! and the value lives in a [`SecretStore`] (the OS keychain in the app and the CLI).

use crate::collection::{slugify, CollectionError, FORMAT_VERSION};
use crate::variables::Variables;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const ENVIRONMENTS_DIR: &str = "environments";

/// One variable of an environment. For secrets, `value` is only filled when loading
/// with secrets or when the user types a new one; `has_value` says whether one is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub has_value: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    /// Path relative to the workspace, e.g. `environments/local.yaml`.
    pub id: String,
    pub name: String,
    pub variables: Vec<Variable>,
}

/// Where secret values are kept.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// Keeps secrets in memory. Used by tests and the browser preview.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().map_err(|e| e.to_string())?.get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|e| e.to_string())?
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.0.lock().map_err(|e| e.to_string())?.remove(key);
        Ok(())
    }
}

/// Key of a secret in the store. The workspace path keeps two checkouts apart.
pub fn secret_key(root: &Path, environment_id: &str, variable: &str) -> String {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    format!("{}|{environment_id}|{variable}", root.display())
}

/// Environment variable that overrides a secret, handy in CI: `PULPO_SECRET_<NAME>`.
pub fn secret_override_name(variable: &str) -> String {
    let name: String = variable
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PULPO_SECRET_{name}")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentFileV1 {
    version: u32,
    name: String,
    #[serde(default)]
    variables: Vec<VariableFileV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VariableFileV1 {
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    secret: bool,
}

fn io_error(path: &Path, err: std::io::Error) -> CollectionError {
    CollectionError::Io(format!("{}: {err}", path.display()))
}

fn store_error(err: String) -> CollectionError {
    CollectionError::Io(format!("keychain: {err}"))
}

fn quote(value: &str) -> String {
    crate::collection::quote(value)
}

fn environment_yaml(env: &Environment) -> String {
    let mut out = format!("version: {FORMAT_VERSION}\nname: {}\n", quote(&env.name));
    if !env.variables.is_empty() {
        out.push_str("variables:\n");
        for var in &env.variables {
            out.push_str(&format!("  - name: {}\n", quote(&var.name)));
            if var.secret {
                out.push_str("    secret: true\n");
            } else {
                out.push_str(&format!("    value: {}\n", quote(&var.value)));
            }
        }
    }
    out
}

fn environment_path(root: &Path, id: &str) -> Result<PathBuf, CollectionError> {
    let file = id
        .strip_prefix(&format!("{ENVIRONMENTS_DIR}/"))
        .filter(|f| f.ends_with(".yaml") && !f.contains(['/', '\\']) && !f.starts_with('.'))
        .ok_or_else(|| CollectionError::InvalidPath(id.to_string()))?;
    Ok(root.join(ENVIRONMENTS_DIR).join(file))
}

fn read_file(root: &Path, id: &str) -> Result<EnvironmentFileV1, CollectionError> {
    let path = environment_path(root, id)?;
    let text = fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
    let file: EnvironmentFileV1 = serde_norway::from_str(&text)
        .map_err(|e| CollectionError::InvalidFile(format!("{}: {e}", path.display())))?;
    if file.version != FORMAT_VERSION {
        return Err(CollectionError::InvalidFile(format!(
            "{}: unsupported version {}",
            path.display(),
            file.version
        )));
    }
    for var in &file.variables {
        if var.secret && var.value.is_some() {
            return Err(CollectionError::InvalidFile(format!(
                "{}: secret variable '{}' must not have a value in the file",
                path.display(),
                var.name
            )));
        }
    }
    Ok(file)
}

/// Lists the environments of a workspace. Secret values are not read, only `has_value`.
pub fn list_environments(
    root: &Path,
    store: &dyn SecretStore,
) -> Result<Vec<Environment>, CollectionError> {
    let dir = root.join(ENVIRONMENTS_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| io_error(&dir, e))?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".yaml") && !n.starts_with('.'))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| load_environment(root, &format!("{ENVIRONMENTS_DIR}/{file}"), store, false))
        .collect()
}

/// Loads one environment. With `with_secrets`, secret values are read from the store.
pub fn load_environment(
    root: &Path,
    id: &str,
    store: &dyn SecretStore,
    with_secrets: bool,
) -> Result<Environment, CollectionError> {
    let file = read_file(root, id)?;
    let mut variables = Vec::with_capacity(file.variables.len());
    for var in file.variables {
        let (value, has_value) = if var.secret {
            // A keychain that cannot be reached (CI runners, headless Linux) counts as
            // "no value stored", so PULPO_SECRET_<NAME> overrides still work.
            let stored = store.get(&secret_key(root, id, &var.name)).unwrap_or(None);
            let has_value = stored.is_some();
            (
                if with_secrets {
                    stored.unwrap_or_default()
                } else {
                    String::new()
                },
                has_value,
            )
        } else {
            (var.value.unwrap_or_default(), true)
        };
        variables.push(Variable {
            name: var.name,
            value,
            secret: var.secret,
            has_value,
        });
    }
    Ok(Environment {
        id: id.to_string(),
        name: file.name,
        variables,
    })
}

/// Writes an environment. Secret values typed by the user go to the store; an empty
/// secret value keeps the stored one. Secrets that disappear or stop being secret are deleted.
pub fn save_environment(
    root: &Path,
    env: &Environment,
    store: &dyn SecretStore,
) -> Result<(), CollectionError> {
    let path = environment_path(root, &env.id)?;
    let previous_secrets: Vec<String> = if path.is_file() {
        read_file(root, &env.id)?
            .variables
            .into_iter()
            .filter(|v| v.secret)
            .map(|v| v.name)
            .collect()
    } else {
        Vec::new()
    };

    for var in env
        .variables
        .iter()
        .filter(|v| v.secret && !v.value.is_empty())
    {
        store
            .set(&secret_key(root, &env.id, &var.name), &var.value)
            .map_err(store_error)?;
    }
    for name in previous_secrets {
        if !env.variables.iter().any(|v| v.secret && v.name == name) {
            store
                .delete(&secret_key(root, &env.id, &name))
                .map_err(store_error)?;
        }
    }

    fs::create_dir_all(root.join(ENVIRONMENTS_DIR)).map_err(|e| io_error(&path, e))?;
    fs::write(&path, environment_yaml(env)).map_err(|e| io_error(&path, e))
}

/// Creates an empty environment and returns it.
pub fn create_environment(
    root: &Path,
    name: &str,
    store: &dyn SecretStore,
) -> Result<Environment, CollectionError> {
    let dir = root.join(ENVIRONMENTS_DIR);
    let base = slugify(name);
    let mut slug = base.clone();
    let mut n = 2;
    while dir.join(format!("{slug}.yaml")).exists() {
        slug = format!("{base}-{n}");
        n += 1;
    }
    let env = Environment {
        id: format!("{ENVIRONMENTS_DIR}/{slug}.yaml"),
        name: name.to_string(),
        variables: Vec::new(),
    };
    save_environment(root, &env, store)?;
    Ok(env)
}

/// Deletes an environment file and its stored secrets.
pub fn delete_environment(
    root: &Path,
    id: &str,
    store: &dyn SecretStore,
) -> Result<(), CollectionError> {
    let path = environment_path(root, id)?;
    for var in read_file(root, id)?
        .variables
        .into_iter()
        .filter(|v| v.secret)
    {
        store
            .delete(&secret_key(root, id, &var.name))
            .map_err(store_error)?;
    }
    fs::remove_file(&path).map_err(|e| io_error(&path, e))
}

/// Rewrites environment files in canonical form; with `check`, only reports them.
pub(crate) fn format_environments(
    root: &Path,
    check: bool,
) -> Result<Vec<String>, CollectionError> {
    let dir = root.join(ENVIRONMENTS_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| io_error(&dir, e))?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".yaml") && !n.starts_with('.'))
        .collect();
    files.sort();
    let mut changed = Vec::new();
    for file in files {
        let id = format!("{ENVIRONMENTS_DIR}/{file}");
        let parsed = read_file(root, &id)?;
        let env = Environment {
            id: id.clone(),
            name: parsed.name,
            variables: parsed
                .variables
                .into_iter()
                .map(|v| Variable {
                    name: v.name,
                    value: v.value.unwrap_or_default(),
                    secret: v.secret,
                    has_value: false,
                })
                .collect(),
        };
        let path = environment_path(root, &id)?;
        let canonical = environment_yaml(&env);
        if fs::read_to_string(&path).map_err(|e| io_error(&path, e))? != canonical {
            changed.push(id);
            if !check {
                fs::write(&path, canonical).map_err(|e| io_error(&path, e))?;
            }
        }
    }
    Ok(changed)
}

/// The variables used to resolve requests: the environment, with secrets filled in.
/// `PULPO_SECRET_<NAME>` process variables win over the store.
pub fn variables_for(
    root: &Path,
    id: &str,
    store: &dyn SecretStore,
) -> Result<Variables, CollectionError> {
    let env = load_environment(root, id, store, true)?;
    Ok(env
        .variables
        .into_iter()
        .filter_map(|v| {
            let value = if v.secret {
                std::env::var(secret_override_name(&v.name))
                    .ok()
                    .or(v.has_value.then_some(v.value))
            } else {
                Some(v.value)
            };
            value.map(|value| (v.name, value))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::init_workspace;

    fn setup() -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        init_workspace(dir.path(), "Demo").unwrap();
        (dir, MemoryStore::default())
    }

    fn var(name: &str, value: &str, secret: bool) -> Variable {
        Variable {
            name: name.into(),
            value: value.into(),
            secret,
            has_value: false,
        }
    }

    #[test]
    fn keeps_secrets_out_of_the_file() {
        let (ws, store) = setup();
        let mut env = create_environment(ws.path(), "Local", &store).unwrap();
        assert_eq!(env.id, "environments/local.yaml");
        env.variables = vec![
            var("base", "http://localhost:8888", false),
            var("token", "s3cr3t", true),
        ];
        save_environment(ws.path(), &env, &store).unwrap();

        let text = fs::read_to_string(ws.path().join("environments/local.yaml")).unwrap();
        assert_eq!(
            text,
            "version: 1\nname: \"Local\"\nvariables:\n  - name: \"base\"\n    value: \"http://localhost:8888\"\n  - name: \"token\"\n    secret: true\n"
        );
        assert!(!text.contains("s3cr3t"));

        let listed = &list_environments(ws.path(), &store).unwrap()[0];
        assert_eq!(listed.variables[1].value, "");
        assert!(listed.variables[1].has_value);

        let vars = variables_for(ws.path(), &env.id, &store).unwrap();
        assert_eq!(vars["base"], "http://localhost:8888");
        assert_eq!(vars["token"], "s3cr3t");
    }

    #[test]
    fn empty_secret_keeps_the_stored_value_and_removal_deletes_it() {
        let (ws, store) = setup();
        let mut env = create_environment(ws.path(), "Dev", &store).unwrap();
        env.variables = vec![var("token", "first", true)];
        save_environment(ws.path(), &env, &store).unwrap();

        env.variables = vec![var("token", "", true)];
        save_environment(ws.path(), &env, &store).unwrap();
        assert_eq!(
            variables_for(ws.path(), &env.id, &store).unwrap()["token"],
            "first"
        );

        env.variables = vec![];
        save_environment(ws.path(), &env, &store).unwrap();
        assert_eq!(
            store.get(&secret_key(ws.path(), &env.id, "token")).unwrap(),
            None
        );
    }

    #[test]
    fn missing_secrets_are_left_unresolved() {
        let (ws, store) = setup();
        let mut env = create_environment(ws.path(), "CI", &store).unwrap();
        env.variables = vec![var("apiKeyThatIsNotSet", "", true)];
        save_environment(ws.path(), &env, &store).unwrap();
        assert!(!variables_for(ws.path(), &env.id, &store)
            .unwrap()
            .contains_key("apiKeyThatIsNotSet"));
    }

    #[test]
    fn rejects_secret_values_written_in_the_file() {
        let (ws, store) = setup();
        fs::create_dir_all(ws.path().join(ENVIRONMENTS_DIR)).unwrap();
        fs::write(
            ws.path().join("environments/bad.yaml"),
            "version: 1\nname: Bad\nvariables:\n  - name: token\n    value: leaked\n    secret: true\n",
        )
        .unwrap();
        assert!(matches!(
            list_environments(ws.path(), &store),
            Err(CollectionError::InvalidFile(_))
        ));
    }

    #[test]
    fn deletes_environments_with_their_secrets() {
        let (ws, store) = setup();
        let mut env = create_environment(ws.path(), "Local", &store).unwrap();
        let again = create_environment(ws.path(), "Local", &store).unwrap();
        assert_eq!(again.id, "environments/local-2.yaml");
        env.variables = vec![var("token", "x", true)];
        save_environment(ws.path(), &env, &store).unwrap();
        delete_environment(ws.path(), &env.id, &store).unwrap();
        assert!(!ws.path().join(&env.id).exists());
        assert_eq!(
            store.get(&secret_key(ws.path(), &env.id, "token")).unwrap(),
            None
        );
    }

    #[test]
    fn refuses_ids_outside_the_environments_folder() {
        let (ws, store) = setup();
        for id in [
            "../x.yaml",
            "environments/../pulpo.yaml",
            "environments/a/b.yaml",
            "pulpo.yaml",
        ] {
            assert!(
                matches!(
                    load_environment(ws.path(), id, &store, false),
                    Err(CollectionError::InvalidPath(_))
                ),
                "{id}"
            );
        }
    }

    struct BrokenStore;

    impl SecretStore for BrokenStore {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Err("no keychain here".into())
        }
        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Err("no keychain here".into())
        }
        fn delete(&self, _: &str) -> Result<(), String> {
            Err("no keychain here".into())
        }
    }

    #[test]
    fn an_unreachable_keychain_reads_as_empty_but_fails_on_write() {
        let (ws, store) = setup();
        let mut env = create_environment(ws.path(), "CI", &store).unwrap();
        env.variables = vec![
            var("base", "https://api", false),
            var("ciOnlyToken", "", true),
        ];
        save_environment(ws.path(), &env, &store).unwrap();

        let listed = &list_environments(ws.path(), &BrokenStore).unwrap()[0];
        assert!(!listed.variables[1].has_value);
        assert_eq!(
            variables_for(ws.path(), &env.id, &BrokenStore).unwrap()["base"],
            "https://api"
        );

        env.variables[1].value = "x".into();
        assert!(save_environment(ws.path(), &env, &BrokenStore).is_err());
    }

    #[test]
    fn names_ci_overrides() {
        assert_eq!(secret_override_name("api-key"), "PULPO_SECRET_API_KEY");
    }
}
