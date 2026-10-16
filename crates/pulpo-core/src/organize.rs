//! Organizing a collection: rename, move, reorder, duplicate and delete folders.
//!
//! Order lives in the `NN-` prefix of file and folder names. Operations keep every number
//! they can and only renumber what has to change, so a move shows up in git as a few
//! renames. A request moves with all its files (body, `.pre.js`, `.test.js`).

use crate::collection::{
    io_error, load_request, read_meta, relative_id, resolve, save_request, slugify, split_order,
    write_meta, BodyKind, CollectionError, REQUEST_SUFFIX, SKIPPED_DIRS,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// What an operation did: the id of the node it acted on (after the operation) and every
/// id that changed, as `(old, new)`. Ids inside a renamed folder change the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organized {
    pub id: String,
    pub renamed: Vec<(String, String)>,
}

const TEMP_PREFIX: &str = ".pulpo-tmp-";

/// Every file that belongs to a request, by suffix after its stem.
fn request_suffixes() -> Vec<String> {
    let mut suffixes = vec![REQUEST_SUFFIX.to_string()];
    for kind in BodyKind::ALL {
        suffixes.push(format!(".body.{}", kind.extension()));
    }
    suffixes.push(".pre.js".into());
    suffixes.push(".test.js".into());
    // The examples folder moves with its request (fs::rename works on folders too).
    suffixes.push(crate::examples::EXAMPLES_SUFFIX.into());
    suffixes
}

/// A request (by stem) or a subfolder (by directory name) of a folder.
#[derive(Debug, Clone)]
struct Child {
    /// Where it is now.
    dir: PathBuf,
    /// Stem (requests) or directory name (folders).
    name: String,
    folder: bool,
    /// The name without its `NN-` prefix.
    slug: String,
    order: Option<u32>,
}

impl Child {
    fn new(dir: &Path, name: String, folder: bool) -> Self {
        let (order, slug) = split_order(&name);
        Child {
            dir: dir.to_path_buf(),
            slug: slug.to_string(),
            order,
            name,
            folder,
        }
    }
}

/// The requests and subfolders of `dir`, in collection order.
fn children(dir: &Path) -> Result<Vec<Child>, CollectionError> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| io_error(dir, e))? {
        let entry = entry.map_err(|e| io_error(dir, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if entry.path().is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str())
                && !name.ends_with(crate::examples::EXAMPLES_SUFFIX)
            {
                found.push(Child::new(dir, name, true));
            }
        } else if let Some(stem) = name.strip_suffix(REQUEST_SUFFIX) {
            found.push(Child::new(dir, stem.to_string(), false));
        }
    }
    found.sort_by_key(|c| crate::collection::order_key(&c.name));
    Ok(found)
}

/// Renames a child (all its files, for a request) from `from_dir/from` to `to_dir/to`.
fn rename_child(
    from_dir: &Path,
    from: &str,
    to_dir: &Path,
    to: &str,
    folder: bool,
) -> Result<(), CollectionError> {
    if folder {
        let (src, dst) = (from_dir.join(from), to_dir.join(to));
        return fs::rename(&src, &dst).map_err(|e| io_error(&src, e));
    }
    for suffix in request_suffixes() {
        let src = from_dir.join(format!("{from}{suffix}"));
        if src.exists() {
            let dst = to_dir.join(format!("{to}{suffix}"));
            fs::rename(&src, &dst).map_err(|e| io_error(&src, e))?;
        }
    }
    Ok(())
}

/// Puts `list` into `dir` in that order. Each child keeps its number when it still fits
/// (bigger than the previous one; for `moved`, also smaller than the next one); the others
/// get the previous number plus one. Returns the final name of each child.
fn arrange(
    root: &Path,
    dir: &Path,
    list: &[Child],
    moved: Option<usize>,
    renamed: &mut Vec<(String, String)>,
) -> Result<Vec<String>, CollectionError> {
    let mut last = 0u32;
    let mut names = Vec::with_capacity(list.len());
    for (i, child) in list.iter().enumerate() {
        let next = list[i + 1..].iter().find_map(|c| c.order);
        let fits = |n: u32| n > last && (moved != Some(i) || next.is_none_or(|m| n < m));
        let number = match child.order {
            Some(n) if fits(n) => n,
            _ => last + 1,
        };
        last = number;
        names.push(format!("{number:02}-{}", child.slug));
    }
    // Two passes through temporary names, so swapping 01 and 02 never collides.
    let changed: Vec<usize> = (0..list.len())
        .filter(|&i| list[i].dir != dir || list[i].name != names[i])
        .collect();
    for &i in &changed {
        let c = &list[i];
        rename_child(&c.dir, &c.name, dir, &format!("{TEMP_PREFIX}{i}"), c.folder)?;
    }
    for &i in &changed {
        rename_child(
            dir,
            &format!("{TEMP_PREFIX}{i}"),
            dir,
            &names[i],
            list[i].folder,
        )?;
        // Children added by the operation (copies) had no id before.
        if !list[i].name.starts_with(TEMP_PREFIX) {
            renamed.push((
                id_of(root, &list[i].dir, &list[i].name, list[i].folder),
                id_of(root, dir, &names[i], list[i].folder),
            ));
        }
    }
    Ok(names)
}

fn id_of(root: &Path, dir: &Path, name: &str, folder: bool) -> String {
    if folder {
        relative_id(root, &dir.join(name))
    } else {
        relative_id(root, &dir.join(format!("{name}{REQUEST_SUFFIX}")))
    }
}

/// The folder (directory) and child for a request or folder id.
fn locate(root: &Path, id: &str) -> Result<(PathBuf, Child), CollectionError> {
    let path = resolve(root, id)?;
    let invalid = || CollectionError::InvalidPath(id.to_string());
    let parent = path.parent().ok_or_else(invalid)?.to_path_buf();
    let file_name = path
        .file_name()
        .ok_or_else(invalid)?
        .to_string_lossy()
        .into_owned();
    if let Some(stem) = file_name.strip_suffix(REQUEST_SUFFIX) {
        if !path.is_file() {
            return Err(invalid());
        }
        Ok((parent.clone(), Child::new(&parent, stem.to_string(), false)))
    } else if path.is_dir() && !id.is_empty() {
        Ok((parent.clone(), Child::new(&parent, file_name, true)))
    } else {
        Err(invalid())
    }
}

/// Renames a request or folder (`""` renames the workspace). Its number is kept; the file
/// name follows the new name. Returns the new id.
pub fn rename(root: &Path, id: &str, name: &str) -> Result<Organized, CollectionError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CollectionError::InvalidPath("a name is needed".into()));
    }
    if id.is_empty() {
        let mut meta = read_meta(root, "")?;
        meta.name = name.to_string();
        write_meta(root, "", &meta)?;
        return Ok(Organized::default());
    }
    let (dir, child) = locate(root, id)?;
    let new_name = match child.order {
        Some(n) => format!("{n:02}-{}", slugify(name)),
        None => slugify(name),
    };
    let new_id = id_of(root, &dir, &new_name, child.folder);
    if new_name != child.name {
        let target = if child.folder {
            dir.join(&new_name)
        } else {
            dir.join(format!("{new_name}{REQUEST_SUFFIX}"))
        };
        // On Windows and macOS only the letter case may differ; that is the same file.
        if target.exists() && !new_name.eq_ignore_ascii_case(&child.name) {
            return Err(CollectionError::InvalidPath(format!(
                "{} already exists",
                relative_id(root, &target)
            )));
        }
        rename_child(
            &dir,
            &child.name,
            &dir,
            &format!("{TEMP_PREFIX}rename"),
            child.folder,
        )?;
        rename_child(
            &dir,
            &format!("{TEMP_PREFIX}rename"),
            &dir,
            &new_name,
            child.folder,
        )?;
    }
    if child.folder {
        let mut meta = read_meta(root, &new_id)?;
        meta.name = name.to_string();
        write_meta(root, &new_id, &meta)?;
    } else {
        let mut entry = load_request(root, &new_id)?;
        entry.name = name.to_string();
        save_request(root, &entry)?;
    }
    let renamed = if new_id == id {
        Vec::new()
    } else {
        vec![(id.to_string(), new_id.clone())]
    };
    Ok(Organized {
        id: new_id,
        renamed,
    })
}

/// Moves a request or folder into `target` (a folder id, `""` = top level) at position
/// `index` among its children (after removing it, when it is the same folder). Returns
/// the new id; for a folder, ids inside it change the same way.
pub fn move_node(
    root: &Path,
    id: &str,
    target: &str,
    index: usize,
) -> Result<Organized, CollectionError> {
    let (source_dir, child) = locate(root, id)?;
    let target_dir = resolve(root, target)?;
    if target.ends_with(REQUEST_SUFFIX) || !target_dir.is_dir() {
        return Err(CollectionError::InvalidPath(target.to_string()));
    }
    if child.folder && (target == id || target.starts_with(&format!("{id}/"))) {
        return Err(CollectionError::InvalidPath(format!(
            "a folder cannot go inside itself: {id} → {target}"
        )));
    }
    let mut list: Vec<Child> = children(&target_dir)?
        .into_iter()
        .filter(|c| !(c.dir == source_dir && c.name == child.name))
        .collect();
    let at = index.min(list.len());
    list.insert(at, child.clone());
    let mut renamed = Vec::new();
    let names = arrange(root, &target_dir, &list, Some(at), &mut renamed)?;
    Ok(Organized {
        id: id_of(root, &target_dir, &names[at], child.folder),
        renamed,
    })
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), CollectionError> {
    fs::create_dir(to).map_err(|e| io_error(to, e))?;
    for entry in fs::read_dir(from).map_err(|e| io_error(from, e))? {
        let entry = entry.map_err(|e| io_error(from, e))?;
        let path = entry.path();
        let dst = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &dst)?;
        } else {
            fs::copy(&path, &dst).map_err(|e| io_error(&path, e))?;
        }
    }
    Ok(())
}

/// Copies a request (with its body and scripts) or a whole folder, named "<name> copy",
/// right after the original. Returns the id of the copy.
pub fn duplicate(root: &Path, id: &str) -> Result<Organized, CollectionError> {
    let (dir, child) = locate(root, id)?;
    let temp = format!("{TEMP_PREFIX}copy");
    let copy_name;
    if child.folder {
        copy_dir(&dir.join(&child.name), &dir.join(&temp))?;
        let temp_id = relative_id(root, &dir.join(&temp));
        let mut meta = read_meta(root, &temp_id)?;
        meta.name = format!("{} copy", meta.name);
        copy_name = meta.name.clone();
        write_meta(root, &temp_id, &meta)?;
    } else {
        let mut entry = load_request(root, id)?;
        entry.name = format!("{} copy", entry.name);
        entry.id = relative_id(root, &dir.join(format!("{temp}{REQUEST_SUFFIX}")));
        copy_name = entry.name.clone();
        save_request(root, &entry)?;
        let examples =
            crate::examples::examples_dir(&dir.join(format!("{}{REQUEST_SUFFIX}", child.name)));
        if examples.is_dir() {
            copy_dir(
                &examples,
                &crate::examples::examples_dir(&dir.join(format!("{temp}{REQUEST_SUFFIX}"))),
            )?;
        }
    }
    let mut list = children(&dir)?;
    let at = list
        .iter()
        .position(|c| c.name == child.name)
        .map(|i| i + 1)
        .unwrap_or(list.len());
    let mut copy = Child::new(&dir, temp, child.folder);
    copy.slug = slugify(&copy_name);
    copy.order = None;
    list.insert(at, copy);
    let mut renamed = Vec::new();
    let names = arrange(root, &dir, &list, Some(at), &mut renamed)?;
    Ok(Organized {
        id: id_of(root, &dir, &names[at], child.folder),
        renamed,
    })
}

/// Deletes a folder with everything inside (requests, subfolders, scripts).
pub fn delete_folder(root: &Path, id: &str) -> Result<(), CollectionError> {
    let (dir, child) = locate(root, id)?;
    if !child.folder {
        return Err(CollectionError::InvalidPath(format!(
            "{id} is not a folder"
        )));
    }
    let path = dir.join(&child.name);
    fs::remove_dir_all(&path).map_err(|e| io_error(&path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{
        create_folder, create_request, init_workspace, load_workspace, save_folder_scripts,
        Scripts, TreeNode,
    };

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        init_workspace(dir.path(), "Demo").unwrap();
        dir
    }

    fn names_in(root: &Path, folder: &str) -> Vec<String> {
        let mut out = Vec::new();
        let dir = resolve(root, folder).unwrap();
        for c in children(&dir).unwrap() {
            out.push(c.name);
        }
        out
    }

    fn with_files(root: &Path, folder: &str, name: &str) -> String {
        let mut entry = create_request(root, folder, name).unwrap();
        entry.request.body = Some("{\"a\": 1}".into());
        entry.scripts = Scripts {
            pre: "// pre".into(),
            test: "// test".into(),
        };
        save_request(root, &entry).unwrap();
        entry.id
    }

    #[test]
    fn reorders_with_few_renames_and_keeps_every_file() {
        let ws = workspace();
        with_files(ws.path(), "", "A");
        with_files(ws.path(), "", "B");
        let c = with_files(ws.path(), "", "C");
        assert_eq!(names_in(ws.path(), ""), ["01-a", "02-b", "03-c"]);

        let moved = move_node(ws.path(), &c, "", 0).unwrap();
        assert_eq!(
            moved.renamed,
            [
                ("03-c.pulpo.yaml", "01-c.pulpo.yaml"),
                ("01-a.pulpo.yaml", "02-a.pulpo.yaml"),
                ("02-b.pulpo.yaml", "03-b.pulpo.yaml"),
            ]
            .map(|(a, b)| (a.to_string(), b.to_string()))
        );
        let moved = moved.id;
        assert_eq!(moved, "01-c.pulpo.yaml");
        assert_eq!(names_in(ws.path(), ""), ["01-c", "02-a", "03-b"]);
        for suffix in [".pulpo.yaml", ".body.json", ".pre.js", ".test.js"] {
            assert!(ws.path().join(format!("01-c{suffix}")).exists(), "{suffix}");
        }
        assert_eq!(
            load_request(ws.path(), &moved).unwrap().scripts.test,
            "// test"
        );

        // Moving to the end keeps the others' numbers.
        let a = move_node(ws.path(), "02-a.pulpo.yaml", "", 2).unwrap().id;
        assert_eq!(names_in(ws.path(), ""), ["01-c", "03-b", "04-a"]);
        assert_eq!(a, "04-a.pulpo.yaml");
    }

    #[test]
    fn moves_requests_and_folders_between_folders() {
        let ws = workspace();
        let orders = create_folder(ws.path(), "", "Orders").unwrap();
        let users = create_folder(ws.path(), "", "Users").unwrap();
        with_files(ws.path(), &users, "List");
        let get = with_files(ws.path(), &orders, "Get");
        save_folder_scripts(
            ws.path(),
            &orders,
            &Scripts {
                pre: "// folder".into(),
                test: String::new(),
            },
        )
        .unwrap();

        let moved = move_node(ws.path(), &get, &users, 0).unwrap().id;
        assert_eq!(moved, "02-users/01-get.pulpo.yaml");
        assert_eq!(names_in(ws.path(), &users), ["01-get", "02-list"]);
        assert!(names_in(ws.path(), &orders).is_empty());

        let folder = move_node(ws.path(), &orders, &users, 5).unwrap().id;
        assert_eq!(folder, "02-users/03-orders");
        assert!(
            ws.path().join("02-users/03-orders/_folder.pre.js").exists(),
            "folder scripts move too"
        );

        let err = move_node(ws.path(), "02-users", "02-users/03-orders", 0).unwrap_err();
        assert!(err.to_string().contains("inside itself"));
        assert!(load_workspace(ws.path()).is_ok());
    }

    #[test]
    fn renames_keeping_the_number() {
        let ws = workspace();
        let folder = create_folder(ws.path(), "", "Orders").unwrap();
        let id = with_files(ws.path(), &folder, "Get order");

        let new_id = rename(ws.path(), &id, "Fetch order").unwrap().id;
        assert_eq!(new_id, "01-orders/01-fetch-order.pulpo.yaml");
        let entry = load_request(ws.path(), &new_id).unwrap();
        assert_eq!(entry.name, "Fetch order");
        assert_eq!(entry.request.body.as_deref(), Some("{\"a\": 1}"));
        assert!(!ws.path().join("01-orders/01-get-order.pre.js").exists());

        let folder = rename(ws.path(), &folder, "Pedidos y pagos").unwrap().id;
        assert_eq!(folder, "01-pedidos-y-pagos");
        assert_eq!(
            read_meta(ws.path(), &folder).unwrap().name,
            "Pedidos y pagos"
        );

        // Only the case changes: same file on Windows and macOS.
        rename(
            ws.path(),
            "01-pedidos-y-pagos/01-fetch-order.pulpo.yaml",
            "FETCH order",
        )
        .unwrap();
        rename(ws.path(), "", "Shop").unwrap();
        assert_eq!(load_workspace(ws.path()).unwrap().name, "Shop");

        assert!(rename(
            ws.path(),
            "01-pedidos-y-pagos/01-fetch-order.pulpo.yaml",
            " "
        )
        .is_err());
    }

    #[test]
    fn duplicates_right_after_the_original() {
        let ws = workspace();
        let a = with_files(ws.path(), "", "A");
        with_files(ws.path(), "", "B");
        let copied = duplicate(ws.path(), &a).unwrap();
        assert_eq!(
            copied.renamed,
            [("02-b.pulpo.yaml".to_string(), "03-b.pulpo.yaml".to_string())],
            "the copy itself is new, not renamed"
        );
        let copy = copied.id;
        assert_eq!(copy, "02-a-copy.pulpo.yaml");
        assert_eq!(names_in(ws.path(), ""), ["01-a", "02-a-copy", "03-b"]);
        let entry = load_request(ws.path(), &copy).unwrap();
        assert_eq!(entry.name, "A copy");
        assert_eq!(entry.scripts.pre, "// pre");

        let folder = create_folder(ws.path(), "", "Orders").unwrap();
        with_files(ws.path(), &folder, "Get");
        let copy = duplicate(ws.path(), &folder).unwrap().id;
        assert_eq!(copy, "05-orders-copy");
        assert_eq!(read_meta(ws.path(), &copy).unwrap().name, "Orders copy");
        assert!(ws.path().join("05-orders-copy/01-get.body.json").exists());

        delete_folder(ws.path(), &copy).unwrap();
        assert!(!ws.path().join(&copy).exists());
        assert!(delete_folder(ws.path(), "01-a.pulpo.yaml").is_err());
    }

    #[test]
    fn orders_past_99_numerically() {
        let ws = workspace();
        for name in ["100-late", "11-early", "9-first"] {
            fs::write(
                ws.path().join(format!("{name}.pulpo.yaml")),
                format!("version: 1\nname: \"{name}\"\nprotocol: \"http\"\nmethod: \"GET\"\nurl: \"u\"\n"),
            )
            .unwrap();
        }
        let tree = load_workspace(ws.path()).unwrap().tree;
        let names: Vec<_> = tree
            .iter()
            .map(|n| match n {
                TreeNode::Request(e) => e.name.clone(),
                TreeNode::Folder { name, .. } => name.clone(),
            })
            .collect();
        assert_eq!(names, ["9-first", "11-early", "100-late"]);
    }
}
