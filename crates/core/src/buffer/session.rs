//! Exact history archives. Encoding, validation, and disk I/O belong on a worker.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct BufferSnapshot {
    current: Snapshot,
    history: Option<CapturedHistory>,
    revision: u64,
    next_state: u64,
    saved_state: u64,
    goal_column: Option<usize>,
}
#[derive(Clone)]
struct CapturedHistory {
    provider: String,
    snapshot: Result<Arc<dyn crate::history::Snapshot>, String>,
}
#[derive(Serialize, Deserialize)]
struct State {
    version: u32,
    text: String,
    selection: Selection,
    state: u64,
    revision: u64,
    next_state: u64,
    saved_state: u64,
    goal_column: Option<usize>,
    history: Option<EncodedHistory>,
}
#[derive(Serialize, Deserialize)]
struct EncodedHistory {
    provider: String,
    data: serde_json::Value,
}
impl BufferSnapshot {
    pub fn is_saved(&self) -> bool {
        self.current.state == self.saved_state
    }
    /// An archive captured for a successful save must use that snapshot's state.
    pub fn mark_saved(&mut self) {
        self.saved_state = self.current.state;
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&State {
            version: 1,
            text: self.current.text.to_string(),
            selection: self.current.selection,
            state: self.current.state,
            revision: self.revision,
            next_state: self.next_state,
            saved_state: self.saved_state,
            goal_column: self.goal_column,
            history: self
                .history
                .as_ref()
                .map(|history| {
                    Ok::<_, String>(EncodedHistory {
                        provider: history.provider.clone(),
                        data: history.snapshot.as_ref().map_err(Clone::clone)?.encode()?,
                    })
                })
                .transpose()?,
        })
        .map_err(|e| e.to_string())
    }

    /// Atomically replace a disk archive. The caller chooses its retention and
    /// access policy and runs this blocking operation outside the editing thread.
    pub fn write_archive(&self, path: &std::path::Path) -> Result<(), String> {
        use std::io::Write;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        temporary
            .write_all(&self.encode()?)
            .map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        temporary.persist(path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
fn valid_selection(text: &Rope, selected: Selection) -> Result<(), String> {
    if [selected.anchor, selected.head]
        .iter()
        .any(|&p| p > text.len_chars() || !graphemes::is_boundary(text, p))
    {
        return Err("invalid history selection".into());
    }
    Ok(())
}
impl DocumentState {
    pub fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            current: self.current.clone(),
            history: self.history.as_ref().map(|history| CapturedHistory {
                provider: history.provider.id().to_owned(),
                snapshot: history.session.snapshot(),
            }),
            revision: self.revision,
            next_state: self.next_state,
            saved_state: self.saved.state,
            goal_column: self.goal_column,
        }
    }
    pub fn restore(
        bytes: &[u8],
        provider: Option<Arc<dyn crate::history::Provider>>,
    ) -> Result<Self, String> {
        let state: State = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if state.version != 1
            || state.text.contains('\r')
            || state.state >= state.next_state
            || state.saved_state >= state.next_state
            || state.revision > u64::MAX - 1024
            || state.next_state > u64::MAX - 1024
        {
            return Err("invalid or unsupported buffer session".into());
        }
        let text = Rope::from_str(&state.text);
        valid_selection(&text, state.selection)?;
        let mut buffer = Self::from_rope(text);
        buffer.current.selection = state.selection;
        buffer.current.state = state.state;
        buffer.history = if let Some(history) = state.history {
            let provider = provider.ok_or("undo archive requires its provider")?;
            if provider.id() != history.provider {
                return Err("wrong undo archive provider".into());
            }
            let session = provider.restore(crate::history::Restore {
                text: state.text,
                state: state.state,
                revision: state.revision,
                next_state: state.next_state,
                data: history.data,
            })?;
            Some(history::Owned { provider, session })
        } else {
            provider.map(|provider| history::Owned {
                session: provider.open(state.state),
                provider,
            })
        };
        buffer.revision = state.revision;
        buffer.next_state = state.next_state;
        buffer.saved.state = state.saved_state;
        buffer.goal_column = state.goal_column;
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reload_preserves_undo_redo_selection_and_saved_identity_with_small_deltas() {
        let mut buffer = crate::Buffer::with_history(
            &"original 🌍\n".repeat(1000),
            std::sync::Arc::new(velk_undo_tree::Tree),
        );
        buffer.insert("one");
        let checkpoint = buffer.checkpoint();
        buffer.mark_saved(checkpoint);
        buffer.insert("two");
        buffer.insert("three");
        buffer.undo();
        let bytes = buffer.snapshot().encode().unwrap();
        assert!(bytes.len() < buffer.text().len_bytes() + 4096);
        let mut restored =
            crate::Buffer::restore_with_history(&bytes, std::sync::Arc::new(velk_undo_tree::Tree))
                .unwrap();
        assert_eq!(restored.text(), buffer.text());
        assert_eq!(restored.selection(), buffer.selection());
        assert!(restored.is_dirty());
        restored.undo();
        assert!(!restored.is_dirty());
        restored.redo();
        restored.redo();
        buffer.redo();
        assert_eq!(restored.text(), buffer.text());
        assert!(restored.is_dirty());
        restored.insert("new");
        assert!(!restored.can_redo());
        assert!(
            crate::Buffer::restore_with_history(b"{}", std::sync::Arc::new(velk_undo_tree::Tree))
                .is_err()
        );
    }
}
