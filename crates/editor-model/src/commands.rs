use std::ops::Range;
use velk_core::Buffer;

/// Editing intent, independent of key bindings, clipboard, and rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditCommand<'a> {
    Insert(&'a str),
    Paste(&'a str),
    ReplaceAll(&'a str),
    Backspace,
    Delete,
    Undo,
    Redo,
    SelectAll,
    SelectRange(Range<usize>),
    SetCaret { position: usize, extend: bool },
    Horizontal { right: bool, extend: bool },
    Vertical { down: bool, extend: bool },
    LineEdge { end: bool, extend: bool },
    DocumentEdge { end: bool, extend: bool },
}

impl EditCommand<'_> {
    /// Returns whether the command was accepted, including valid no-ops at a
    /// document boundary. An invalid selection range leaves the buffer unchanged.
    pub fn apply(self, buffer: &mut Buffer) -> bool {
        match self {
            Self::Insert(text) | Self::Paste(text) => buffer.insert(text),
            Self::ReplaceAll(text) => {
                buffer.select_all();
                buffer.insert(text);
            }
            Self::Backspace => buffer.backspace(),
            Self::Delete => buffer.delete(),
            Self::Undo => buffer.undo(),
            Self::Redo => buffer.redo(),
            Self::SelectAll => buffer.select_all(),
            Self::SelectRange(range) => {
                if range.start > range.end || range.end > buffer.text().len_chars() {
                    return false;
                }
                buffer.set_caret(range.start, false);
                buffer.set_caret(range.end, true);
                if !range.is_empty() && buffer.selection().head < range.end {
                    buffer.horizontal(true, true);
                }
            }
            Self::SetCaret { position, extend } => buffer.set_caret(position, extend),
            Self::Horizontal { right, extend } => buffer.horizontal(right, extend),
            Self::Vertical { down, extend } => buffer.vertical(down, extend),
            Self::LineEdge { end, extend } => buffer.line_edge(end, extend),
            Self::DocumentEdge { end, extend } => buffer.document_edge(end, extend),
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Home,
    End,
    Left,
    Right,
    Up,
    Down,
    Backspace,
    Delete,
    Enter,
    Tab,
    Other,
}

/// Default navigation/edit bindings. Text commits and clipboard/menu shortcuts
/// are supplied separately by the frontend. Shift+Tab remains focus traversal.
pub fn key_command(
    key: Key,
    modifiers: Modifiers,
    platform: Platform,
) -> Option<EditCommand<'static>> {
    let primary = match platform {
        Platform::Mac => modifiers.meta,
        Platform::Other => modifiers.control,
    };
    if modifiers.alt || modifiers.control && !primary {
        return None;
    }
    let extend = modifiers.shift;
    Some(match key {
        Key::Home | Key::End => {
            let end = key == Key::End;
            if primary {
                EditCommand::DocumentEdge { end, extend }
            } else {
                EditCommand::LineEdge { end, extend }
            }
        }
        Key::Left | Key::Right if platform == Platform::Mac && primary => EditCommand::LineEdge {
            end: key == Key::Right,
            extend,
        },
        Key::Up | Key::Down if platform == Platform::Mac && primary => EditCommand::DocumentEdge {
            end: key == Key::Down,
            extend,
        },
        _ if primary => return None,
        Key::Left | Key::Right => EditCommand::Horizontal {
            right: key == Key::Right,
            extend,
        },
        Key::Up | Key::Down => EditCommand::Vertical {
            down: key == Key::Down,
            extend,
        },
        Key::Backspace => EditCommand::Backspace,
        Key::Delete => EditCommand::Delete,
        Key::Enter => EditCommand::Insert("\n"),
        Key::Tab if !extend => EditCommand::Insert("    "),
        _ => return None,
    })
}
