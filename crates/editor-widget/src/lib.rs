//! Reusable rope-backed text editing state and an RXUI editor view.
mod annotations;
mod blink;
mod buffer;
mod surface;

pub use annotations::{
    AnnotationError, Annotations, Color, EditorAppearance, HoverCard, LinePattern, OverlayKind,
    OverlayPlacement, SpanStyle, StyledRange, TextOverlay, Underline,
};
pub use buffer::{Buffer, Selection};
use rxui::{Bounds, prelude::*, standard_commands as commands};
use std::{cell::RefCell, rc::Rc};
use std::{sync::Arc, time::Duration};
use surface::{Composition, Geometry, HoverHit, Surface};

/// An editor view. Create a separate entity per window; document state can be
/// accessed through `buffer()` and programmatic inserts through `insert()`.
#[derive(Default)]
pub struct Editor {
    buffer: Buffer,
    composition: Option<Composition>,
    geometry: Rc<RefCell<Geometry>>,
    epoch: u64,
    reveal_caret: bool,
    annotations: Option<Arc<Annotations>>,
    appearance: EditorAppearance,
    hovered: Option<HoverHit>,
    hover_task: Option<Task>,
    tooltip_visible: bool,
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

    pub fn set_annotations(&mut self, annotations: Annotations) -> Result<(), AnnotationError> {
        annotations.validate(&self.buffer)?;
        self.annotations = Some(Arc::new(annotations));
        self.hide_hover();
        self.epoch += 1;
        self.reveal_caret = false;
        Ok(())
    }

    pub fn annotations(&self) -> Option<&Annotations> {
        self.annotations.as_deref()
    }

    pub fn clear_annotations(&mut self) {
        self.annotations = None;
        self.hide_hover();
        self.epoch += 1;
        self.reveal_caret = false;
    }

    pub fn set_appearance(&mut self, appearance: EditorAppearance) -> Result<(), AnnotationError> {
        appearance.validate()?;
        self.appearance = appearance;
        self.epoch += 1;
        self.reveal_caret = false;
        Ok(())
    }

    fn hide_hover(&mut self) {
        self.hover_task = None;
        self.hovered = None;
        self.tooltip_visible = false;
    }

    fn hover(&mut self, point: [f32; 2], cx: &mut Context<'_, Self>) {
        let target = self.geometry.borrow().hover(point);
        if self.hovered.as_ref().map(|h| h.id) == target.as_ref().map(|h| h.id) {
            cx.unchanged();
            return;
        }
        self.hide_hover();
        if let Some(target) = target {
            let id = target.id;
            self.hovered = Some(target);
            self.hover_task = cx
                .try_spawn(
                    rxui::sleep(Duration::from_millis(350)),
                    move |this, result, _| {
                        if result.is_ok() && this.hovered.as_ref().is_some_and(|hit| hit.id == id) {
                            this.tooltip_visible = true;
                        }
                    },
                )
                .ok();
        }
    }

    /// Replace the entire document as one undoable edit.
    pub fn set_text(&mut self, text: &str) {
        self.buffer.select_all();
        self.insert(text);
    }

    pub fn insert(&mut self, text: &str) {
        self.composition = None;
        self.buffer.insert(text);
        self.edited();
    }

    fn edited(&mut self) {
        if self
            .annotations
            .as_ref()
            .is_some_and(|a| a.document_revision != self.buffer.revision())
        {
            self.annotations = None;
        }
        self.hide_hover();
        self.epoch += 1;
        self.reveal_caret = true;
    }

    fn key(&mut self, event: &KeyInput, cx: &mut Context<'_, Self>) {
        let key = &event.event;
        if !key.pressed || self.composition.is_some() {
            return;
        }
        let had_hover = self.hovered.is_some();
        self.hide_hover();
        let extend = key.modifiers.shift;
        // Leave unsupported modifier chords untouched. Word movement will be
        // added in a later editor increment.
        if key.modifiers.alt || key.modifiers.control && !key.modifiers.primary() {
            if !had_hover {
                cx.unchanged();
            }
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
        } else if !had_hover {
            cx.unchanged();
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
        // One follow-up frame publishes measured IME/overlay geometry. Caret
        // blinking is timer-driven rather than a continuous animation loop.
        let geometry = self.geometry.borrow();
        if geometry.epoch != Some(self.epoch) {
            cx.request_animation_frame();
        }
        let blink_geometry = self.geometry.clone();
        let blink = cx.use_state(move |cx| blink::Blink::new(blink_geometry, cx));
        let caret_visible = !blink.read(cx).hidden || geometry.epoch != Some(self.epoch);
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
            annotations: self.annotations.clone(),
            selection_color: self.appearance.selection,
        })
        .key("editor-surface")
        .fill_width()
        .fill_height()
        .clip()
        .background(self.appearance.background)
        .color(self.appearance.foreground)
        .cursor(Cursor::Text)
        .text_input_client(true)
        .accessibility_role(SemanticRole::MultilineTextInput)
        .accessibility_label("Text editor")
        .ime_cursor_area(self.caret_area())
        .on_key_down(cx.listener(Self::key))
        .on_pointer_down(cx.listener(|this, event, _| this.pointer(event, false)))
        .on_pointer_move(cx.listener(|this, event: &PointerInput, cx| {
            if event.buttons.contains(PointerButton::Primary) {
                this.pointer(event, true);
            } else {
                this.hover(event.local_position, cx);
            }
        }))
        .on_hover(cx.listener(|this, event: &HoverEvent, cx| {
            if !event.hovered {
                this.hide_hover();
            } else {
                cx.unchanged();
            }
        }))
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
            this.hide_hover();
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
        let mut root = column().fill_width().fill_height().child(surface);
        if self.tooltip_visible
            && let Some(hit) = &self.hovered
        {
            root = root.child(
                popover(
                    hit.anchor,
                    column()
                        .gap(6.)
                        .child(
                            label(hit.card.title.clone())
                                .font_size(14.)
                                .color(ThemeColor::Text),
                        )
                        .child(
                            label(hit.card.body.clone())
                                .font_size(12.)
                                .color(ThemeColor::TextMuted),
                        ),
                )
                .key("editor-hover")
                .width(360.)
                .padding(12.)
                .autofocus(false)
                .on_dismiss(cx.listener(|this, _: &DismissEvent, _| this.hide_hover()))
                .into_element()
                .pointer_events(PointerEvents::None),
            );
        }
        root
    }
}
