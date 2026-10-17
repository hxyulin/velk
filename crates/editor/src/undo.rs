//! Optional persistent undo archives. Called only from file I/O workers.
use std::path::{Path, PathBuf};
use velk_core::{Buffer, BufferSnapshot};

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
        let mut buffer = Buffer::restore(&bytes)?;
        if buffer.text() != *text {
            return Ok(None);
        }
        // The file just read is the saved baseline, regardless of archive metadata.
        buffer.mark_saved(buffer.checkpoint());
        Ok(Some(buffer))
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
    fn persistent_undo_attaches_only_to_matching_text_and_keeps_branches() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store {
            directory: directory.path().join("undo"),
        };
        let path = directory.path().join("document.txt");
        let mut buffer = Buffer::new("base");
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
