//! Reusable rope-backed text editing state and an RXUI editor view.
mod buffer;
mod surface;

pub use buffer::{Buffer, Selection};
use rxui::{Bounds, prelude::*, standard_commands as commands};
use std::{cell::RefCell, rc::Rc};
use surface::{Composition, Geometry, Surface};

/// An editor view. Create a separate entity per window; document state can be
/// accessed through `buffer()` and programmatic inserts through `insert()`.
#[derive(Default)]
pub struct Editor {
    buffer: Buffer,
    composition: Option<Composition>,
    geometry: Rc<RefCell<Geometry>>,
    epoch: u64,
    reveal_caret: bool,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        Self {
            buffer: Buffer::new(text),
            ..Self::default()
        }
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn insert(&mut self, text: &str) {
        self.composition = None;
        self.buffer.insert(text);
        self.edited();
    }

    fn edited(&mut self) {
        self.epoch += 1;
        self.reveal_caret = true;
    }

    fn key(&mut self, event: &KeyInput, _: &mut Context<'_, Self>) {
        let key = &event.event;
        if !key.pressed || self.composition.is_some() {
            return;
        }
        let extend = key.modifiers.shift;
        // Leave unsupported modifier chords untouched. Word movement will be
        // added in a later editor increment.
        if key.modifiers.alt || key.modifiers.control && !key.modifiers.primary() {
            return;
        }
        let handled = match &key.key {
            KeyboardKey::Home => {
                if key.modifiers.primary() {
                    self.buffer.document_edge(false, extend);
                } else {
                    self.buffer.line_edge(false, extend);
                }
                true
            }
            KeyboardKey::End => {
                if key.modifiers.primary() {
                    self.buffer.document_edge(true, extend);
                } else {
                    self.buffer.line_edge(true, extend);
                }
                true
            }
            KeyboardKey::ArrowLeft if cfg!(target_os = "macos") && key.modifiers.primary() => {
                self.buffer.line_edge(false, extend);
                true
            }
            KeyboardKey::ArrowRight if cfg!(target_os = "macos") && key.modifiers.primary() => {
                self.buffer.line_edge(true, extend);
                true
            }
            KeyboardKey::ArrowUp if cfg!(target_os = "macos") && key.modifiers.primary() => {
                self.buffer.document_edge(false, extend);
                true
            }
            KeyboardKey::ArrowDown if cfg!(target_os = "macos") && key.modifiers.primary() => {
                self.buffer.document_edge(true, extend);
                true
            }
            _ if key.modifiers.primary() => false,
            KeyboardKey::ArrowLeft => {
                self.buffer.horizontal(false, extend);
                true
            }
            KeyboardKey::ArrowRight => {
                self.buffer.horizontal(true, extend);
                true
            }
            KeyboardKey::ArrowUp => {
                self.buffer.vertical(false, extend);
                true
            }
            KeyboardKey::ArrowDown => {
                self.buffer.vertical(true, extend);
                true
            }
            KeyboardKey::Backspace => {
                self.buffer.backspace();
                true
            }
            KeyboardKey::Delete => {
                self.buffer.delete();
                true
            }
            KeyboardKey::Enter => {
                self.buffer.insert("\n");
                true
            }
            KeyboardKey::Tab if !extend => {
                self.buffer.insert("    ");
                true
            }
            // Shift+Tab remains available for focus traversal out of this widget.
            _ => false,
        };
        if handled {
            event.prevent_default();
            self.edited();
        }
    }

    fn pointer(&mut self, event: &PointerInput, drag: bool) {
        if !drag && event.button != Some(PointerButton::Primary) {
            return;
        }
        if drag && !event.buttons.contains(PointerButton::Primary) {
            return;
        }
        let position = self.geometry.borrow().hit(event.local_position);
        if let Some(position) = position {
            self.buffer
                .set_caret(position, drag || event.modifiers.shift);
            self.composition = None;
            self.edited();
        }
        if !drag {
            event.focus();
            event.capture_pointer();
        }
    }

    fn caret_area(&self) -> Bounds {
        self.geometry.borrow().caret_area()
    }
}

impl View for Editor {
    fn view(&self, cx: &mut ViewContext<'_, Self>) -> impl IntoElement {
        // Preparation supplies measured caret geometry. One follow-up frame
        // publishes the new IME candidate rectangle after an edit/resize/scroll.
        // A focused caret also blinks; an unfocused editor stays on-demand.
        let geometry = self.geometry.borrow();
        if geometry.epoch != Some(self.epoch) || geometry.focused {
            cx.request_animation_frame();
        }
        let caret_visible = cx.frame_time().as_millis() % 1000 < 600;
        drop(geometry);
        let copy = cx
            .command(commands::Copy, |this, _, cx| {
                if let Err(error) = cx.write_clipboard_text(this.buffer.selected_text()) {
                    eprintln!("Velk clipboard: {error}");
                }
            })
            .enabled(!self.buffer.selection().is_empty())
            .shortcut(Shortcut::primary("c"));
        let cut = cx
            .command(commands::Cut, |this, _, cx| {
                match cx.write_clipboard_text(this.buffer.selected_text()) {
                    Ok(()) => {
                        this.composition = None;
                        this.buffer.delete();
                        this.edited();
                    }
                    Err(error) => eprintln!("Velk clipboard: {error}"),
                }
            })
            .enabled(!self.buffer.selection().is_empty() && self.composition.is_none())
            .shortcut(Shortcut::primary("x"));
        let undo = cx
            .command(commands::Undo, |this, _, _| {
                this.buffer.undo();
                this.edited();
            })
            .enabled(self.buffer.can_undo() && self.composition.is_none())
            .shortcut(Shortcut::primary("z"));
        let redo = cx
            .command(commands::Redo, |this, _, _| {
                this.buffer.redo();
                this.edited();
            })
            .enabled(self.buffer.can_redo() && self.composition.is_none())
            .shortcut(Shortcut::primary("z").shift())
            .shortcut(Shortcut::primary("y"));
        let select_all = cx
            .command(commands::SelectAll, |this, _, _| {
                this.buffer.select_all();
                this.edited();
            })
            .enabled(self.composition.is_none())
            .shortcut(Shortcut::primary("a"));
        let surface = rxui::custom(Surface {
            rope: self.buffer.text().clone(),
            revision: self.buffer.revision(),
            selection: self.buffer.selection(),
            composition: self.composition.clone(),
            geometry: self.geometry.clone(),
            epoch: self.epoch,
            reveal_caret: self.reveal_caret,
            caret_visible,
        })
        .key("editor-surface")
        .fill_width()
        .fill_height()
        .clip()
        .background(ThemeColor::Surface)
        .color(ThemeColor::Text)
        .cursor(Cursor::Text)
        .text_input_client(true)
        .accessibility_role(SemanticRole::MultilineTextInput)
        .accessibility_label("Text editor")
        .ime_cursor_area(self.caret_area())
        .on_key_down(cx.listener(Self::key))
        .on_pointer_down(cx.listener(|this, event, _| this.pointer(event, false)))
        .on_pointer_move(cx.listener(|this, event, _| this.pointer(event, true)))
        .on_pointer_up(cx.listener(|_, event: &PointerInput, _| event.release_pointer()))
        .on_text_insert(cx.listener(|this, event: &TextInsertInput, _| this.insert(&event.text)))
        .on_ime(cx.listener(|this, event: &ImeInput, _| {
            this.composition = match &event.event {
                ImeEvent::Preedit { text, cursor } => Some(Composition {
                    text: text.clone(),
                    cursor: *cursor,
                }),
                ImeEvent::Cancel(_) => None,
            };
            this.edited();
        }))
        .on_wheel(cx.listener(|this, event: &WheelInput, _| {
            this.geometry.borrow_mut().scroll(event.delta);
            this.epoch += 1;
            this.reveal_caret = false;
            event.prevent_default();
        }))
        .on_command(copy)
        .on_command(cut)
        .on_command(undo)
        .on_command(redo)
        .on_command(select_all);
        // Paste uses RXUI's default native clipboard handler, delivered through
        // on_text_insert with TextInsertSource::Paste.
        // A container root keeps native input and preparation on the same
        // path as RXUI's built-in editor examples.
        column().fill_width().fill_height().child(surface)
    }
}
