//! Filesystem mutations run off the UI; open documents retain their identities.
use super::Document;
use rxui::prelude::*;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use velk_plugin_contracts::host::FileOperation;

struct Outcome {
    renamed: Vec<(u64, PathBuf)>,
    created: Option<PathBuf>,
    moved: Option<(PathBuf, PathBuf)>,
}

fn destination(root: &Path, path: &Path) -> io::Result<PathBuf> {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(io::Error::other("parent traversal is not allowed"));
    }
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let parent = fs::canonicalize(
        path.parent()
            .ok_or_else(|| io::Error::other("path needs a parent"))?,
    )?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("path needs a name"))?;
    let path = parent.join(name);
    if !path.starts_with(root) || path == root {
        return Err(io::Error::other(
            "file operation must stay inside the project folder",
        ));
    }
    Ok(path)
}
fn existing(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let path = destination(root, path)?;
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        return Err(io::Error::other(
            "only regular files and directories can be changed",
        ));
    }
    Ok(path)
}
fn run(root: PathBuf, operation: FileOperation, open: Vec<(u64, PathBuf)>) -> io::Result<Outcome> {
    let root = fs::canonicalize(root)?;
    let mut result = Outcome {
        renamed: vec![],
        created: None,
        moved: None,
    };
    match operation {
        FileOperation::CreateFile { path } => {
            let path = destination(&root, &path)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            result.created = Some(path);
        }
        FileOperation::CreateDirectory { path } => {
            fs::create_dir(destination(&root, &path)?)?;
        }
        FileOperation::Rename {
            source,
            destination: target,
        } => {
            let source = existing(&root, &source)?;
            let target = destination(&root, &target)?;
            if source == target {
                return Ok(result);
            }
            if target.starts_with(&source) {
                return Err(io::Error::other("cannot move a directory into itself"));
            }
            for (id, path) in &open {
                if let Ok(relative) = path.strip_prefix(&source) {
                    let next = if relative.as_os_str().is_empty() {
                        target.clone()
                    } else {
                        target.join(relative)
                    };
                    if open
                        .iter()
                        .any(|(other, path)| other != id && path == &next)
                    {
                        return Err(io::Error::other(
                            "destination is already open in another document",
                        ));
                    }
                    result.renamed.push((*id, next));
                }
            }
            if source.is_file() {
                // Hard links provide no-clobber publication; cross-device moves fail safely.
                fs::hard_link(&source, &target)?;
                if let Err(error) = fs::remove_file(&source) {
                    let _ = fs::remove_file(&target);
                    return Err(error);
                }
            } else {
                rename_directory(&source, &target)?;
            }
            result.moved = Some((source.clone(), target.clone()));
            if let Some(store) = crate::undo::Store::from_env() {
                let mut paths = vec![(source.clone(), target)];
                paths.extend(result.renamed.iter().filter_map(|(id, next)| {
                    open.iter()
                        .find(|(old_id, _)| old_id == id)
                        .map(|(_, previous)| (previous.clone(), next.clone()))
                }));
                paths.sort();
                paths.dedup();
                for (previous, next) in paths {
                    if let Err(error) = store.relocate(&previous, &next) {
                        tracing::warn!(%error, "Cannot move persistent undo archive");
                    }
                }
            }
        }
        FileOperation::Delete { path } => {
            let path = existing(&root, &path)?;
            if open.iter().any(|(_, p)| p.starts_with(&path)) {
                return Err(io::Error::other(
                    "close the affected documents before deleting this path",
                ));
            }
            if path.is_dir() {
                fs::remove_dir(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
    }
    Ok(result)
}
#[cfg(unix)]
fn rename_directory(source: &Path, target: &Path) -> io::Result<()> {
    // Reserve an empty destination atomically so rename cannot overwrite user data.
    fs::create_dir(target)?;
    if let Err(error) = fs::rename(source, target) {
        let _ = fs::remove_dir(target);
        return Err(error);
    }
    Ok(())
}
#[cfg(not(unix))]
fn rename_directory(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "safe directory moves are not supported on this platform yet",
    ))
}

impl Document {
    pub(super) fn mutate_project(
        &mut self,
        operation: FileOperation,
        cx: &mut Context<'_, Self>,
    ) -> Result<(), String> {
        if self.busy()
            || self.any_saving()
            || self.saving_as
            || self.opening.is_some()
            || self.lifecycle.close_intent().is_some()
        {
            return Err("editor is busy; try the file operation again".into());
        }
        let root = self
            .workspace
            .root()
            .ok_or("select a project folder before changing files")?
            .to_path_buf();
        let open = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .filter_map(|b| b.file.as_ref().map(|f| (b.id, f.path().to_path_buf())))
            .collect();
        self.file_operation = Some(
            cx.try_spawn_blocking(
                move || run(root, operation, open),
                |this, result, cx| {
                    this.file_operation = None;
                    match result {
                        Ok(Ok(outcome)) => {
                            if let Some((old, new)) = &outcome.moved
                                && let Some(source) = this
                                    .panels
                                    .ui
                                    .source
                                    .as_ref()
                                    .and_then(|p| p.to_path().ok())
                                && let Ok(relative) = source.strip_prefix(old)
                            {
                                let source = if relative.as_os_str().is_empty() {
                                    new.clone()
                                } else {
                                    new.join(relative)
                                };
                                this.panels.ui.source = Some(velk_panels::NativePath::new(&source));
                            }
                            for (id, path) in outcome.renamed {
                                // The operation locks document lifecycle; every captured ID still exists.
                                if let Err(error) = this.documents.rename(id, Some(path.clone())) {
                                    this.notice(Some(format!(
                                        "Cannot update renamed document: {error}"
                                    )));
                                    continue;
                                }
                                this.with_buffer(id, cx, |this, cx| {
                                    if let Some(state) = this.buffer.semantic.take() {
                                        state.service.shutdown();
                                    }
                                    this.buffer.file = this
                                        .buffer
                                        .file
                                        .as_ref()
                                        .map(|f| f.relocated(path.clone()));
                                    this.buffer.path = path;
                                    this.buffer.disk_stamp = None;
                                    this.buffer.disk_changed = false;
                                    this.buffer.disk_notice_dismissed = false;
                                    this.buffer.refresh_view_languages(cx);
                                    this.start_buffer_semantics(cx);
                                });
                            }
                            this.publish_buffer_snapshot(cx);
                            this.refresh_panels(cx);
                            this.schedule_recovery(cx);
                            this.notice(Some("File operation completed".into()));
                            if let Some(path) = outcome.created {
                                this.open_path(path, cx);
                            }
                        }
                        Ok(Err(error)) => {
                            this.notice(Some(format!("File operation failed: {error}")))
                        }
                        Err(error) => this.notice(Some(format!("File operation failed: {error}"))),
                    }
                },
            )
            .map_err(|e| e.to_string())?,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rename_keeps_document_identity_unsaved_edits_and_undo() {
        use crate::{document::tests::QueuedExecutor, file::FileState};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let source = root.join("old.rs");
        let target = root.join("new.txt");
        fs::write(&source, "original").unwrap();
        let (file, editor) = FileState::open(&source).unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, source.clone(), cx))
        });
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.workspace.set_root(root.clone());
                d.buffer
                    .editor
                    .update(cx, |editor, _| editor.insert("edited"));
                let id = d.buffer.id;
                d.split_editor(velk_editor_model::views::Axis::Horizontal, cx);
                assert_eq!(d.buffer.views.len(), 2);
                d.mutate_project(
                    FileOperation::Rename {
                        source: "old.rs".into(),
                        destination: "new.txt".into(),
                    },
                    cx,
                )
                .unwrap();
                assert!(d.busy());
                d.open_path(target.clone(), cx);
                assert!(d.opening.is_none());
                assert!(
                    d.mutate_project(
                        FileOperation::CreateFile {
                            path: "other".into()
                        },
                        cx
                    )
                    .is_err()
                );
                assert_eq!(id, d.buffer.id);
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.buffer.id, 1);
                assert_eq!(d.buffer.views.len(), 2);
                assert_eq!(d.documents.find_path(&target), Some(1));
                assert_eq!(d.documents.find_path(&source), None);
                assert_eq!(d.buffer.file.as_ref().unwrap().path(), target);
                assert!(d.buffer.editor.read(cx).buffer().is_dirty());
                assert!(
                    d.buffer
                        .editor
                        .read(cx)
                        .buffer()
                        .text()
                        .to_string()
                        .contains("edited")
                );
                d.buffer.editor.update(cx, |editor, _| {
                    editor.execute(velk_editor_model::EditCommand::Undo);
                });
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "original"
                );
                assert!(!d.buffer.editor.read(cx).buffer().is_dirty());
            })
        });
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
    }
    #[test]
    fn mutations_do_not_clobber_escape_follow_symlinks_or_delete_open_documents() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let file = root.join("one.rs");
        fs::write(&file, "original").unwrap();
        let execute = |op| run(root.clone(), op, vec![(7, file.clone())]);
        assert!(
            execute(FileOperation::CreateFile {
                path: "one.rs".into()
            })
            .is_err()
        );
        assert!(
            execute(FileOperation::CreateFile {
                path: "../outside".into()
            })
            .is_err()
        );
        assert!(
            execute(FileOperation::Delete {
                path: "one.rs".into()
            })
            .is_err()
        );
        fs::write(root.join("two.rs"), "other").unwrap();
        assert!(
            execute(FileOperation::Rename {
                source: "one.rs".into(),
                destination: "two.rs".into()
            })
            .is_err()
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "original");
        assert_eq!(fs::read_to_string(root.join("two.rs")).unwrap(), "other");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&file, root.join("link")).unwrap();
            assert!(
                execute(FileOperation::Delete {
                    path: "link".into()
                })
                .is_err()
            );
        }
        let result = execute(FileOperation::Rename {
            source: "one.rs".into(),
            destination: "renamed.rs".into(),
        })
        .unwrap();
        assert_eq!(result.renamed, vec![(7, root.join("renamed.rs"))]);
        assert!(!file.exists());
    }
    #[test]
    #[cfg(unix)]
    fn directory_moves_relocate_descendants_and_deletion_is_nonrecursive() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(root.join("old")).unwrap();
        fs::write(root.join("old/a"), "text").unwrap();
        assert!(
            run(
                root.clone(),
                FileOperation::Delete { path: "old".into() },
                vec![]
            )
            .is_err()
        );
        let outcome = run(
            root.clone(),
            FileOperation::Rename {
                source: "old".into(),
                destination: "new".into(),
            },
            vec![(3, root.join("old/a"))],
        )
        .unwrap();
        assert_eq!(outcome.renamed, vec![(3, root.join("new/a"))]);
        assert_eq!(fs::read_to_string(root.join("new/a")).unwrap(), "text");
        run(
            root.clone(),
            FileOperation::Delete {
                path: "new/a".into(),
            },
            vec![],
        )
        .unwrap();
        run(
            root.clone(),
            FileOperation::Delete { path: "new".into() },
            vec![],
        )
        .unwrap();
        run(
            root.clone(),
            FileOperation::CreateDirectory {
                path: "created".into(),
            },
            vec![],
        )
        .unwrap();
        let out = run(
            root.clone(),
            FileOperation::CreateFile {
                path: "created/new".into(),
            },
            vec![],
        )
        .unwrap();
        assert_eq!(out.created, Some(root.join("created/new")));
    }
}
