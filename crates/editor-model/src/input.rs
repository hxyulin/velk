use crate::selections::{MAX_SELECTIONS, SelectionRegion, SelectionSet};
use velk_core::{Buffer, Selection, TextEdit, Transaction};
use velk_editing::input::{Action, Motion, SelectionKind};

#[derive(Clone)]
pub(super) struct VisualSelection {
    anchor: usize,
    pub head: usize,
    kind: SelectionKind,
    revision: u64,
    projected: Selection,
    pub block: Option<SelectionSet>,
    columns: Option<(usize, usize)>,
}
impl VisualSelection {
    pub fn synchronize(&mut self, buffer: &Buffer) -> bool {
        if self.revision == buffer.revision() {
            return self.valid(buffer);
        }
        let Some(changes) = buffer.edits_since(self.revision) else {
            return false;
        };
        let selected_text = !self.projected.is_empty();
        for change in &changes {
            self.anchor = change.map_position(self.anchor, velk_core::Affinity::After);
            self.head = change.map_position(self.head, velk_core::Affinity::After);
            self.projected = change.map_selection(self.projected, velk_core::Affinity::After);
        }
        if selected_text && self.projected.is_empty() {
            return false;
        }
        let snap = |position: usize| {
            if buffer.is_grapheme_boundary(position) {
                position
            } else {
                buffer.next_grapheme_boundary(position.min(buffer.text().len_chars()))
            }
        };
        self.anchor = snap(self.anchor);
        self.head = snap(self.head);
        self.projected.anchor = snap(self.projected.anchor);
        self.projected.head = snap(self.projected.head);
        if let Some(block) = &mut self.block {
            if !block.synchronize(buffer) {
                return false;
            }
            self.columns = Some((column(buffer, self.anchor), column(buffer, self.head)));
        }
        self.revision = buffer.revision();
        self.valid(buffer)
    }
    pub fn valid(&self, buffer: &Buffer) -> bool {
        buffer.revision() == self.revision && buffer.selection() == self.projected
    }
    fn project(&mut self, buffer: &mut Buffer) -> Result<(), String> {
        if self.kind == SelectionKind::Block {
            let text = buffer.text();
            let first = text.char_to_line(self.anchor.min(self.head));
            let last = text.char_to_line(self.anchor.max(self.head));
            if last - first >= MAX_SELECTIONS {
                return Err("block exceeds 4096 rows".into());
            }
            let (anchor_column, head_column) = *self
                .columns
                .get_or_insert_with(|| (column(buffer, self.anchor), column(buffer, self.head)));
            let left = anchor_column.min(head_column);
            let right = anchor_column.max(head_column).saturating_add(1);
            let regions = (first..=last)
                .map(|line| {
                    let start = at_column(buffer, line, left);
                    let end = at_column(buffer, line, right);
                    let caret = at_column(buffer, line, head_column);
                    SelectionRegion {
                        selection: if head_column >= anchor_column {
                            Selection {
                                anchor: start,
                                head: end,
                            }
                        } else {
                            Selection {
                                anchor: end,
                                head: start,
                            }
                        },
                        caret,
                    }
                })
                .collect();
            let primary = text.char_to_line(self.head) - first;
            let set = SelectionSet::new(buffer, regions, primary)?;
            self.projected = set.regions[primary].selection;
            self.revision = buffer.revision();
            set.project(buffer);
            self.block = Some(set);
            return Ok(());
        }
        self.block = None;
        self.columns = None;
        let forward = self.anchor <= self.head;
        let (start, end) = match self.kind {
            SelectionKind::Block => unreachable!(),
            SelectionKind::Character => (
                self.anchor.min(self.head),
                buffer.next_grapheme_boundary(self.anchor.max(self.head)),
            ),
            SelectionKind::Line => {
                let text = buffer.text();
                let first = text.char_to_line(self.anchor.min(self.head));
                let last = text.char_to_line(self.anchor.max(self.head));
                (
                    text.line_to_char(first),
                    if last + 1 < text.len_lines() {
                        text.line_to_char(last + 1)
                    } else {
                        text.len_chars()
                    },
                )
            }
        };
        self.projected = if forward {
            Selection {
                anchor: start,
                head: end,
            }
        } else {
            Selection {
                anchor: end,
                head: start,
            }
        };
        self.revision = buffer.revision();
        buffer.set_selection_preserving_column(self.projected);
        Ok(())
    }
}
fn column(buffer: &Buffer, position: usize) -> usize {
    let mut cursor = buffer
        .text()
        .line_to_char(buffer.text().char_to_line(position));
    let mut column = 0;
    while cursor < position {
        cursor = buffer.next_grapheme_boundary(cursor);
        column += 1;
    }
    column
}
fn at_column(buffer: &Buffer, line: usize, column: usize) -> usize {
    let start = buffer.text().line_to_char(line);
    let text = buffer.text();
    let slice = text.line(line);
    let end = start + slice.len_chars()
        - usize::from(slice.get_char(slice.len_chars().saturating_sub(1)) == Some('\n'));
    let mut cursor = start;
    for _ in 0..column {
        if cursor >= end {
            break;
        }
        cursor = buffer.next_grapheme_boundary(cursor);
    }
    cursor.min(end)
}

pub(super) fn apply_visual(
    buffer: &mut Buffer,
    action: Action,
    visual: &mut Option<VisualSelection>,
    multiple: &mut Option<SelectionSet>,
) -> Result<(), String> {
    let before = (visual.clone(), multiple.clone(), buffer.selection());
    let result = apply_visual_inner(buffer, action, visual, multiple);
    if result.is_err() {
        *visual = before.0;
        *multiple = before.1;
        buffer.set_selection_preserving_column(before.2);
    }
    result
}
fn apply_visual_inner(
    buffer: &mut Buffer,
    action: Action,
    visual: &mut Option<VisualSelection>,
    multiple: &mut Option<SelectionSet>,
) -> Result<(), String> {
    match action {
        Action::Select { mode: Some(kind) } => {
            if let Some(selection) = visual {
                selection.kind = kind;
                selection.project(buffer)?;
            } else {
                let head = buffer.selection().head;
                let mut selection = VisualSelection {
                    anchor: head,
                    head,
                    kind,
                    revision: buffer.revision(),
                    projected: buffer.selection(),
                    block: None,
                    columns: None,
                };
                selection.project(buffer)?;
                *visual = Some(selection);
            }
        }
        Action::Select { mode: None } => {
            if let Some(selection) = visual.take() {
                buffer.set_caret(selection.head, false);
            }
            if let Some(set) = multiple.take() {
                buffer.set_caret(set.regions[set.primary].caret, false);
            }
        }
        Action::SwapSelectionEnds => {
            if let Some(selection) = visual {
                std::mem::swap(&mut selection.anchor, &mut selection.head);
                if let Some((anchor, head)) = &mut selection.columns {
                    std::mem::swap(anchor, head);
                }
                buffer.set_caret(selection.head, false);
                selection.project(buffer)?;
            }
        }
        Action::Move { motion, count } if visual.is_some() => {
            let selection = visual.as_mut().unwrap();
            buffer.set_selection_preserving_column(Selection {
                anchor: selection.head,
                head: selection.head,
            });
            move_caret(buffer, motion, count);
            let vertical = matches!(motion, Motion::Up | Motion::Down);
            if selection.kind != SelectionKind::Block || !vertical {
                buffer.clamp_caret_to_line_character();
            }
            selection.head = buffer.selection().head;
            if !vertical && let Some((_, head)) = &mut selection.columns {
                *head = column(buffer, selection.head);
            }
            selection.project(buffer)?;
        }
        Action::InsertAtSelections { after } => {
            if let Some(selection) = visual.take() {
                if let Some(mut set) = selection.block {
                    set.primary = 0;
                    set.collapse(buffer, after);
                    *multiple = Some(set);
                } else {
                    buffer.set_caret(selection.head, false);
                }
            }
        }
        Action::DeleteSelection | Action::ChangeSelection => {
            if let Some(selection) = visual.take() {
                if let Some(mut set) = selection.block {
                    set.primary = 0;
                    set.replace(buffer, "")?;
                    if matches!(action, Action::ChangeSelection) {
                        *multiple = Some(set);
                    }
                    return Ok(());
                }
                let mut range = buffer.selection().range();
                let linewise = selection.kind == SelectionKind::Line;
                let change = matches!(action, Action::ChangeSelection);
                let replacement = if change
                    && linewise
                    && range.end > range.start
                    && buffer.text().char(range.end - 1) == '\n'
                    && selection.anchor.max(selection.head) < buffer.text().len_chars()
                {
                    "\n"
                } else {
                    ""
                };
                if linewise
                    && !change
                    && range.end == buffer.text().len_chars()
                    && range.start > 0
                    && (range.end == range.start
                        || buffer.text().char(range.end - 1) != '\n'
                        || selection.anchor.max(selection.head) == buffer.text().len_chars())
                {
                    range.start -= 1;
                }
                let caret = range.start;
                buffer.set_caret(caret, false);
                replace(buffer, range, replacement, caret)?;
                if linewise && !change {
                    move_caret(buffer, Motion::FirstNonBlank, 1);
                }
            }
        }
        action => {
            if let Some(selection) = visual.take() {
                buffer.set_caret(selection.head, false);
            }
            apply(buffer, action)?;
        }
    }
    Ok(())
}

fn line_range(buffer: &Buffer) -> std::ops::Range<usize> {
    let text = buffer.text();
    let line = text.char_to_line(buffer.selection().head);
    let start = text.line_to_char(line);
    let mut end = start + text.line(line).len_chars();
    if end > start && text.char(end - 1) == '\n' {
        end -= 1;
    }
    start..end
}
fn class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if unicode_ident::is_xid_continue(ch) {
        1
    } else {
        2
    }
}
fn word_position(buffer: &Buffer, motion: Motion, mut position: usize) -> usize {
    let text = buffer.text();
    let len = text.len_chars();
    match motion {
        Motion::WordForward => {
            if position < len {
                let kind = class(text.char(position));
                while position < len && class(text.char(position)) == kind {
                    position = buffer.next_grapheme_boundary(position);
                }
            }
            while position < len && class(text.char(position)) == 0 {
                position = buffer.next_grapheme_boundary(position);
            }
        }
        Motion::WordBackward => {
            position = buffer.previous_grapheme_boundary(position);
            while position > 0 && class(text.char(position)) == 0 {
                position = buffer.previous_grapheme_boundary(position);
            }
            if position < len {
                let kind = class(text.char(position));
                while position > 0 {
                    let previous = buffer.previous_grapheme_boundary(position);
                    if class(text.char(previous)) != kind {
                        break;
                    }
                    position = previous;
                }
            }
        }
        Motion::WordEnd => {
            position = buffer.next_grapheme_boundary(position);
            while position < len && class(text.char(position)) == 0 {
                position = buffer.next_grapheme_boundary(position);
            }
            if position < len {
                let kind = class(text.char(position));
                loop {
                    let next = buffer.next_grapheme_boundary(position);
                    if next >= len || class(text.char(next)) != kind {
                        break;
                    }
                    position = next;
                }
            }
        }
        _ => unreachable!(),
    }
    position
}
fn move_caret(buffer: &mut Buffer, motion: Motion, count: usize) {
    match motion {
        Motion::Up | Motion::Down => {
            for _ in 0..count {
                buffer.vertical(motion == Motion::Down, false);
            }
        }
        Motion::Left | Motion::Right => {
            let range = line_range(buffer);
            for _ in 0..count {
                let head = buffer.selection().head;
                if motion == Motion::Left && head > range.start {
                    buffer.horizontal(false, false);
                } else if motion == Motion::Right && head < range.end {
                    buffer.horizontal(true, false);
                } else {
                    break;
                }
            }
        }
        Motion::WordForward | Motion::WordBackward | Motion::WordEnd => {
            let mut position = buffer.selection().head;
            for _ in 0..count {
                position = word_position(buffer, motion, position);
            }
            buffer.set_caret(position, false);
        }
        Motion::LineStart => buffer.line_edge(false, false),
        Motion::LineEnd => {
            for _ in 1..count {
                buffer.vertical(true, false);
            }
            buffer.line_edge(true, false);
        }
        Motion::FirstNonBlank => {
            let range = line_range(buffer);
            let position = buffer
                .text()
                .slice(range.clone())
                .chars()
                .position(|ch| !matches!(ch, ' ' | '\t'))
                .map_or(range.start, |offset| range.start + offset);
            buffer.set_caret(position, false);
        }
        Motion::DocumentStart | Motion::DocumentEnd | Motion::Line => {
            let line = match motion {
                Motion::DocumentStart => 0,
                Motion::DocumentEnd => buffer.text().len_lines() - 1,
                _ => count.saturating_sub(1).min(buffer.text().len_lines() - 1),
            };
            buffer.set_caret(buffer.text().line_to_char(line), false);
            move_caret(buffer, Motion::FirstNonBlank, 1);
        }
    }
}
fn replace(
    buffer: &mut Buffer,
    range: std::ops::Range<usize>,
    text: &str,
    caret: usize,
) -> Result<(), String> {
    buffer
        .apply_transaction(Transaction {
            revision: buffer.revision(),
            edits: vec![TextEdit {
                range,
                text: text.into(),
            }],
            selection: Some(Selection {
                anchor: caret,
                head: caret,
            }),
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
pub(super) fn apply(buffer: &mut Buffer, action: Action) -> Result<(), String> {
    match action {
        Action::Select { .. }
        | Action::SwapSelectionEnds
        | Action::DeleteSelection
        | Action::ChangeSelection
        | Action::InsertAtSelections { .. } => {
            return Err("selection action requires a selection session".into());
        }
        Action::Move { motion, count } => move_caret(buffer, motion, count),
        Action::DeleteCharacters { count } => {
            let start = buffer.selection().head;
            move_caret(buffer, Motion::Right, count);
            let end = buffer.selection().head;
            buffer.set_caret(start, false);
            replace(buffer, start..end, "", start)?;
        }
        Action::DeleteLines { count } => {
            let text = buffer.text();
            let first = text.char_to_line(buffer.selection().head);
            let next = first.saturating_add(count);
            let mut start = text.line_to_char(first);
            let end = if next < text.len_lines() {
                text.line_to_char(next)
            } else {
                text.len_chars()
            };
            if next >= text.len_lines() && start > 0 {
                start -= 1;
            }
            replace(buffer, start..end, "", start)?;
            move_caret(buffer, Motion::FirstNonBlank, 1);
        }
        Action::InsertLine { below } => {
            let range = line_range(buffer);
            let position = if below { range.end } else { range.start };
            replace(
                buffer,
                position..position,
                "\n",
                position + usize::from(below),
            )?;
        }
        Action::Undo { count } => {
            for _ in 0..count {
                buffer.undo();
            }
        }
        Action::Redo { count } => {
            for _ in 0..count {
                buffer.redo();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visual_block_regions_follow_peer_edits_without_losing_the_mode() {
        let mut buffer =
            velk_core::Buffer::with_history("ab\ncd", std::sync::Arc::new(velk_undo_tree::Tree));
        let mut visual = None;
        let mut multiple = None;
        apply_visual(
            &mut buffer,
            Action::Select {
                mode: Some(SelectionKind::Block),
            },
            &mut visual,
            &mut multiple,
        )
        .unwrap();
        apply_visual(
            &mut buffer,
            Action::Move {
                motion: Motion::Down,
                count: 1,
            },
            &mut visual,
            &mut multiple,
        )
        .unwrap();
        let mut peer = buffer.new_view();
        peer.set_caret(0, false);
        peer.insert("x\n");
        let visual = visual.as_mut().unwrap();
        assert!(visual.synchronize(&buffer));
        assert_eq!(visual.kind, SelectionKind::Block);
        let regions = &visual.block.as_ref().unwrap().regions;
        assert_eq!(regions[0].caret, 2);
        assert_eq!(regions[1].caret, 5);
        assert_eq!(visual.head, 5);
    }
}
