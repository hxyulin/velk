//! Reusable rope-backed text editing state and an RXUI editor view.
mod annotations;
mod blink;
mod buffer;
mod composition;
mod graphemes;
mod horizontal;
mod inline;
mod surface;
mod syntax;
mod unicode_window;
mod viewport;
#[cfg(feature = "performance")]
#[doc(hidden)]
pub use viewport::CpuViewportProbe;
#[cfg(any(test, feature = "performance"))]
pub use viewport::FrameMetrics;

pub use annotations::{
    AnnotationError, Annotations, Color, EditorAppearance, HoverCard, LinePattern, OverlayKind,
    OverlayPlacement, SpanStyle, StyledRange, TextOverlay, Underline,
};
pub use buffer::{Buffer, Checkpoint, Selection, TextEdit, Transaction, TransactionError};
use rxui::{prelude::*, standard_commands as commands};
#[cfg(any(test, feature = "performance"))]
use std::time::Instant;
use std::{cell::RefCell, rc::Rc};
use std::{sync::Arc, time::Duration};
use surface::{Composition, Geometry, HoverHit, Surface};
pub use velk_syntax::Language;

/// An editor view. Create a separate entity per window; document state can be
/// accessed through `buffer()` and programmatic inserts through `insert()`.
#[derive(Default)]
pub struct Editor {
    buffer: Buffer,
    syntax_worker: Option<Arc<velk_syntax::Worker>>,
    syntax_revision: u64,
    syntax_demand: Option<syntax::Demand>,
    syntax_snapshot: Option<Arc<velk_syntax::HighlightSnapshot>>,
    composition: Option<Composition>,
    geometry: Rc<RefCell<Geometry>>,
    epoch: u64,
    caret_epoch: u64,
    #[cfg(any(test, feature = "performance"))]
    input_started: Option<Instant>,
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

    /// Load a UTF-8 document without creating a full-document String.
    pub fn from_reader(reader: impl std::io::Read) -> std::io::Result<Self> {
        Ok(Self {
            buffer: Buffer::from_reader(reader)?,
            ..Self::default()
        })
    }

    /// Enable an in-process language parser. Parsing runs off the UI thread.
    pub fn set_language(
        &mut self,
        language: Option<Language>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.syntax_worker = language
            .map(velk_syntax::Worker::new)
            .transpose()?
            .map(Arc::new);
        self.syntax_revision = self.buffer.revision();
        self.syntax_snapshot = None;
        self.syntax_demand = self
            .syntax_worker
            .as_ref()
            .map(|worker| syntax::Demand::new(worker.clone()));
        if let Some(worker) = &self.syntax_worker {
            worker.request(
                self.buffer.revision(),
                self.buffer.text().clone(),
                Vec::new(),
            );
        }
        self.epoch += 1;
        Ok(())
    }

    /// Most recently presented syntax revision, for opt-in performance probes.
    #[cfg(feature = "performance")]
    pub fn syntax_revision(&self) -> Option<u64> {
        self.syntax_snapshot
            .as_ref()
            .map(|snapshot| snapshot.revision)
    }

    /// Preparation diagnostics from the most recent native frame.
    #[cfg(any(test, feature = "performance"))]
    pub fn frame_metrics(&self) -> FrameMetrics {
        self.geometry.borrow().metrics
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

    /// Present a semantic hover requested at the current caret. The caller must
    /// validate its document revision, request identity, and caret before use.
    pub fn show_hover_at_caret(&mut self, card: HoverCard) {
        self.hide_hover();
        let geometry = self.geometry.borrow();
        let caret = geometry.caret_area();
        self.hovered = Some(HoverHit {
            id: usize::MAX,
            card,
            anchor: [
                geometry.origin[0] + caret.x,
                geometry.origin[1] + caret.y + caret.height,
            ],
        });
        self.tooltip_visible = true;
        self.reveal_caret = false;
        self.epoch += 1;
    }

    fn hide_hover(&mut self) {
        if self.hovered.is_some() || self.tooltip_visible {
            self.epoch += 1;
            self.reveal_caret = false;
        }
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
            self.epoch += 1;
            self.reveal_caret = false;
            self.hover_task = cx
                .try_spawn(
                    rxui::sleep(Duration::from_millis(350)),
                    move |this, result, _| {
                        if result.is_ok() && this.hovered.as_ref().is_some_and(|hit| hit.id == id) {
                            this.tooltip_visible = true;
                            this.epoch += 1;
                            this.reveal_caret = false;
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
        #[cfg(any(test, feature = "performance"))]
        {
            self.input_started = Some(Instant::now());
        }
        self.composition = None;
        self.buffer.insert(text);
        self.edited();
    }

    /// Commit programmatic edits as one undo step and one UI/syntax update.
    pub fn apply_transaction(
        &mut self,
        transaction: Transaction,
    ) -> Result<bool, TransactionError> {
        let changed = self.buffer.apply_transaction(transaction)?;
        self.composition = None;
        self.edited();
        Ok(changed)
    }

    pub fn mark_saved(&mut self, checkpoint: Checkpoint) -> bool {
        self.buffer.mark_saved(checkpoint)
    }

    fn edited(&mut self) {
        if self.syntax_revision != self.buffer.revision() {
            self.syntax_revision = self.buffer.revision();
            if let Some(worker) = &self.syntax_worker {
                worker.request(
                    self.buffer.revision(),
                    self.buffer.text().clone(),
                    self.buffer
                        .edits_since(self.buffer.revision().saturating_sub(128))
                        .unwrap_or_default(),
                );
            }
        }
        self.caret_epoch += 1;
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
        #[cfg(any(test, feature = "performance"))]
        let input_started = Instant::now();
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
            #[cfg(any(test, feature = "performance"))]
            {
                self.input_started = Some(input_started);
            }
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
        #[cfg(any(test, feature = "performance"))]
        let input_started = Instant::now();
        let position = self.geometry.borrow().hit(event.local_position);
        if let Some(position) = position {
            #[cfg(any(test, feature = "performance"))]
            {
                self.input_started = Some(input_started);
            }
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
}

impl View for Editor {
    fn view(&self, cx: &mut ViewContext<'_, Self>) -> impl IntoElement {
        let blink_geometry = self.geometry.clone();
        let caret_epoch = self.caret_epoch;
        let blink = cx.use_state(move |cx| blink::Blink::new(blink_geometry, caret_epoch, cx));
        let caret_visible = blink.read(cx).visible(caret_epoch);
        if let Some(worker) = &self.syntax_worker {
            let worker = worker.clone();
            let id = worker.id;
            let publish = cx.listener(move |this, update: &velk_syntax::Update, cx| {
                let snapshot = match update {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        eprintln!("Velk syntax: {error}");
                        cx.unchanged();
                        return;
                    }
                };
                if this
                    .syntax_worker
                    .as_ref()
                    .is_none_or(|worker| worker.id != id)
                    || snapshot.revision > this.buffer.revision()
                    || this.syntax_snapshot.as_ref().is_some_and(|old| {
                        old.revision > snapshot.revision
                            || old.revision == snapshot.revision
                                && old.viewport >= snapshot.viewport
                    })
                {
                    cx.unchanged();
                    return;
                }
                this.syntax_snapshot = Some(snapshot.clone());
                this.epoch += 1;
                this.reveal_caret = false;
            });
            cx.use_keyed_state(id, move |cx| syntax::Monitor::new(worker, publish, cx));
        }
        let syntax = self
            .syntax_snapshot
            .as_ref()
            .and_then(|snapshot| syntax::SyntaxView::new(snapshot.clone(), &self.buffer));
        let ime_geometry = self.geometry.clone();
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
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
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
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
                this.buffer.undo();
                this.edited();
            })
            .enabled(self.buffer.can_undo() && self.composition.is_none())
            .shortcut(Shortcut::primary("z"));
        let redo = cx
            .command(commands::Redo, |this, _, _| {
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
                this.buffer.redo();
                this.edited();
            })
            .enabled(self.buffer.can_redo() && self.composition.is_none())
            .shortcut(Shortcut::primary("z").shift())
            .shortcut(Shortcut::primary("y"));
        let select_all = cx
            .command(commands::SelectAll, |this, _, _| {
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
                this.buffer.select_all();
                this.edited();
            })
            .enabled(self.composition.is_none())
            .shortcut(Shortcut::primary("a"));
        let surface = rxui::custom(viewport::ViewportSurface(Surface {
            rope: self.buffer.text().clone(),
            revision: self.buffer.revision(),
            selection: self.buffer.selection(),
            composition: self.composition.clone(),
            preedit: None,
            geometry: self.geometry.clone(),
            epoch: self.epoch,
            #[cfg(any(test, feature = "performance"))]
            input_started: self.input_started,
            reveal_caret: self.reveal_caret,
            caret_visible,
            minimum_line_metrics: None,
            syntax,
            syntax_demand: self
                .syntax_demand
                .as_ref()
                .map(|demand| demand.at_revision(self.buffer.revision())),
            annotations: self.annotations.clone(),
            annotation_groups: None,
            selection_color: self.appearance.selection,
        }))
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
        .ime_cursor_area_with(move || Some(ime_geometry.borrow().caret_area()))
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
            #[cfg(any(test, feature = "performance"))]
            {
                this.input_started = Some(Instant::now());
            }
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
            #[cfg(any(test, feature = "performance"))]
            {
                this.input_started = Some(Instant::now());
            }
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

#[cfg(test)]
mod responsiveness_tests {
    use super::*;
    use rxui::{KeyEvent, Modifiers, TestUi, TextInputEvent, astrelis::GraphicsContext};
    use std::time::Duration;

    fn draw_and_check(test: &mut TestUi<Editor>, frame: u64) -> rxui::Bounds {
        test.runtime().begin_frame(Duration::from_millis(frame * 8));
        test.render().unwrap();
        let expected = test.read(|editor| {
            let geometry = editor.geometry.borrow();
            assert_eq!(geometry.epoch, Some(editor.epoch));
            let mut area = geometry.caret_area();
            area.x += geometry.origin[0];
            area.y += geometry.origin[1];
            area
        });
        let (runtime, ui, painter) = test.parts();
        assert_eq!(ui.ime_cursor_area(painter).unwrap(), Some(expected));
        assert!(
            !runtime.animation_frame_requested(),
            "caret geometry requires no follow-up frame"
        );
        expected
    }

    #[test]
    fn ime_tracks_the_painted_caret_in_one_frame_after_navigation_resize_and_composition() {
        let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
        let mut test =
            TestUi::new(&graphics, [640, 240], Editor::new(&"x".repeat(10_000))).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        let id = test.element("editor-surface").unwrap().unwrap().id;
        test.parts().1.focus(id);
        let start = draw_and_check(&mut test, 0);
        for (frame, key) in [(1, KeyboardKey::End), (2, KeyboardKey::ArrowLeft)] {
            test.key(KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            })
            .unwrap();
            let caret = draw_and_check(&mut test, frame);
            assert!(caret.x > start.x);
            assert!(caret.x + caret.width <= 640.);
        }
        test.resize([320, 160]);
        let caret = draw_and_check(&mut test, 3);
        assert!(caret.x + caret.width <= 320.);
        test.text_input(TextInputEvent::Preedit {
            text: "かんじ".into(),
            cursor: Some((0, 9)),
        })
        .unwrap();
        draw_and_check(&mut test, 4);
        assert!(test.read(|e| e.frame_metrics().composition_projected));
        assert_eq!(test.read(|e| e.frame_metrics().unicode_horizontal_lines), 1);
        assert!(test.read(|e| e.frame_metrics().cached_source_bytes) < 4000);
        assert_eq!(test.read(|e| e.buffer.text().len_chars()), 10_000);
        test.text_input(TextInputEvent::Commit("漢字".into()))
            .unwrap();
        draw_and_check(&mut test, 5);
        assert_eq!(test.read(|e| e.buffer.text().len_chars()), 10_002);
        test.text_input(TextInputEvent::Preedit {
            text: "あ".into(),
            cursor: None,
        })
        .unwrap();
        draw_and_check(&mut test, 6);
        test.text_input(TextInputEvent::CancelComposition).unwrap();
        draw_and_check(&mut test, 7);
        assert_eq!(test.read(|e| e.buffer.text().len_chars()), 10_002);
    }

    #[test]
    fn projected_ime_hits_and_multiline_replacement_restore_without_changing_source() {
        let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
        let source = "aébc 👩‍💻 def\n".repeat(1000);
        let mut test = TestUi::new(&graphics, [640, 240], Editor::new(&source)).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        let id = test.element("editor-surface").unwrap().unwrap().id;
        test.parts().1.focus(id);
        let entity = test.entity().clone();
        let start = test.read(|e| e.buffer.text().line_to_char(500) + 1);
        test.runtime().update(|cx| {
            entity.update(cx, |e, _| {
                e.buffer.set_caret(start, false);
                e.buffer.set_caret(start + 2, true);
                e.edited();
            })
        });
        test.text_input(TextInputEvent::Preedit {
            text: "かんじ".into(),
            cursor: Some((0, 3)),
        })
        .unwrap();
        let caret = draw_and_check(&mut test, 0);
        test.read(|e| {
            let geometry = e.geometry.borrow();
            assert!(!geometry.metrics.composition_fallback);
            assert!(geometry.metrics.cached_source_bytes < 2000);
            assert_eq!(
                geometry.hit([caret.x - 1., caret.y + caret.height / 2.]),
                Some(start)
            );
        });
        test.text_input(TextInputEvent::CancelComposition).unwrap();
        draw_and_check(&mut test, 1);
        test.runtime().update(|cx| {
            entity.update(cx, |e, _| {
                e.buffer.set_caret(start, false);
                e.buffer.set_caret(start + 30, true);
                e.edited();
            })
        });
        test.text_input(TextInputEvent::Preedit {
            text: "漢字".into(),
            cursor: Some((0, 6)),
        })
        .unwrap();
        draw_and_check(&mut test, 2);
        assert!(test.read(|e| e.frame_metrics().composition_projected));
        assert!(!test.read(|e| e.frame_metrics().composition_fallback));
        test.text_input(TextInputEvent::CancelComposition).unwrap();
        draw_and_check(&mut test, 3);
        assert!(!test.read(|e| e.frame_metrics().composition_fallback));
        assert_eq!(test.read(|e| e.buffer.text().to_string()), source);
    }
}
