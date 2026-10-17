//! Exact history archives. Encoding, validation, and disk I/O belong on a worker.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct BufferSnapshot {
    current: Snapshot,
    history: history::RamHistory,
    revision: u64,
    next_state: u64,
    saved_state: u64,
    goal_column: Option<usize>,
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
    history: history::RamHistory,
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
            history: self.history.clone(),
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
fn apply_patch(text: &mut Rope, patch: &history::Patch, inverse: bool) -> Result<(), String> {
    if patch.change.replaced.start > patch.change.replaced.end
        || patch.change.replaced.len() != patch.removed.chars().count()
        || patch.change.inserted != patch.inserted.chars().count()
    {
        return Err("invalid history patch lengths".into());
    }
    let (removed, inserted) = if inverse {
        (&patch.inserted, &patch.removed)
    } else {
        (&patch.removed, &patch.inserted)
    };
    let start = patch.change.replaced.start;
    let end = start
        .checked_add(removed.chars().count())
        .ok_or("invalid history range")?;
    if end > text.len_chars()
        || text.slice(start..end) != removed.as_ref()
        || removed.contains('\r')
        || inserted.contains('\r')
    {
        return Err("history patch does not match its document".into());
    }
    let before = text.clone();
    text.remove(start..end);
    text.insert(start, inserted);
    let expected = Change::between(
        patch.change.revision,
        &before,
        text,
        start..end,
        inserted.chars().count(),
    );
    let actual = if inverse {
        patch.change.inverse(patch.change.revision)
    } else {
        patch.change.clone()
    };
    if expected != actual {
        return Err("invalid history coordinates".into());
    }
    Ok(())
}
fn validate_history(state: &State, text: &Rope) -> Result<(), String> {
    let history = &state.history;
    if history.entries.len() > history.limits.entries
        || history.bytes() > history.limits.bytes
        || history.root >= state.next_state
        || history.entries.iter().any(|(&id, entry)| {
            id >= state.next_state
                || !history.preferred.contains_key(&entry.parent)
                || id <= entry.parent
                || entry.parent < history.root
                || entry.patches.is_empty()
        })
        || history.preferred.iter().any(|(&parent, child)| {
            history
                .entries
                .get(child)
                .is_none_or(|entry| entry.parent != parent)
        })
    {
        return Err("invalid history tree".into());
    }
    let changes = history
        .entries
        .values()
        .try_fold(0u64, |sum, entry| {
            sum.checked_add(entry.patches.len() as u64)
        })
        .ok_or("history revision overflow")?;
    state
        .revision
        .checked_add(changes)
        .ok_or("history revision overflow")?;
    let mut children = std::collections::BTreeMap::<u64, Vec<_>>::new();
    for (&id, entry) in &history.entries {
        children.entry(entry.parent).or_default().push((id, entry));
    }
    let mut root = text.clone();
    let mut cursor = state.state;
    while cursor != history.root {
        let entry = history
            .entries
            .get(&cursor)
            .ok_or("history current state is disconnected")?;
        for patch in entry.patches.iter().rev() {
            apply_patch(&mut root, patch, true)?;
        }
        cursor = entry.parent;
    }
    let mut queue = vec![(history.root, root)];
    let mut visited = 0;
    while let Some((parent, text)) = queue.pop() {
        for &(id, entry) in children.get(&parent).map_or(&[][..], Vec::as_slice) {
            valid_selection(&text, entry.before)?;
            let mut next = text.clone();
            for patch in &entry.patches {
                apply_patch(&mut next, patch, false)?;
            }
            valid_selection(&next, entry.after)?;
            queue.push((id, next));
            visited += 1;
        }
    }
    if visited != history.entries.len() {
        return Err("disconnected history branch".into());
    }
    Ok(())
}
impl DocumentState {
    pub fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            current: self.current.clone(),
            history: self.history.clone(),
            revision: self.revision,
            next_state: self.next_state,
            saved_state: self.saved.state,
            goal_column: self.goal_column,
        }
    }
    pub fn restore(bytes: &[u8]) -> Result<Self, String> {
        let mut state: State = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if state.version != 1
            || state.text.contains('\r')
            || state.state >= state.next_state
            || state.saved_state >= state.next_state
            || state.revision > u64::MAX - 1024
            || state.next_state > u64::MAX - 1024
        {
            return Err("invalid or unsupported buffer session".into());
        }
        state.history.recount();
        let text = Rope::from_str(&state.text);
        valid_selection(&text, state.selection)?;
        validate_history(&state, &text)?;
        let mut buffer = Self::from_rope(text);
        buffer.current.selection = state.selection;
        buffer.current.state = state.state;
        buffer.history = state.history;
        buffer.revision = state.revision;
        buffer.next_state = state.next_state;
        buffer.saved.state = state.saved_state;
        buffer.goal_column = state.goal_column;
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reload_preserves_undo_redo_selection_and_saved_identity_with_small_deltas() {
        let mut buffer = Buffer::new(&"original 🌍\n".repeat(1000));
        buffer.insert("one");
        let checkpoint = buffer.checkpoint();
        buffer.mark_saved(checkpoint);
        buffer.insert("two");
        buffer.insert("three");
        buffer.undo();
        let bytes = buffer.snapshot().encode().unwrap();
        assert!(bytes.len() < buffer.text().len_bytes() + 4096);
        let mut restored = Buffer::restore(&bytes).unwrap();
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
        assert!(Buffer::restore(b"{}").is_err());
    }
}
