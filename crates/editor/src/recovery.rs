//! Debounced, atomic recovery snapshots. Source files are never written here.
//! An OS file lock prevents a second instance from claiming a live workspace.
use crate::{file::FileState, workspace::Workspace};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::Duration,
};
use velk_core::BufferSnapshot;
use velk_editor_widget::Editor;

pub struct PendingDocument {
    pub id: u64,
    pub path: PathBuf,
    pub file: Option<FileState>,
    pub buffer: BufferSnapshot,
    pub scroll: [f32; 2],
}
pub struct Pending {
    pub next_id: u64,
    pub documents: Vec<PendingDocument>,
    pub active: u64,
    pub workspace: Workspace,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedDocument {
    id: u64,
    #[serde(with = "crate::session::path")]
    path: PathBuf,
    file: Option<FileState>,
    buffer: String,
    scroll: [f32; 2],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    #[serde(default)]
    next_id: u64,
    version: u32,
    documents: Vec<SavedDocument>,
    active: u64,
    workspace: Workspace,
}
pub struct RestoredDocument {
    pub id: u64,
    pub path: PathBuf,
    pub file: Option<FileState>,
    pub editor: Editor,
}
pub struct Restored {
    pub next_id: u64,
    pub documents: Vec<RestoredDocument>,
    pub active: u64,
    pub workspace: Workspace,
}
impl Pending {
    fn encode(self) -> Result<Saved, String> {
        Ok(Saved {
            next_id: self.next_id,
            version: 1,
            active: self.active,
            workspace: self.workspace,
            documents: self
                .documents
                .into_iter()
                .map(|d| {
                    Ok(SavedDocument {
                        id: d.id,
                        path: d.path,
                        file: d.file,
                        buffer: String::from_utf8(d.buffer.encode()?).map_err(|e| e.to_string())?,
                        scroll: d.scroll,
                    })
                })
                .collect::<Result<_, String>>()?,
        })
    }
}
impl Saved {
    fn restore(self) -> Result<Restored, String> {
        let mut ids = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        if self.version != 1
            || self.documents.len() > 256
            || if self.documents.is_empty() {
                self.active != 0
            } else {
                !self.documents.iter().any(|d| d.id == self.active)
            }
        {
            return Err("invalid recovery workspace".into());
        }
        let documents = self
            .documents
            .into_iter()
            .map(|d| {
                if d.id == 0
                    || d.id == u64::MAX
                    || !ids.insert(d.id)
                    || d.file
                        .as_ref()
                        .is_some_and(|f| !paths.insert(f.path().to_path_buf()))
                {
                    return Err("invalid or duplicate recovered document identity".into());
                }
                let mut editor = Editor::from_session(d.buffer.as_bytes(), d.scroll)?;
                if let Some(file) = &d.file {
                    editor
                        .set_language(velk_editor_widget::Language::for_path(file.path()))
                        .map_err(|e| e.to_string())?;
                }
                Ok(RestoredDocument {
                    id: d.id,
                    path: d.path,
                    file: d.file,
                    editor,
                })
            })
            .collect::<Result<_, String>>()?;
        let minimum_next = ids.iter().next_back().copied().unwrap_or(0) + 1;
        let next_id = if self.next_id == 0 {
            minimum_next
        } else {
            self.next_id
        };
        if next_id < minimum_next || next_id == u64::MAX {
            return Err("invalid next document identity".into());
        }
        Ok(Restored {
            next_id,
            documents,
            active: self.active,
            workspace: self.workspace,
        })
    }
}
#[derive(Default)]
struct State {
    pending: Option<Pending>,
    submitted: u64,
    completed: u64,
    stopping: bool,
    discard: bool,
    error: Option<String>,
}
#[derive(Clone)]
pub struct Handle {
    state: Arc<Mutex<State>>,
    wake: mpsc::SyncSender<()>,
    completion: Arc<Condvar>,
}
impl Handle {
    pub fn update(&self, pending: Pending) {
        let mut state = self.state.lock().unwrap();
        if state.discard || state.stopping {
            return;
        }
        state.pending = Some(pending);
        state.submitted += 1;
        let _ = self.wake.try_send(());
    }
    /// Called only after all close/save/discard prompts succeeded.
    pub fn discard(&self) {
        let mut state = self.state.lock().unwrap();
        state.discard = true;
        state.pending = None;
        let _ = self.wake.try_send(());
    }
    /// Startup/teardown only. Never wait for recovery I/O on the UI thread.
    pub fn flush(&self) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        let submitted = state.submitted;
        let (state, timeout) = self
            .completion
            .wait_timeout_while(state, Duration::from_secs(10), |s| s.completed < submitted)
            .map_err(|e| e.to_string())?;
        if timeout.timed_out() {
            return Err("timed out persisting recovery snapshot".into());
        }
        state.error.clone().map_or(Ok(()), Err)
    }
    pub fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }
}
pub struct Store {
    pub handle: Handle,
    pub warning: Option<String>,
    worker: Option<thread::JoinHandle<()>>,
}
fn directory() -> Option<PathBuf> {
    std::env::var_os("VELK_RECOVERY_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            crate::settings::settings_path().and_then(|p| p.parent().map(|p| p.join("recovery")))
        })
}
impl Store {
    pub fn open(restore: bool) -> Result<(Self, Option<Restored>), String> {
        Self::open_in(
            directory().ok_or("cannot locate recovery directory")?,
            restore,
        )
    }
    fn open_in(directory: PathBuf, restore: bool) -> Result<(Self, Option<Restored>), String> {
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let mut restored = None;
        let mut claimed = None;
        let mut warning = None;
        if restore {
            let mut candidates: Vec<_> = fs::read_dir(&directory)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            candidates.sort_by_key(|p| {
                std::cmp::Reverse(fs::metadata(p).and_then(|m| m.modified()).ok())
            });
            for snapshot in candidates {
                let lock_path = snapshot.with_extension("lock");
                let Ok(lock) = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&lock_path)
                else {
                    continue;
                };
                if lock.try_lock().is_err() {
                    continue;
                }
                let read = (|| {
                    if fs::metadata(&snapshot).map_err(|e| e.to_string())?.len() > 128 * 1024 * 1024
                    {
                        return Err("recovery snapshot exceeds 128 MiB".into());
                    }
                    let saved: Saved =
                        serde_json::from_reader(File::open(&snapshot).map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?;
                    saved.restore()
                })();
                match read {
                    Ok(workspace) => {
                        restored = Some(workspace);
                        claimed = Some((lock, lock_path, snapshot));
                        break;
                    }
                    Err(error) => {
                        warning = Some(format!(
                            "Could not restore {}. Its recovery file was preserved: {error}",
                            snapshot.display()
                        ));
                        tracing::error!(%error, file = %snapshot.display(), "Recovery snapshot preserved but could not be restored")
                    }
                }
            }
        }
        let (lock, lock_path, snapshot) = match claimed {
            Some(claimed) => claimed,
            None => {
                let (lock, lock_path) = tempfile::Builder::new()
                    .prefix("workspace-")
                    .suffix(".lock")
                    .tempfile_in(&directory)
                    .map_err(|e| e.to_string())?
                    .keep()
                    .map_err(|e| e.to_string())?;
                lock.try_lock().map_err(|e| e.to_string())?;
                let snapshot = lock_path.with_extension("json");
                (lock, lock_path, snapshot)
            }
        };
        let (wake, receiver) = mpsc::sync_channel(1);
        let state = Arc::new(Mutex::new(State::default()));
        let completion = Arc::new(Condvar::new());
        let handle = Handle {
            state: state.clone(),
            wake,
            completion: completion.clone(),
        };
        let worker = thread::Builder::new()
            .name("velk-recovery".into())
            .spawn(move || {
                let _lock = lock;
                while receiver.recv().is_ok() {
                    // Bound the debounce even during continuous typing.
                    let stopping = state.lock().unwrap().stopping;
                    if !stopping {
                        thread::sleep(Duration::from_millis(200));
                    }
                    while receiver.try_recv().is_ok() {}
                    let (pending, stopping, discard, submitted) = {
                        let mut state = state.lock().unwrap();
                        (
                            state.pending.take(),
                            state.stopping,
                            state.discard,
                            state.submitted,
                        )
                    };
                    if discard {
                        if let Err(error) = fs::remove_file(&snapshot)
                            && error.kind() != io::ErrorKind::NotFound
                        {
                            state.lock().unwrap().error = Some(error.to_string());
                        }
                    } else if let Some(pending) = pending {
                        let result = pending.encode().and_then(|saved| write(&snapshot, &saved));
                        state.lock().unwrap().error = result.err();
                    }
                    state.lock().unwrap().completed = submitted;
                    completion.notify_all();
                    if stopping {
                        break;
                    }
                }
                drop(_lock);
                // Keep the lock inode stable for concurrent startup contenders.
                let _ = lock_path;
            })
            .map_err(|e| e.to_string())?;
        Ok((
            Self {
                handle,
                warning,
                worker: Some(worker),
            },
            restored,
        ))
    }
    /// Join during application teardown, never from a UI callback.
    pub fn shutdown(mut self) {
        self.handle.state.lock().unwrap().stopping = true;
        let _ = self.handle.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        self.handle.state.lock().unwrap().stopping = true;
        let _ = self.handle.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn write(path: &std::path::Path, saved: &Saved) -> Result<(), String> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("recovery file has no directory")?)
            .map_err(|e| e.to_string())?;
    serde_json::to_writer(file.as_file_mut(), saved).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    if file.as_file().metadata().map_err(|e| e.to_string())?.len() > 128 * 1024 * 1024 {
        return Err("recovery snapshot exceeds 128 MiB".into());
    }
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if let Ok(parent) = File::open(path.parent().unwrap()) {
        let _ = parent.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(source: &std::path::Path) -> Pending {
        let (file, mut editor) = FileState::open(source).unwrap();
        editor.insert("unsaved 🌍");
        let mut untitled = Editor::new("");
        untitled.insert("scratch text");
        let mut workspace = Workspace::default();
        workspace.set_root(source.parent().unwrap().to_path_buf());
        Pending {
            next_id: 20,
            active: 11,
            workspace,
            documents: vec![
                PendingDocument {
                    id: 7,
                    path: source.into(),
                    file: Some(file),
                    buffer: editor.buffer().snapshot(),
                    scroll: [0., 80.],
                },
                PendingDocument {
                    id: 11,
                    path: "Untitled 11".into(),
                    file: None,
                    buffer: untitled.buffer().snapshot(),
                    scroll: [0., 0.],
                },
            ],
        }
    }
    #[test]
    fn empty_workspace_persists_without_creating_an_untitled_document() {
        let directory = tempfile::tempdir().unwrap();
        let (store, _) = Store::open_in(directory.path().into(), false).unwrap();
        store.handle.update(Pending {
            next_id: 42,
            documents: vec![],
            active: 0,
            workspace: Workspace::default(),
        });
        store.handle.flush().unwrap();
        store.shutdown();
        let (store, restored) = Store::open_in(directory.path().into(), true).unwrap();
        let restored = restored.unwrap();
        assert!(restored.documents.is_empty());
        assert_eq!(restored.active, 0);
        assert_eq!(restored.next_id, 42);
        store.handle.discard();
        store.shutdown();
    }
    #[test]
    fn persists_unsaved_history_and_identities_skips_live_workspaces_and_discards_only_on_clean_close()
     {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        fs::write(&source, "original\r\n").unwrap();
        let recovery = directory.path().join("recovery");
        let (first, restored) = Store::open_in(recovery.clone(), true).unwrap();
        assert!(restored.is_none());
        first.handle.update(pending(&source));
        first.handle.flush().unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "original\r\n");
        let (other, restored) = Store::open_in(recovery.clone(), true).unwrap();
        assert!(
            restored.is_none(),
            "a second instance must not claim the live workspace"
        );
        other.shutdown();
        first.shutdown(); // simulate an interrupted session, without discard
        fs::write(&source, "external").unwrap();
        let (resumed, restored) = Store::open_in(recovery.clone(), true).unwrap();
        let restored = restored.unwrap();
        assert_eq!(restored.active, 11);
        assert_eq!(restored.next_id, 20);
        assert_eq!(restored.workspace.root(), source.parent());
        assert_eq!(
            restored.documents.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![7, 11]
        );
        assert_eq!(
            restored.documents[0].editor.buffer().text().to_string(),
            "unsaved 🌍original\n"
        );
        assert!(restored.documents[0].editor.buffer().is_dirty());
        assert_eq!(restored.documents[0].editor.scroll_offset(), [0., 80.]);
        assert!(
            restored.documents[0]
                .file
                .as_ref()
                .unwrap()
                .changed_on_disk()
                .unwrap()
        );
        assert!(
            restored.documents[0]
                .file
                .clone()
                .unwrap()
                .save(restored.documents[0].editor.buffer().text().clone())
                .is_err()
        );
        let mut history = velk_core::Buffer::restore(
            &restored.documents[0]
                .editor
                .buffer()
                .snapshot()
                .encode()
                .unwrap(),
        )
        .unwrap();
        history.undo();
        assert_eq!(history.text().to_string(), "original\n");
        assert!(restored.documents[1].file.is_none());
        assert_eq!(
            restored.documents[1].editor.buffer().text().to_string(),
            "scratch text"
        );
        resumed.handle.discard();
        resumed.shutdown();
        let (next, restored) = Store::open_in(recovery, true).unwrap();
        assert!(restored.is_none());
        next.shutdown();
        assert_eq!(fs::read_to_string(&source).unwrap(), "external");
    }
    #[test]
    fn malformed_recovery_is_preserved_and_duplicate_document_ids_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let corrupt = directory.path().join("broken.json");
        fs::write(&corrupt, b"not valid JSON").unwrap();
        let (store, restored) = Store::open_in(directory.path().into(), true).unwrap();
        assert!(restored.is_none());
        store.shutdown();
        assert_eq!(fs::read(&corrupt).unwrap(), b"not valid JSON");
        let source = directory.path().join("file.txt");
        fs::write(&source, "text").unwrap();
        let mut saved = pending(&source).encode().unwrap();
        saved.documents[1].id = saved.documents[0].id;
        assert!(saved.restore().is_err());
        let mut saved = pending(&source).encode().unwrap();
        saved.active = 999;
        assert!(saved.restore().is_err());
    }
}
