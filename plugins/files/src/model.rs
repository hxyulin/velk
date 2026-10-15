use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use velk_panels::{Content, Context, Effect, NativePath, Node, Panel, Request, Slot, Snapshot};
const MAX_DIRECTORY: usize = 1_000;
const MAX_ENTRIES: usize = 1_800;
const MAX_ROWS: usize = 2_000;
#[derive(Clone)]
struct Item {
    path: PathBuf,
    directory: bool,
    symlink: bool,
    depth: u8,
}
#[derive(Clone)]
struct Listing {
    entries: Vec<Item>,
    notice: Option<String>,
}
#[derive(Clone, Default)]
pub(super) struct Model {
    root: Option<PathBuf>,
    directories: BTreeMap<PathBuf, Listing>,
    nodes: BTreeMap<String, Item>,
    revision: u64,
    #[cfg(test)]
    pub reads: usize,
}
pub(super) fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("file tree request cancelled".into())
    } else {
        Ok(())
    }
}
fn id(path: &Path) -> String {
    let mut hash = blake3::Hasher::new();
    match NativePath::new(path) {
        NativePath::Unix(bytes) => {
            hash.update(&bytes);
        }
        NativePath::Windows(units) => {
            for unit in units {
                hash.update(&unit.to_le_bytes());
            }
        }
    }
    hash.finalize().to_hex().to_string()
}
pub(super) fn caption(value: impl AsRef<str>) -> String {
    let mut result = String::new();
    for c in value.as_ref().chars() {
        let c = if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
            '�'
        } else {
            c
        };
        if result.len() + c.len_utf8() > 188 {
            result.push('…');
            break;
        }
        result.push(c);
    }
    if result.is_empty() {
        result.push('?');
    }
    result
}
fn root(document: NativePath, cancel: &AtomicBool) -> Result<PathBuf, String> {
    let source = document.to_path()?;
    let path = match std::fs::canonicalize(&source) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => source,
        Err(error) => return Err(error.to_string()),
    };
    let directory = if path.is_dir() {
        path.as_path()
    } else {
        path.parent().ok_or("document has no directory")?
    };
    let mut cargo = None;
    for ancestor in directory.ancestors() {
        cancelled(cancel)?;
        if ancestor.join(".git").exists() {
            return Ok(ancestor.into());
        }
        if cargo.is_none() && ancestor.join("Cargo.toml").is_file() {
            cargo = Some(ancestor.to_path_buf());
        }
    }
    Ok(cargo.unwrap_or_else(|| directory.into()))
}
impl Model {
    #[cfg(test)]
    pub fn apply(
        &mut self,
        request: Request,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Snapshot, String> {
        self.apply_watched(request, cancel, &mut |_| {})
    }
    pub fn watched_paths(&self) -> Vec<PathBuf> {
        self.directories.keys().cloned().collect()
    }
    pub fn root_path(&self) -> Option<&Path> {
        self.root.as_deref()
    }
    pub fn apply_watched(
        &mut self,
        request: Request,
        cancel: &Arc<AtomicBool>,
        watch: &mut impl FnMut(&Path),
    ) -> Result<Snapshot, String> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("files.request");
        cancelled(cancel)?;
        match request {
            Request::Describe { context } => {
                context.validate()?;
                self.directories.clear();
                self.nodes.clear();
                self.root = if let Some(path) = context.root {
                    let path = path.to_path()?;
                    let path = std::fs::canonicalize(&path).unwrap_or(path);
                    if path.exists() && !path.is_dir() {
                        return Err("workspace root is not a directory".into());
                    }
                    Some(path)
                } else if self.root.is_some() {
                    self.root.clone()
                } else {
                    context
                        .document
                        .map(|path| root(path, cancel))
                        .transpose()?
                };
                if let Some(path) = self.root.clone() {
                    let expanded: BTreeSet<_> = context.expanded.into_iter().collect();
                    if !context.initialized || expanded.contains(&id(&path)) {
                        self.expand(
                            &Item {
                                path,
                                directory: true,
                                symlink: false,
                                depth: 0,
                            },
                            &expanded,
                            cancel,
                            watch,
                        )?;
                    }
                }
            }
            Request::Expand {
                panel,
                revision,
                node,
                expanded,
            } => {
                self.check(&panel, revision)?;
                Context {
                    expanded: expanded.clone(),
                    ..Default::default()
                }
                .validate()?;
                let item = self
                    .nodes
                    .get(&node)
                    .filter(|i| i.directory)
                    .ok_or("unknown directory row")?
                    .clone();
                self.expand(&item, &expanded.into_iter().collect(), cancel, watch)?;
            }
            Request::Invoke {
                panel,
                revision,
                node,
                action,
            } => {
                self.check(&panel, revision)?;
                if action != "open" {
                    return Err("unknown file action".into());
                }
                let item = self
                    .nodes
                    .get(&node)
                    .filter(|i| !i.directory)
                    .ok_or("unknown file row")?;
                let path = NativePath::new(&item.path);
                let mut snapshot = self.snapshot()?;
                snapshot.effects.push(Effect::OpenFile { path });
                snapshot.validate()?;
                return Ok(snapshot);
            }
            Request::Input { .. }
            | Request::Updates { .. }
            | Request::Changes
            | Request::Poll { .. }
            | Request::Cancel { .. } => {
                return Err("worker received a control request".into());
            }
        }
        cancelled(cancel)?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("file tree revision exhausted")?;
        self.snapshot()
    }
    fn check(&self, panel: &str, revision: u64) -> Result<(), String> {
        if panel != "files" || revision != self.revision {
            Err("stale or unknown file tree snapshot".into())
        } else {
            Ok(())
        }
    }
    fn expand(
        &mut self,
        item: &Item,
        expanded: &BTreeSet<String>,
        cancel: &Arc<AtomicBool>,
        watch: &mut impl FnMut(&Path),
    ) -> Result<(), String> {
        cancelled(cancel)?;
        if item.depth >= 32 {
            return Ok(());
        }
        watch(&item.path);
        if !self.directories.contains_key(&item.path) {
            let used: usize = self.directories.values().map(|d| d.entries.len()).sum();
            let limit = MAX_DIRECTORY.min(MAX_ENTRIES.saturating_sub(used));
            #[cfg(test)]
            {
                self.reads += 1;
            }
            let mut listing = Listing {
                entries: Vec::new(),
                notice: None,
            };
            let result = (|| -> std::io::Result<()> {
                let _trace = velk_plugin_sdk::diagnostics::scope!("files.read_directory");
                for (scanned, entry) in std::fs::read_dir(&item.path)?.enumerate() {
                    if cancel.load(Ordering::Acquire) {
                        break;
                    }
                    if scanned >= limit {
                        listing.notice = Some("[Directory listing limit reached]".into());
                        break;
                    }
                    let entry = entry?;
                    let kind = entry.file_type()?;
                    // Symlinks are leaves: no traversal cycles or unexpected scans.
                    if kind.is_dir() || kind.is_file() || kind.is_symlink() {
                        listing.entries.push(Item {
                            path: entry.path(),
                            directory: kind.is_dir(),
                            symlink: kind.is_symlink(),
                            depth: item.depth + 1,
                        });
                    }
                }
                Ok(())
            })();
            cancelled(cancel)?;
            if let Err(error) = result {
                listing.notice = Some(format!("[Cannot read directory: {error}]"));
            }
            listing.entries.sort_by(|a, b| {
                (!a.directory, a.path.file_name()).cmp(&(!b.directory, b.path.file_name()))
            });
            self.directories.insert(item.path.clone(), listing);
        }
        let children: Vec<_> = self.directories[&item.path]
            .entries
            .iter()
            .filter(|i| i.directory && expanded.contains(&id(&i.path)))
            .cloned()
            .collect();
        for child in children {
            self.expand(&child, expanded, cancel, watch)?;
        }
        Ok(())
    }
    fn append(&mut self, item: Item, rows: &mut Vec<Node>) {
        if rows.len() >= MAX_ROWS {
            return;
        }
        let node_id = id(&item.path);
        let name = item
            .path
            .file_name()
            .unwrap_or(item.path.as_os_str())
            .to_string_lossy();
        let suffix = if item.symlink {
            " ↗"
        } else if item.directory && item.depth == 32 {
            " [depth limit]"
        } else {
            ""
        };
        rows.push(Node {
            id: node_id.clone(),
            label: caption(format!("{name}{suffix}")),
            depth: item.depth,
            branch: item.directory,
            expanded: item.depth == 0,
            lazy: item.directory && item.depth < 32 && !self.directories.contains_key(&item.path),
            action: (!item.directory).then(|| "open".into()),
        });
        self.nodes.insert(node_id.clone(), item.clone());
        if let Some(listing) = self.directories.get(&item.path) {
            let entries = listing.entries.clone();
            let notice = listing.notice.clone();
            for child in entries {
                self.append(child, rows);
            }
            if let Some(notice) = notice
                && rows.len() < MAX_ROWS
                && item.depth < 32
            {
                rows.push(Node {
                    id: format!("notice:{node_id}"),
                    label: caption(notice),
                    depth: item.depth + 1,
                    branch: false,
                    expanded: false,
                    lazy: false,
                    action: None,
                });
            }
        }
    }
    fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.nodes.clear();
        let content = if let Some(path) = self.root.clone() {
            let mut rows = Vec::new();
            self.append(
                Item {
                    path,
                    directory: true,
                    symlink: false,
                    depth: 0,
                },
                &mut rows,
            );
            if rows.len() >= MAX_ROWS {
                rows.pop();
                rows.push(Node {
                    id: "row-limit".into(),
                    label: "[Tree row limit reached]".into(),
                    depth: 1,
                    branch: false,
                    expanded: false,
                    lazy: false,
                    action: None,
                });
            }
            Content::Tree(rows)
        } else {
            Content::Text("Open a file to show its project directory.".into())
        };
        let snapshot = Snapshot {
            watch: true,
            stream: false,
            panels: vec![Panel {
                id: "files".into(),
                title: "Files".into(),
                slot: Slot::Left,
                revision: self.revision,
                content,
            }],
            effects: Vec::new(),
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(document: &Path) -> Context {
        Context {
            document: Some(NativePath::new(document)),
            ..Default::default()
        }
    }
    fn rows(snapshot: &Snapshot) -> &[Node] {
        let Content::Tree(rows) = &snapshot.panels[0].content else {
            panic!("expected tree")
        };
        rows
    }
    fn apply(model: &mut Model, request: Request) -> Snapshot {
        model
            .apply(request, &Arc::new(AtomicBool::new(false)))
            .unwrap()
    }
    #[test]
    fn only_open_directories_are_read_and_refresh_rehydrates_expansions() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/closed/deep")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("Cargo.toml"), "manifest").unwrap();
        let document = root.join("src/main.rs");
        let mut model = Model::default();
        let first = apply(
            &mut model,
            Request::Describe {
                context: context(&document),
            },
        );
        assert_eq!(model.reads, 1);
        assert!(!rows(&first).iter().any(|n| n.label == "main.rs"));
        assert_eq!(
            rows(&first).last().unwrap().label,
            "Cargo.toml",
            "directories sort first"
        );
        let src = rows(&first)
            .iter()
            .find(|n| n.label == "src")
            .unwrap()
            .id
            .clone();
        let expanded = vec![rows(&first)[0].id.clone(), src.clone()];
        let second = apply(
            &mut model,
            Request::Expand {
                panel: "files".into(),
                revision: first.panels[0].revision,
                node: src.clone(),
                expanded: expanded.clone(),
            },
        );
        assert_eq!(model.reads, 2);
        assert!(rows(&second).iter().any(|n| n.label == "closed" && n.lazy));
        assert!(!rows(&second).iter().any(|n| n.label == "deep"));
        let leaf = rows(&second)
            .iter()
            .find(|n| n.label == "main.rs")
            .unwrap()
            .id
            .clone();
        let open = apply(
            &mut model,
            Request::Invoke {
                panel: "files".into(),
                revision: second.panels[0].revision,
                node: leaf.clone(),
                action: "open".into(),
            },
        );
        assert_eq!(model.reads, 2, "file activation performs no filesystem I/O");
        assert_eq!(
            open.effects,
            vec![Effect::OpenFile {
                path: NativePath::new(&document)
            }]
        );
        assert!(
            model
                .apply(
                    Request::Invoke {
                        panel: "files".into(),
                        revision: 0,
                        node: leaf,
                        action: "open".into()
                    },
                    &Arc::new(AtomicBool::new(false))
                )
                .is_err()
        );
        let mut restored = Model::default();
        let refreshed = apply(
            &mut restored,
            Request::Describe {
                context: Context {
                    initialized: true,
                    expanded: expanded.clone(),
                    ..context(&document)
                },
            },
        );
        assert_eq!(restored.reads, 2);
        assert_eq!(
            rows(&refreshed),
            rows(&second),
            "identities survive worker/plugin restart"
        );
        let collapsed = apply(
            &mut restored,
            Request::Describe {
                context: Context {
                    initialized: true,
                    expanded: vec![src],
                    ..context(&document)
                },
            },
        );
        assert_eq!(
            restored.reads, 2,
            "collapsed root performs no new directory reads"
        );
        assert_eq!(rows(&collapsed).len(), 1);
        assert!(rows(&collapsed)[0].lazy);
        let reopened = apply(
            &mut restored,
            Request::Expand {
                panel: "files".into(),
                revision: collapsed.panels[0].revision,
                node: expanded[0].clone(),
                expanded,
            },
        );
        assert_eq!(restored.reads, 4);
        assert_eq!(rows(&reopened), rows(&second));
    }
    #[test]
    fn removed_directory_is_a_visible_error_and_listing_limits_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("gone")).unwrap();
        let mut model = Model::default();
        let first = apply(
            &mut model,
            Request::Describe {
                context: context(dir.path()),
            },
        );
        let gone = rows(&first)
            .iter()
            .find(|n| n.label == "gone")
            .unwrap()
            .id
            .clone();
        std::fs::remove_dir(dir.path().join("gone")).unwrap();
        let failed = apply(
            &mut model,
            Request::Expand {
                panel: "files".into(),
                revision: first.panels[0].revision,
                node: gone,
                expanded: Vec::new(),
            },
        );
        assert!(
            rows(&failed)
                .iter()
                .any(|n| n.label.contains("Cannot read directory"))
        );
        for i in 0..MAX_DIRECTORY + 1 {
            std::fs::write(dir.path().join(format!("file-{i}")), "").unwrap();
        }
        let limited = apply(
            &mut model,
            Request::Describe {
                context: context(dir.path()),
            },
        );
        assert!(rows(&limited).len() <= MAX_DIRECTORY + 2);
        assert!(
            rows(&limited)
                .iter()
                .any(|n| n.label.contains("listing limit"))
        );
        assert!(velk_panels::encode(&limited).unwrap().len() < velk_panels::MAX_BYTES);
    }
    #[cfg(unix)]
    #[test]
    fn native_names_are_lossless_actions_and_directory_symlinks_are_not_followed() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let name = if cfg!(target_os = "macos") {
            b"name-\n.txt".to_vec()
        } else {
            b"name-\xff\n.txt".to_vec()
        };
        let path = root.join(std::ffi::OsString::from_vec(name));
        std::fs::write(&path, "hello").unwrap();
        symlink(&root, root.join("cycle")).unwrap();
        let mut model = Model::default();
        let first = apply(
            &mut model,
            Request::Describe {
                context: context(&root),
            },
        );
        assert_eq!(model.reads, 1);
        let link = rows(&first)
            .iter()
            .find(|n| n.label.contains("cycle"))
            .unwrap();
        assert!(!link.branch && !link.lazy);
        let leaf = rows(&first)
            .iter()
            .find(|n| n.label.starts_with("name-"))
            .unwrap();
        assert!(!leaf.label.contains('\n'));
        let open = apply(
            &mut model,
            Request::Invoke {
                panel: "files".into(),
                revision: first.panels[0].revision,
                node: leaf.id.clone(),
                action: "open".into(),
            },
        );
        let Effect::OpenFile { path: actual } = &open.effects[0];
        assert_eq!(actual.to_path().unwrap(), path);
    }
}
