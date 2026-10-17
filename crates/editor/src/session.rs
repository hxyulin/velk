//! Private process handoff, not a persistent workspace/distribution format.
use crate::{document::reload::UiState, file::FileState};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufReader, BufWriter, Write},
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};
use velk_core::BufferSnapshot;
use velk_editor_widget::Editor;
pub const SNAPSHOT_ENV: &str = "VELK_RELOAD_SESSION";
pub const READY_ENV: &str = "VELK_RELOAD_READY";

pub struct RestoredBuffer {
    pub id: u64,
    pub file: Option<FileState>,
    pub editor: Editor,
    pub path: PathBuf,
    pub search_open: bool,
    pub search_query: velk_search::Query,
}
pub struct PendingBuffer {
    pub id: u64,
    pub path: PathBuf,
    pub file: Option<FileState>,
    pub buffer: BufferSnapshot,
    pub scroll: [f32; 2],
    pub search_open: bool,
    pub search_query: velk_search::Query,
}
#[derive(Serialize, Deserialize)]
pub struct SessionBuffer {
    #[serde(default)]
    id: u64,
    #[serde(with = "path")]
    path: PathBuf,
    file: Option<FileState>,
    buffer: String,
    scroll: [f32; 2],
    #[serde(default)]
    search_open: bool,
    #[serde(default)]
    search_query: velk_search::Query,
}
impl SessionBuffer {
    pub fn editor(&self) -> Result<Editor, String> {
        let mut editor = crate::undo::restore_editor(self.buffer.as_bytes(), self.scroll)?;
        editor
            .set_language(velk_editor_widget::Language::for_path(&self.path))
            .map_err(|e| e.to_string())?;
        Ok(editor)
    }
}
impl PendingBuffer {
    fn encode(self) -> Result<SessionBuffer, String> {
        Ok(SessionBuffer {
            id: self.id,
            path: self.path,
            file: self.file,
            buffer: String::from_utf8(self.buffer.encode()?).map_err(|e| e.to_string())?,
            scroll: self.scroll,
            search_open: self.search_open,
            search_query: self.search_query,
        })
    }
}
pub struct Pending {
    pub id: u64,
    pub next_id: u64,
    pub path: PathBuf,
    pub file: Option<FileState>,
    pub buffer: Option<BufferSnapshot>,
    pub scroll: [f32; 2],
    pub ui: UiState,
    pub size: [f64; 2],
    pub position: Option<[i32; 2]>,
    pub maximized: bool,
    pub client: Option<velk_plugin_host::Client>,
    pub buffers: Vec<PendingBuffer>,
    pub active_index: usize,
}
#[derive(Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub id: u64,
    version: u32,
    #[serde(with = "path")]
    pub path: PathBuf,
    pub file: Option<FileState>,
    buffer: Option<String>,
    pub scroll: [f32; 2],
    pub ui: UiState,
    pub size: [f64; 2],
    #[serde(default)]
    pub position: Option<[i32; 2]>,
    #[serde(default)]
    pub maximized: bool,
    pub plugin_states: Vec<(String, Vec<u8>)>,
    #[serde(default)]
    pub buffers: Vec<SessionBuffer>,
    #[serde(default)]
    pub active_index: usize,
}
impl Pending {
    #[tracing::instrument(level = "debug", skip_all)]
    fn encode(self) -> Result<Session, String> {
        let plugin_states = if let Some(client) = &self.client {
            pollster::block_on(client.snapshot_plugins(self.ui.plugins.clone()))?
        } else {
            Vec::new()
        };
        Ok(Session {
            next_id: self.next_id,
            id: self.id,
            version: 1,
            buffers: self
                .buffers
                .into_iter()
                .map(PendingBuffer::encode)
                .collect::<Result<_, _>>()?,
            active_index: self.active_index,
            plugin_states,
            path: self.path,
            file: self.file,
            buffer: self
                .buffer
                .map(|buffer| String::from_utf8(buffer.encode()?).map_err(|e| e.to_string()))
                .transpose()?,
            scroll: self.scroll,
            ui: self.ui,
            size: self.size,
            position: self.position,
            maximized: self.maximized,
        })
    }
}
impl Session {
    pub fn read(path: PathBuf, expected: &std::path::Path) -> Result<Self, String> {
        let session: Self = serde_json::from_reader(BufReader::new(
            std::fs::File::open(path).map_err(|e| e.to_string())?,
        ))
        .map_err(|e| e.to_string())?;
        if session.version != 1
            || session.path != expected
            || session.active_index > session.buffers.len()
            || session
                .size
                .iter()
                .any(|v| !v.is_finite() || *v <= 0. || *v > 32768.)
        {
            return Err("invalid or unsupported reload session".into());
        }
        if session.is_empty()
            && (session.id != 0
                || session.file.is_some()
                || !session.buffers.is_empty()
                || session.active_index != 0)
        {
            return Err("invalid empty reload session".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        if !session.is_empty() {
            ids.insert(session.id.max(1));
        }
        if session.buffers.len() >= 256
            || session.id == u64::MAX
            || session
                .buffers
                .iter()
                .any(|b| b.id == u64::MAX || (b.id != 0 && !ids.insert(b.id)))
            || (session.next_id != 0
                && (session.next_id == u64::MAX
                    || session.next_id <= ids.last().copied().unwrap_or(0)))
        {
            return Err("invalid reload document identities".into());
        }
        Ok(session)
    }
    pub fn extra_editors(&self) -> Result<Vec<RestoredBuffer>, String> {
        self.buffers
            .iter()
            .map(|b| {
                Ok(RestoredBuffer {
                    id: b.id,
                    file: b.file.clone(),
                    editor: b.editor()?,
                    path: b.path.clone(),
                    search_open: b.search_open,
                    search_query: b.search_query.clone(),
                })
            })
            .collect()
    }
    pub fn is_empty(&self) -> bool {
        self.buffer.is_none()
    }
    pub fn editor(&self) -> Result<Editor, String> {
        crate::undo::restore_editor(
            self.buffer
                .as_ref()
                .ok_or("workspace has no documents")?
                .as_bytes(),
            self.scroll,
        )
    }
}
/// Runs only on the blocking pool. Keep the old app alive until the replacement
/// has restored the session and created its window. Failed startup kills/reaps
/// the child, removes private handoff files, and leaves the old document intact.
#[tracing::instrument(level = "debug", skip_all)]
pub fn launch(pending: Pending) -> Result<Child, String> {
    launch_with(
        pending,
        std::env::current_exe().map_err(|e| e.to_string())?,
        Duration::from_secs(20),
    )
}
fn launch_with(pending: Pending, executable: PathBuf, timeout: Duration) -> Result<Child, String> {
    let session = pending.encode()?;
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let snapshot = directory.path().join("session.json");
    let ready = directory.path().join("ready");
    let mut writer = BufWriter::new(std::fs::File::create(&snapshot).map_err(|e| e.to_string())?);
    serde_json::to_writer(&mut writer, &session).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    writer.get_ref().sync_all().map_err(|e| e.to_string())?;
    let mut child = Command::new(executable)
        .arg(&session.path)
        .env(
            "VELK_TRACE",
            if velk_diagnostics::recording() {
                "1"
            } else {
                "0"
            },
        )
        .env(SNAPSHOT_ENV, snapshot)
        .env(READY_ENV, &ready)
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!(
                    "replacement exited before becoming ready: {status}"
                ));
            }
            if ready.is_file() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("replacement window did not become ready in time".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    if let Err(error) = result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}
pub fn acknowledge() -> io::Result<()> {
    if let Some(path) = std::env::var_os(READY_ENV) {
        std::fs::write(path, b"ready")?;
    }
    Ok(())
}
// Paths in private sessions preserve native encoding, including non-UTF-8 Unix
// filenames. No lossy display conversion is used for paths that will be saved.
pub mod path {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::path::{Path, PathBuf};
    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            path.as_os_str().as_bytes().serialize(serializer)
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            path.as_os_str()
                .encode_wide()
                .collect::<Vec<_>>()
                .serialize(serializer)
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Vec::<u8>::deserialize(deserializer)
                .map(|bytes| std::ffi::OsString::from_vec(bytes).into())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            Vec::<u16>::deserialize(deserializer)
                .map(|units| std::ffi::OsString::from_wide(&units).into())
        }
    }
}

/// Native-encoded optional paths used by persistent workspace state.
pub mod optional_path {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::path::PathBuf;
    pub fn serialize<S: Serializer>(
        value: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .map(|p| velk_panels::NativePath::new(p))
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Option::<velk_panels::NativePath>::deserialize(deserializer)?
            .map(|p| p.to_path().map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(path: PathBuf) -> Pending {
        let (file, mut editor) = FileState::open(&path).unwrap();
        editor.insert("unsaved 🌍");
        Pending {
            id: 1,
            next_id: 3,
            path,
            file: Some(file),
            buffer: Some(editor.buffer().snapshot()),
            scroll: [20., 400.],
            ui: UiState::default(),
            size: [900., 600.],
            position: None,
            maximized: false,
            client: None,
            buffers: Vec::new(),
            active_index: 0,
        }
    }
    #[test]
    fn empty_reload_session_has_no_document_snapshot_or_file() {
        let directory = tempfile::tempdir().unwrap();
        let session = Pending {
            id: 0,
            next_id: 42,
            path: PathBuf::new(),
            file: None,
            buffer: None,
            scroll: [0., 0.],
            ui: Default::default(),
            size: [900., 600.],
            position: None,
            maximized: false,
            client: None,
            buffers: vec![],
            active_index: 0,
        }
        .encode()
        .unwrap();
        let snapshot = directory.path().join("session.json");
        std::fs::write(&snapshot, serde_json::to_vec(&session).unwrap()).unwrap();
        let restored = Session::read(snapshot, std::path::Path::new("")).unwrap();
        assert!(restored.is_empty());
        assert_eq!(restored.next_id, 42);
        assert!(restored.extra_editors().unwrap().is_empty());
        assert!(restored.editor().is_err());
    }
    #[test]
    fn session_preserves_unsaved_text_and_original_disk_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "original\r\n").unwrap();
        let session = pending(path.clone()).encode().unwrap();
        let snapshot = dir.path().join("session.json");
        std::fs::write(&snapshot, serde_json::to_vec(&session).unwrap()).unwrap();
        let restored = Session::read(snapshot.clone(), &path).unwrap();
        let editor = restored.editor().unwrap();
        assert_eq!(editor.buffer().text().to_string(), "unsaved 🌍original\n");
        assert!(editor.buffer().is_dirty());
        assert!(editor.buffer().can_undo());
        assert_eq!(editor.scroll_offset(), [20., 400.]);
        assert!(Session::read(snapshot, &dir.path().join("other")).is_err());
        std::fs::write(&path, "external").unwrap();
        assert!(
            restored
                .file
                .unwrap()
                .save(editor.buffer().text())
                .err()
                .unwrap()
                .to_string()
                .contains("changed on disk")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external");
    }
    #[test]
    fn multiple_buffers_roundtrip_in_order_with_dirty_history_scroll_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "first").unwrap();
        std::fs::write(&b, "second").unwrap();
        let mut pending = pending(b.clone());
        let (file, mut editor) = FileState::open(&a).unwrap();
        editor.select_range(1..3);
        editor.insert("edited");
        pending.buffers.push(PendingBuffer {
            id: 2,
            path: a.clone(),
            file: Some(file),
            buffer: editor.buffer().snapshot(),
            scroll: [10., 80.],
            search_open: true,
            search_query: velk_search::Query {
                text: "edited".into(),
                ..Default::default()
            },
        });
        pending.active_index = 1;
        let session = pending.encode().unwrap();
        let snapshot = dir.path().join("session.json");
        std::fs::write(&snapshot, serde_json::to_vec(&session).unwrap()).unwrap();
        let restored = Session::read(snapshot, &b).unwrap();
        assert_eq!(restored.active_index, 1);
        let mut runtime = rxui::Runtime::new();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| restored.editor().unwrap());
            let d = cx.new(|cx| {
                crate::document::Document::new(editor, restored.file.clone(), b.clone(), cx)
            });
            d.update(cx, |d, cx| {
                d.restore_buffers(restored.extra_editors().unwrap(), restored.active_index, cx)
            });
            d
        });
        let _ = document;
        let extras = restored.extra_editors().unwrap();
        assert_eq!(extras[0].path, a);
        assert_eq!(extras[0].editor.scroll_offset(), [10., 80.]);
        assert!(extras[0].editor.buffer().is_dirty());
        assert!(extras[0].editor.buffer().can_undo());
        assert_eq!(extras[0].editor.buffer().text().to_string(), "feditedst");
        assert!(extras[0].search_open);
        assert_eq!(extras[0].search_query.text, "edited");
        assert!(restored.editor().unwrap().buffer().is_dirty());
        assert_eq!(std::fs::read_to_string(a).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "second");
    }
    #[cfg(unix)]
    #[test]
    fn handoff_requires_readiness_and_reaps_failed_or_timed_out_children() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        assert!(
            launch_with(
                pending(path.clone()),
                "/usr/bin/false".into(),
                Duration::from_secs(1)
            )
            .is_err()
        );
        let script = dir.path().join("child");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 2\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            launch_with(
                pending(path.clone()),
                script.clone(),
                Duration::from_millis(40)
            )
            .unwrap_err()
            .contains("ready")
        );
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf ready > \"$VELK_RELOAD_READY\"\nexec sleep 2\n",
        )
        .unwrap();
        let mut child = launch_with(pending(path), script, Duration::from_secs(1)).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
