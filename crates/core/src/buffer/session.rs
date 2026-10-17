//! Owned, versioned document state. Rope clones are cheap on the UI thread;
//! flattening/delta encoding and validation belong on a background/startup thread.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct BufferSnapshot {
    current: Snapshot,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    revision: u64,
    next_state: u64,
    saved_state: u64,
    goal_column: Option<usize>,
}
#[derive(Serialize, Deserialize)]
struct State {
    version: u32,
    text: String,
    selection: [usize; 2],
    state: u64,
    revision: u64,
    next_state: u64,
    saved_state: u64,
    goal_column: Option<usize>,
    undo: Vec<Step>,
    redo: Vec<Step>,
}
#[derive(Serialize, Deserialize)]
struct Step {
    start: usize,
    removed: usize,
    text: String,
    selection: [usize; 2],
    state: u64,
}
fn difference(from: &Rope, to: &Rope) -> (Range<usize>, String) {
    let prefix = from
        .chars()
        .zip(to.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = from
        .chars_at(from.len_chars())
        .reversed()
        .zip(to.chars_at(to.len_chars()).reversed())
        .take(from.len_chars().min(to.len_chars()) - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    (
        prefix..from.len_chars() - suffix,
        to.slice(prefix..to.len_chars() - suffix).to_string(),
    )
}
fn steps<'a>(mut from: &'a Rope, history: impl Iterator<Item = &'a Snapshot>) -> Vec<Step> {
    history
        .map(|entry| {
            let snapshot = entry;
            let (range, text) = difference(from, &snapshot.text);
            from = &snapshot.text;
            Step {
                start: range.start,
                removed: range.len(),
                text,
                selection: [snapshot.selection.anchor, snapshot.selection.head],
                state: snapshot.state,
            }
        })
        .collect()
}
impl BufferSnapshot {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&State {
            version: 1,
            text: self.current.text.to_string(),
            selection: [self.current.selection.anchor, self.current.selection.head],
            state: self.current.state,
            revision: self.revision,
            next_state: self.next_state,
            saved_state: self.saved_state,
            goal_column: self.goal_column,
            undo: steps(&self.current.text, self.undo.iter().rev()),
            redo: steps(&self.current.text, self.redo.iter().rev()),
        })
        .map_err(|e| e.to_string())
    }
}
fn selection(text: &Rope, [anchor, head]: [usize; 2]) -> Result<Selection, String> {
    if [anchor, head]
        .iter()
        .any(|&p| p > text.len_chars() || !graphemes::is_boundary(text, p))
    {
        return Err("invalid session selection".into());
    }
    Ok(Selection { anchor, head })
}
fn history(
    current: &Snapshot,
    steps: Vec<Step>,
    undo: bool,
    next_state: u64,
) -> Result<Vec<History>, String> {
    let mut from = current.text.clone();
    let mut result = Vec::new();
    for step in steps {
        let end = step
            .start
            .checked_add(step.removed)
            .ok_or("invalid session range")?;
        if end > from.len_chars() || step.state >= next_state || step.text.contains('\r') {
            return Err("invalid session history".into());
        }
        let mut text = from.clone();
        text.remove(step.start..end);
        text.insert(step.start, &step.text);
        let selected = selection(&text, step.selection)?;
        let edit = if undo {
            Change::between(
                0,
                &text,
                &from,
                step.start..step.start + step.text.chars().count(),
                step.removed,
            )
        } else {
            Change::between(0, &from, &text, step.start..end, step.text.chars().count())
        };
        result.push(History {
            snapshot: Snapshot {
                text: text.clone(),
                selection: selected,
                state: step.state,
            },
            edits: vec![edit],
        });
        from = text;
    }
    result.reverse();
    Ok(result)
}
impl DocumentState {
    pub fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            current: self.current.clone(),
            undo: self
                .undo
                .iter()
                .map(|entry| entry.snapshot.clone())
                .collect(),
            redo: self
                .redo
                .iter()
                .map(|entry| entry.snapshot.clone())
                .collect(),
            revision: self.revision,
            next_state: self.next_state,
            saved_state: self.saved.state,
            goal_column: self.goal_column,
        }
    }
    pub fn restore(bytes: &[u8]) -> Result<Self, String> {
        let state: State = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if state.version != 1
            || state.undo.len() + state.redo.len() > HISTORY_LIMIT
            || state.text.contains('\r')
            || state.state >= state.next_state
            || state.saved_state >= state.next_state
            || state.revision > u64::MAX - 1024
            || state.next_state > u64::MAX - 1024
        {
            return Err("invalid or unsupported buffer session".into());
        }
        let mut buffer = Self::from_rope(Rope::from_str(&state.text));
        buffer.current.selection = selection(&buffer.current.text, state.selection)?;
        buffer.current.state = state.state;
        buffer.undo = history(&buffer.current, state.undo, true, state.next_state)?.into();
        buffer.redo = history(&buffer.current, state.redo, false, state.next_state)?;
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
        assert!(bytes.len() < buffer.text().len_bytes() + 2048);
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
