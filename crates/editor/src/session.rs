//! Private process handoff, not a persistent workspace/distribution format.
use crate::{document::reload::UiState, file::FileState};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufReader, BufWriter, Write},
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};
use velk_editor_widget::{BufferSnapshot, Editor};
pub const SNAPSHOT_ENV: &str = "VELK_RELOAD_SESSION";
pub const READY_ENV: &str = "VELK_RELOAD_READY";

pub struct Pending {
    pub path: PathBuf,
    pub file: FileState,
    pub buffer: BufferSnapshot,
    pub scroll: [f32; 2],
    pub ui: UiState,
    pub size: [f64; 2],
    pub position: Option<[i32; 2]>,
    pub maximized: bool,
    pub client: Option<velk_plugin_host::Client>,
}
#[derive(Serialize, Deserialize)]
pub struct Session {
    version: u32,
    #[serde(with = "path")]
    pub path: PathBuf,
    pub file: FileState,
    buffer: String,
    pub scroll: [f32; 2],
    pub ui: UiState,
    pub size: [f64; 2],
    #[serde(default)]
    pub position: Option<[i32; 2]>,
    #[serde(default)]
    pub maximized: bool,
    pub plugin_states: Vec<(String, Vec<u8>)>,
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
            version: 1,
            plugin_states,
            path: self.path,
            file: self.file,
            buffer: String::from_utf8(self.buffer.encode()?).map_err(|e| e.to_string())?,
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
            || session
                .size
                .iter()
                .any(|v| !v.is_finite() || *v <= 0. || *v > 32768.)
        {
            return Err("invalid or unsupported reload session".into());
        }
        Ok(session)
    }
    pub fn editor(&self) -> Result<Editor, String> {
        Editor::from_session(self.buffer.as_bytes(), self.scroll)
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

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(path: PathBuf) -> Pending {
        let (file, mut editor) = FileState::open(&path).unwrap();
        editor.insert("unsaved 🌍");
        Pending {
            path,
            file,
            buffer: editor.buffer().snapshot(),
            scroll: [20., 400.],
            ui: UiState::default(),
            size: [900., 600.],
            position: None,
            maximized: false,
            client: None,
        }
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
                .save(editor.buffer().text().clone())
                .err()
                .unwrap()
                .to_string()
                .contains("changed on disk")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external");
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
