//! Saved examples of a request: responses kept next to it, to document what an endpoint
//! answers and, later, to feed the mock server and contract tests.
//!
//! ```text
//! 01-get-order.pulpo.yaml
//! 01-get-order.examples/
//!     01-ok.yaml           name, status, headers
//!     01-ok.body.json      the body, so it diffs well in git
//! ```

use crate::collection::{
    io_error, quote, request_path, slugify, split_order, stem_of, BodyKind, CollectionError,
};
use crate::http::Header;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const EXAMPLES_SUFFIX: &str = ".examples";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Example {
    /// File stem inside the examples folder (`01-ok`); empty for a new example.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExampleFileV1 {
    version: u32,
    name: String,
    status: u16,
    #[serde(default)]
    headers: Vec<Header>,
    #[serde(default)]
    body: Option<BodyKind>,
}

/// The examples folder of a request file.
pub fn examples_dir(request_file: &Path) -> PathBuf {
    request_file.with_file_name(format!("{}{EXAMPLES_SUFFIX}", stem_of(request_file)))
}

fn body_file(dir: &Path, id: &str, kind: BodyKind) -> PathBuf {
    dir.join(format!("{id}.body.{}", kind.extension()))
}

fn example_yaml(example: &Example, kind: Option<BodyKind>) -> String {
    let mut out = format!(
        "version: {FORMAT_VERSION}\nname: {}\nstatus: {}\n",
        quote(&example.name),
        example.status
    );
    if !example.headers.is_empty() {
        out.push_str("headers:\n");
        for h in &example.headers {
            out.push_str(&format!("  - name: {}\n", quote(&h.name)));
            out.push_str(&format!("    value: {}\n", quote(&h.value)));
            if !h.enabled {
                out.push_str("    enabled: false\n");
            }
        }
    }
    if let Some(kind) = kind {
        out.push_str(&format!("body: {}\n", quote(kind.as_str())));
    }
    out
}

/// The examples of a request, in order.
pub fn list_examples(root: &Path, request_id: &str) -> Result<Vec<Example>, CollectionError> {
    let dir = examples_dir(&request_path(root, request_id)?);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| io_error(&dir, e))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter_map(|n| n.strip_suffix(".yaml").map(str::to_string))
        .collect();
    ids.sort_by_key(|id| crate::collection::order_key(id));
    let mut out = Vec::new();
    for id in ids {
        let file = dir.join(format!("{id}.yaml"));
        let text = fs::read_to_string(&file).map_err(|e| io_error(&file, e))?;
        let parsed: ExampleFileV1 = serde_norway::from_str(&text)
            .map_err(|e| CollectionError::InvalidFile(format!("{}: {e}", file.display())))?;
        if parsed.version != FORMAT_VERSION {
            return Err(CollectionError::InvalidFile(format!(
                "{}: unsupported version {}",
                file.display(),
                parsed.version
            )));
        }
        let body = match parsed.body {
            Some(kind) => {
                let path = body_file(&dir, &id, kind);
                fs::read_to_string(&path).map_err(|e| io_error(&path, e))?
            }
            None => String::new(),
        };
        out.push(Example {
            id,
            name: parsed.name,
            status: parsed.status,
            headers: parsed.headers,
            body,
        });
    }
    Ok(out)
}

/// Saves an example (a new one when `id` is empty) and returns it with its id.
pub fn save_example(
    root: &Path,
    request_id: &str,
    example: &Example,
) -> Result<Example, CollectionError> {
    let dir = examples_dir(&request_path(root, request_id)?);
    fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
    let id = if example.id.is_empty() {
        let next = fs::read_dir(&dir)
            .map_err(|e| io_error(&dir, e))?
            .filter_map(|e| e.ok())
            .filter_map(|e| split_order(&e.file_name().to_string_lossy()).0)
            .max()
            .unwrap_or(0)
            + 1;
        format!("{next:02}-{}", slugify(&example.name))
    } else {
        example.id.clone()
    };
    if id.contains(['/', '\\']) || id.starts_with('.') {
        return Err(CollectionError::InvalidPath(id));
    }
    let body = (!example.body.is_empty()).then_some(example.body.as_str());
    let kind = body.map(|b| BodyKind::detect(&example.headers, b));
    for other in BodyKind::ALL {
        let path = body_file(&dir, &id, other);
        if Some(other) != kind && path.exists() {
            fs::remove_file(&path).map_err(|e| io_error(&path, e))?;
        }
    }
    if let (Some(kind), Some(body)) = (kind, body) {
        let path = body_file(&dir, &id, kind);
        fs::write(&path, body).map_err(|e| io_error(&path, e))?;
    }
    let saved = Example {
        id: id.clone(),
        ..example.clone()
    };
    let file = dir.join(format!("{id}.yaml"));
    fs::write(&file, example_yaml(&saved, kind)).map_err(|e| io_error(&file, e))?;
    Ok(saved)
}

pub fn delete_example(root: &Path, request_id: &str, id: &str) -> Result<(), CollectionError> {
    let dir = examples_dir(&request_path(root, request_id)?);
    if id.is_empty() || id.contains(['/', '\\']) {
        return Err(CollectionError::InvalidPath(id.to_string()));
    }
    for kind in BodyKind::ALL {
        let path = body_file(&dir, id, kind);
        if path.exists() {
            fs::remove_file(&path).map_err(|e| io_error(&path, e))?;
        }
    }
    let file = dir.join(format!("{id}.yaml"));
    fs::remove_file(&file).map_err(|e| io_error(&file, e))?;
    // An empty examples folder goes away.
    if fs::read_dir(&dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(false)
    {
        fs::remove_dir(&dir).map_err(|e| io_error(&dir, e))?;
    }
    Ok(())
}

/// Files of the examples of a request that are not in canonical form (rewritten unless `check`).
pub(crate) fn format_examples(
    root: &Path,
    request_id: &str,
    check: bool,
) -> Result<Vec<PathBuf>, CollectionError> {
    let dir = examples_dir(&request_path(root, request_id)?);
    let mut changed = Vec::new();
    for example in list_examples(root, request_id)? {
        let kind =
            (!example.body.is_empty()).then(|| BodyKind::detect(&example.headers, &example.body));
        let file = dir.join(format!("{}.yaml", example.id));
        let canonical = example_yaml(&example, kind);
        if fs::read_to_string(&file).map_err(|e| io_error(&file, e))? != canonical {
            changed.push(file.clone());
            if !check {
                fs::write(&file, canonical).map_err(|e| io_error(&file, e))?;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{
        create_request, delete_request, format_workspace, init_workspace, load_workspace,
    };

    fn example(name: &str, status: u16, body: &str) -> Example {
        Example {
            id: String::new(),
            name: name.into(),
            status,
            headers: vec![Header::new("Content-Type", "application/json")],
            body: body.into(),
        }
    }

    #[test]
    fn saves_lists_and_deletes_examples_next_to_the_request() {
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let entry = create_request(ws.path(), "", "Get order").unwrap();

        let ok = save_example(
            ws.path(),
            &entry.id,
            &example("200 OK", 200, "{\"id\": 1}\n"),
        )
        .unwrap();
        let missing = save_example(ws.path(), &entry.id, &example("Not found", 404, "")).unwrap();
        assert_eq!(
            (ok.id.as_str(), missing.id.as_str()),
            ("01-200-ok", "02-not-found")
        );
        assert!(ws
            .path()
            .join("01-get-order.examples/01-200-ok.body.json")
            .exists());
        assert_eq!(
            fs::read_to_string(ws.path().join("01-get-order.examples/01-200-ok.yaml")).unwrap(),
            "version: 1\nname: \"200 OK\"\nstatus: 200\nheaders:\n  - name: \"Content-Type\"\n    value: \"application/json\"\nbody: \"json\"\n"
        );

        let all = list_examples(ws.path(), &entry.id).unwrap();
        assert_eq!(all, vec![ok.clone(), missing.clone()]);

        // Editing keeps the file; the tree does not show the examples folder as a folder.
        let edited = save_example(ws.path(), &entry.id, &Example { status: 201, ..ok }).unwrap();
        assert_eq!(edited.id, "01-200-ok");
        assert_eq!(load_workspace(ws.path()).unwrap().tree.len(), 1);
        assert!(format_workspace(ws.path(), true).unwrap().is_empty());

        delete_example(ws.path(), &entry.id, "02-not-found").unwrap();
        assert_eq!(list_examples(ws.path(), &entry.id).unwrap().len(), 1);
        delete_request(ws.path(), &entry.id).unwrap();
        assert!(
            !ws.path().join("01-get-order.examples").exists(),
            "examples go with the request"
        );
    }

    #[test]
    fn examples_follow_their_request_when_organizing() {
        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let entry = create_request(ws.path(), "", "Get order").unwrap();
        save_example(ws.path(), &entry.id, &example("OK", 200, "{}")).unwrap();

        let renamed = crate::organize::rename(ws.path(), &entry.id, "Fetch order")
            .unwrap()
            .id;
        assert_eq!(list_examples(ws.path(), &renamed).unwrap().len(), 1);
        let copy = crate::organize::duplicate(ws.path(), &renamed).unwrap().id;
        assert_eq!(list_examples(ws.path(), &copy).unwrap()[0].name, "OK");
    }
}
