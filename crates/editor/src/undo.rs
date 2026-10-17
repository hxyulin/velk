//! Undo provider selection and optional persistent archives. Bootstrap/restore
//! may wait for the plugin worker; UI document creation uses cached factories.
use std::path::{Path, PathBuf};
use velk_core::{Buffer, BufferSnapshot};

type Provider = std::sync::Arc<dyn velk_core::history::Provider>;
#[derive(Default)]
struct Providers {
    client: Option<velk_plugin_host::Client>,
    bootstrap_complete: bool,
    selected: String,
    disabled: std::collections::BTreeSet<String>,
    factories: std::collections::BTreeMap<String, Provider>,
}
fn registry() -> &'static std::sync::RwLock<Providers> {
    static REGISTRY: std::sync::OnceLock<std::sync::RwLock<Providers>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::RwLock::new(Providers::default()))
}
pub(crate) fn finish_bootstrap() {
    registry().write().unwrap().bootstrap_complete = true;
}
pub(crate) fn set_client(client: Option<velk_plugin_host::Client>) {
    registry().write().unwrap().client = client;
}
pub(crate) fn configure(
    preferences: &crate::settings::Preferences,
    factories: Option<std::collections::BTreeMap<String, Provider>>,
) {
    let mut registry = registry().write().unwrap();
    registry.selected = preferences.undo_provider.clone();
    registry.disabled = preferences.disabled_plugins.iter().cloned().collect();
    if let Some(factories) = factories {
        registry.bootstrap_complete = true;
        registry.factories = factories;
    }
}
fn resolve(id: &str) -> Result<Provider, String> {
    {
        let registry = registry().read().unwrap();
        if registry.disabled.contains(id) {
            return if id == velk_core::history::DEFAULT_PROVIDER {
                Ok(std::sync::Arc::new(velk_undo_tree::Tree))
            } else {
                Err(format!("undo provider {id} is disabled"))
            };
        }
        if let Some(provider) = registry.factories.get(id) {
            return Ok(provider.clone());
        }
        if registry.bootstrap_complete {
            return if id == velk_core::history::DEFAULT_PROVIDER {
                Ok(std::sync::Arc::new(velk_undo_tree::Tree))
            } else {
                Err(format!("undo provider {id} is not loaded"))
            };
        }
    }
    let client = registry().read().unwrap().client.clone();
    let loaded = (|| {
        let client = client.ok_or("plugin runtime unavailable")?;
        let report = client
            .load_selected(vec![id.to_owned()])?
            .recv_blocking()
            .map_err(|e| e.to_string())??;
        let provider = report
            .undo
            .get(id)
            .cloned()
            .ok_or("undo service unavailable")?;
        registry().write().unwrap().factories.extend(report.undo);
        Ok::<_, String>(provider)
    })();
    let provider = match loaded {
        Ok(provider) => provider,
        Err(_) if id == velk_core::history::DEFAULT_PROVIDER => {
            std::sync::Arc::new(velk_undo_tree::Tree)
        }
        Err(error) => return Err(format!("undo provider {id} is unavailable: {error}")),
    };
    registry()
        .write()
        .unwrap()
        .factories
        .insert(id.into(), provider.clone());
    Ok(provider)
}
pub(crate) fn provider() -> Provider {
    let selected = registry().read().unwrap().selected.clone();
    let selected = if selected.is_empty() {
        velk_core::history::DEFAULT_PROVIDER
    } else {
        &selected
    };
    resolve(selected).unwrap_or_else(|error| {
        tracing::warn!(%error, "using bundled undo provider for new document");
        std::sync::Arc::new(velk_undo_tree::Tree)
    })
}
pub(crate) fn editor(text: &str) -> velk_editor_widget::Editor {
    velk_editor_widget::Editor::from_buffer(Buffer::with_history(text, provider()))
}
fn restore(bytes: &[u8]) -> Result<Buffer, String> {
    let provider = Buffer::archive_history_provider(bytes)?
        .map(|id| resolve(&id))
        .transpose()?;
    if let Some(provider) = provider {
        Buffer::restore_with_history(bytes, provider)
    } else {
        let mut buffer = Buffer::restore(bytes)?;
        buffer.attach_history(self::provider())?;
        Ok(buffer)
    }
}
pub(crate) fn restore_editor(
    bytes: &[u8],
    scroll: [f32; 2],
) -> Result<velk_editor_widget::Editor, String> {
    let mut editor = velk_editor_widget::Editor::from_buffer(restore(bytes)?);
    editor.restore_view(editor.buffer().selection(), scroll)?;
    Ok(editor)
}

pub(crate) struct Store {
    directory: PathBuf,
}
impl Store {
    pub fn from_env() -> Option<Self> {
        std::env::var_os("VELK_UNDO_DIR")
            .filter(|path| !path.is_empty())
            .map(|directory| Self {
                directory: directory.into(),
            })
    }
    fn path(&self, document: &Path) -> PathBuf {
        let key = blake3::hash(document.as_os_str().as_encoded_bytes());
        self.directory.join(format!("{key}.undo.json"))
    }
    pub fn load(&self, document: &Path, text: &ropey::Rope) -> Result<Option<Buffer>, String> {
        let bytes = match std::fs::read(self.path(document)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut buffer = restore(&bytes)?;
        if buffer.text() != *text {
            return Ok(None);
        }
        // The file just read is the saved baseline, regardless of archive metadata.
        buffer.mark_saved(buffer.checkpoint());
        Ok(Some(buffer))
    }
    pub fn relocate(&self, source: &Path, destination: &Path) -> Result<(), String> {
        match std::fs::rename(self.path(source), self.path(destination)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
    pub fn save(&self, document: &Path, mut snapshot: BufferSnapshot) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        snapshot.mark_saved();
        snapshot.write_archive(&self.path(document))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_archives_follow_renamed_files() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store {
            directory: directory.path().join("undo"),
        };
        let source = directory.path().join("before.txt");
        let destination = directory.path().join("after.txt");
        let mut buffer =
            velk_core::Buffer::with_history("base", std::sync::Arc::new(velk_undo_tree::Tree));
        buffer.insert("more");
        store.save(&source, buffer.snapshot()).unwrap();
        store.relocate(&source, &destination).unwrap();
        assert!(store.load(&source, &buffer.text()).unwrap().is_none());
        let mut restored = store.load(&destination, &buffer.text()).unwrap().unwrap();
        restored.undo();
        assert_eq!(restored.text().to_string(), "base");
        store.relocate(&source, &destination).unwrap();
    }
    #[test]
    fn persistent_undo_attaches_only_to_matching_text_and_keeps_branches() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store {
            directory: directory.path().join("undo"),
        };
        let path = directory.path().join("document.txt");
        let mut buffer =
            velk_core::Buffer::with_history("base", std::sync::Arc::new(velk_undo_tree::Tree));
        assert!(store.load(&path, &buffer.text()).unwrap().is_none());
        buffer.document_edge(true, false);
        buffer.insert("A");
        buffer.undo();
        buffer.insert("B");
        store.save(&path, buffer.snapshot()).unwrap();
        let mut reopened = store.load(&path, &buffer.text()).unwrap().unwrap();
        assert!(!reopened.is_dirty());
        reopened.undo();
        assert_eq!(reopened.text().to_string(), "base");
        assert_eq!(reopened.redo_branches().len(), 2);
        assert!(store.load(&path, &reopened.text()).unwrap().is_none());
        std::fs::write(store.path(&path), b"broken").unwrap();
        assert!(store.load(&path, &buffer.text()).is_err());
    }
}
