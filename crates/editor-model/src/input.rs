use velk_core::{Buffer, Selection, TextEdit, Transaction};
use velk_editing::input::{Action, Motion, SelectionKind};

pub(super) struct VisualSelection {
    anchor: usize,
    pub head: usize,
    kind: SelectionKind,
    revision: u64,
    projected: Selection,
}
impl VisualSelection {
    pub fn valid(&self, buffer: &Buffer) -> bool {
        buffer.revision() == self.revision && buffer.selection() == self.projected
    }
    fn project(&mut self, buffer: &mut Buffer) {
        let forward = self.anchor <= self.head;
        let (start, end) = match self.kind {
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
    }
}

pub(super) fn apply_visual(
    buffer: &mut Buffer,
    action: Action,
    visual: &mut Option<VisualSelection>,
) -> Result<(), String> {
    match action {
        Action::Select { mode: Some(kind) } => {
            if let Some(selection) = visual {
                selection.kind = kind;
                selection.project(buffer);
            } else {
                let head = buffer.selection().head;
                let mut selection = VisualSelection {
                    anchor: head,
                    head,
                    kind,
                    revision: buffer.revision(),
                    projected: buffer.selection(),
                };
                selection.project(buffer);
                *visual = Some(selection);
            }
        }
        Action::Select { mode: None } => {
            if let Some(selection) = visual.take() {
                buffer.set_caret(selection.head, false);
            }
        }
        Action::SwapSelectionEnds => {
            if let Some(selection) = visual {
                std::mem::swap(&mut selection.anchor, &mut selection.head);
                buffer.set_caret(selection.head, false);
                selection.project(buffer);
            }
        }
        Action::Move { motion, count } if visual.is_some() => {
            let selection = visual.as_mut().unwrap();
            buffer.set_selection_preserving_column(Selection {
                anchor: selection.head,
                head: selection.head,
            });
            move_caret(buffer, motion, count);
            buffer.clamp_caret_to_line_character();
            selection.head = buffer.selection().head;
            selection.project(buffer);
        }
        Action::DeleteSelection | Action::ChangeSelection => {
            if let Some(selection) = visual.take() {
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
        | Action::ChangeSelection => {
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
