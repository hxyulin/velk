//! RXUI editor view over the UI-independent Velk Core buffer.
mod annotations;
mod blink;
mod completion;
mod composition;
mod documentation;
mod horizontal;
mod hover;
mod signature;
mod snippet_choices;
mod surface;
mod syntax;
mod unicode_window;
mod viewport;
#[cfg(feature = "performance")]
#[doc(hidden)]
pub use viewport::CpuViewportProbe;
#[cfg(any(test, feature = "performance"))]
pub use viewport::FrameMetrics;

use rxui::{Element, prelude::*, standard_commands as commands};
use std::sync::Arc;
#[cfg(any(test, feature = "performance"))]
use std::time::Instant;
use std::{cell::RefCell, rc::Rc};
pub use surface::MONOSPACE_FAMILY;
use surface::{Composition, Geometry, Surface};
use velk_core::graphemes;
pub use velk_core::{
    Buffer, BufferSnapshot, Checkpoint, Selection, TextEdit, Transaction, TransactionError,
};
use velk_editor_model::inline;
pub use velk_editor_model::{
    AnnotationError, Annotations, Color, EditCommand, EditorAppearance, HoverCard, HoverSource,
    HoverTarget, LinePattern, OverlayKind, OverlayPlacement, SpanStyle, StyledRange, TextOverlay,
    Underline,
};
pub use velk_syntax::Language;

/// One view of a Core document. Use `new_view` for independent selection, input,
/// scrolling, and rendering over shared text and undo history.
pub struct Editor {
    buffer: Buffer,
    document_revision: u64,
    editing: velk_editor_model::editing::Session,
    reserved_shortcuts: Vec<Shortcut>,
    syntax_worker: Option<Arc<velk_syntax::Worker>>,
    language: Option<Language>,
    syntax_provider: Option<Arc<dyn velk_syntax::Provider>>,
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
    hover: hover::State,
    completion: completion::State,
    signature: signature::State,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            buffer: Buffer::with_history("", Arc::new(velk_undo_tree::Tree)),
            document_revision: Default::default(),
            editing: Default::default(),
            reserved_shortcuts: Default::default(),
            syntax_worker: Default::default(),
            language: Default::default(),
            syntax_provider: Default::default(),
            syntax_revision: Default::default(),
            syntax_demand: Default::default(),
            syntax_snapshot: Default::default(),
            composition: Default::default(),
            geometry: Default::default(),
            epoch: Default::default(),
            caret_epoch: Default::default(),
            #[cfg(any(test, feature = "performance"))]
            input_started: Default::default(),
            reveal_caret: Default::default(),
            annotations: Default::default(),
            appearance: Default::default(),
            hover: Default::default(),
            completion: Default::default(),
            signature: Default::default(),
        }
    }
}

struct ReservedShortcut;
impl Command for ReservedShortcut {}
struct KeymapShortcut<const SLOT: usize>;
impl<const SLOT: usize> Command for KeymapShortcut<SLOT> {}

impl Editor {
    fn default_shortcut<C: Command>(
        &self,
        action: CommandAction<C>,
        shortcut: Shortcut,
    ) -> CommandAction<C> {
        if self.reserved_shortcuts.contains(&shortcut) {
            action
        } else {
            action.shortcut(shortcut)
        }
    }

    /// Reserve host-owned bindings ahead of editing commands in native menu lookup.
    /// The host's capture listener executes them; this action only blocks accelerators.
    pub fn set_reserved_shortcuts(&mut self, shortcuts: Vec<Shortcut>) -> bool {
        if self.reserved_shortcuts == shortcuts {
            return false;
        }
        self.reserved_shortcuts = shortcuts;
        true
    }
    pub fn set_editing_providers(
        &mut self,
        providers: std::collections::BTreeMap<velk_editing::Role, Arc<dyn velk_editing::Provider>>,
    ) {
        self.editing.set_providers(providers);
        self.keymap("", false, false, false, false);
    }
    pub fn caret_position(&self) -> usize {
        self.editing.caret_position(&self.buffer)
    }
    pub fn selection_regions(
        &self,
    ) -> Option<Arc<Vec<velk_editor_model::selections::SelectionRegion>>> {
        self.editing.selection_regions()
    }
    pub fn selected_text(&self) -> String {
        self.editing.selected_text(&self.buffer)
    }
    /// Stable keymap mode, independent of status labels and pending operator text.
    pub fn input_mode_id(&self) -> &str {
        let state = self.editing.input_state();
        if state.mode.is_empty() {
            if state.capture_text {
                "command"
            } else {
                "insert"
            }
        } else {
            &state.mode
        }
    }
    pub fn input_mode(&self) -> Option<&str> {
        let label = &self.editing.input_state().label;
        (!label.is_empty()).then_some(label.as_str())
    }
    fn keymap(&mut self, key: &str, control: bool, alt: bool, meta: bool, text: bool) -> bool {
        let handled = self.interactive(velk_editing::Intent::Key(velk_editing::input::Key {
            key: key.into(),
            text,
            control,
            alt,
            meta,
            state: Default::default(),
        }));
        if handled {
            self.buffer.break_undo_group();
        }
        handled
    }
    fn bind_keymap_shortcut<const SLOT: usize>(
        &self,
        surface: Element,
        cx: &mut ViewContext<'_, Self>,
    ) -> Element {
        let Some(key) = self.editing.input_state().control_keys.get(SLOT).cloned() else {
            return surface;
        };
        let shortcut = Shortcut::new(
            KeyboardKey::Character(key.clone()),
            Modifiers {
                control: true,
                ..Default::default()
            },
        );
        if self.reserved_shortcuts.contains(&shortcut) {
            return surface;
        }
        surface.on_command(
            cx.command(KeymapShortcut::<SLOT>, move |this, _, _| {
                this.keymap(&key, true, false, false, false);
            })
            .shortcut(shortcut),
        )
    }
    /// Apply a plugin editing intent. False asks the caller to use its ordinary fallback.
    pub fn apply_editing_intent(&mut self, intent: velk_editing::Intent) -> bool {
        self.interactive(intent)
    }
    fn interactive(&mut self, intent: velk_editing::Intent) -> bool {
        let preserve_selection = matches!(
            intent,
            velk_editing::Intent::Indent | velk_editing::Intent::Outdent
        ) && !self.buffer.selection().is_empty();
        let capture_key = matches!(intent, velk_editing::Intent::Key(_))
            && self.editing.input_state().capture_text;
        match self.editing.evaluate(&mut self.buffer, intent) {
            Ok(true) => {
                self.composition = None;
                self.edited();
                true
            }
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(%error, "interactive editing provider failed");
                // A failed indentation request must not replace selected text
                // with the plain Tab fallback.
                preserve_selection || capture_key
            }
        }
    }
    fn committed_text(&mut self, text: &str, source: rxui::TextInsertSource) {
        if source == rxui::TextInsertSource::Ime && self.editing.input_state().capture_text {
            return;
        }
        if source == rxui::TextInsertSource::Keyboard
            && self.composition.is_none()
            && self.keymap(text, false, false, false, true)
        {
            return;
        }
        if source == rxui::TextInsertSource::Keyboard
            && self.composition.is_none()
            && self.commit_completion_character(text)
        {
            return;
        }
        let revision = self.buffer.revision();
        if source == rxui::TextInsertSource::Keyboard
            && self.composition.is_none()
            && self.interactive(velk_editing::Intent::Type(text.into()))
        {
            self.buffer.break_undo_group();
            if self.buffer.revision() != revision {
                self.record_completion_typing(text);
            }
            return;
        }
        if matches!(
            source,
            rxui::TextInsertSource::Paste | rxui::TextInsertSource::Ime
        ) {
            self.execute(EditCommand::Paste(text));
        } else {
            self.insert(text);
            if self.buffer.revision() != revision {
                self.record_completion_typing(text);
            }
        }
    }
    pub fn new(text: &str) -> Self {
        Self::from_buffer(velk_core::Buffer::with_history(
            text,
            std::sync::Arc::new(velk_undo_tree::Tree),
        ))
    }

    /// Attach independently loaded document state to a UI view without copying text.
    pub fn from_buffer(buffer: Buffer) -> Self {
        Self {
            document_revision: buffer.revision(),
            buffer,
            ..Self::default()
        }
    }

    pub fn new_view(&self) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut view = Self::from_buffer(self.buffer.new_view());
        view.appearance = self.appearance;
        view.reserved_shortcuts = self.reserved_shortcuts.clone();
        view.editing = self.editing.new_view();
        view.keymap("", false, false, false, false);
        view.annotations = self.annotations.clone();
        view.hover.enabled = self.hover.enabled;
        view.geometry.borrow_mut().offset = self.scroll_offset();
        view.set_language(self.language.clone())?;
        view.set_syntax_provider(self.syntax_provider.clone())?;
        Ok(view)
    }
    pub fn shared_annotations(&self) -> Option<Arc<Annotations>> {
        self.annotations.clone()
    }
    pub fn semantic_hover_enabled(&self) -> bool {
        self.hover.enabled
    }
    /// Reconcile presentation after another view or a host operation edits the
    /// shared document. Preserve this view's scroll instead of revealing its caret.
    pub fn synchronize_document(
        &mut self,
        annotations: Option<Arc<Annotations>>,
        hover: bool,
    ) -> bool {
        let revision = self.buffer.revision();
        let mut changed = self.document_revision != revision;
        if changed {
            self.editing.cancel_snippet();
            self.composition = None;
            self.edited();
            self.reveal_caret = false;
        }
        let annotations = annotations.filter(|a| a.document_revision == revision);
        let same = match (&self.annotations, &annotations) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.annotations = annotations;
            self.refresh_hover_annotations();
            self.epoch += 1;
            changed = true;
        }
        if self.hover.enabled != hover {
            self.set_semantic_hover(hover);
            changed = true;
        }
        changed
    }
    pub fn restore_view(&mut self, selection: Selection, scroll: [f32; 2]) -> Result<(), String> {
        if scroll.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err("invalid view scroll position".into());
        }
        self.buffer.set_selection_preserving_column(selection);
        self.geometry.borrow_mut().offset = scroll;
        self.edited();
        self.reveal_caret = false;
        Ok(())
    }
    pub fn cancel_composition(&mut self) {
        if self.composition.take().is_some() {
            self.epoch += 1;
            self.reveal_caret = false;
        }
    }

    /// Load a UTF-8 document without creating a full-document String.
    pub fn from_reader(reader: impl std::io::Read) -> std::io::Result<Self> {
        Ok(Self::from_buffer(
            velk_core::Buffer::from_reader_with_history(
                reader,
                std::sync::Arc::new(velk_undo_tree::Tree),
            )?,
        ))
    }

    /// Recreate document state; rendering, syntax and semantic caches start fresh.
    pub fn from_session(bytes: &[u8], scroll: [f32; 2]) -> Result<Self, String> {
        if scroll.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err("invalid scroll position".into());
        }
        let editor = Self::from_buffer(velk_core::Buffer::restore_with_history(
            bytes,
            std::sync::Arc::new(velk_undo_tree::Tree),
        )?);
        editor.geometry.borrow_mut().offset = scroll;
        Ok(editor)
    }
    pub fn has_composition(&self) -> bool {
        self.composition.is_some()
    }
    pub fn scroll_offset(&self) -> [f32; 2] {
        self.geometry.borrow().offset
    }
    pub fn language(&self) -> Option<Language> {
        self.language.clone()
    }
    /// Set the document language. A selected syntax service parses off the UI thread.
    pub fn set_language(
        &mut self,
        language: Option<Language>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.language == language {
            return Ok(());
        }
        self.language = language;
        self.restart_syntax()
    }
    pub fn set_syntax_provider(
        &mut self,
        provider: Option<Arc<dyn velk_syntax::Provider>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let unchanged = match (&self.syntax_provider, &provider) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return Ok(());
        }
        self.syntax_provider = provider;
        self.restart_syntax()
    }
    fn restart_syntax(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.syntax_worker = None;
        self.syntax_snapshot = None;
        self.syntax_demand = None;
        self.epoch += 1;
        self.syntax_worker = self
            .language
            .clone()
            .zip(self.syntax_provider.clone())
            .map(|(language, provider)| velk_syntax::Worker::new(provider, language))
            .transpose()?
            .map(Arc::new);
        self.syntax_revision = self.buffer.revision();
        self.syntax_snapshot = None;
        self.syntax_demand = self
            .syntax_worker
            .as_ref()
            .map(|worker| syntax::Demand::new(worker.clone()));
        if let Some(worker) = &self.syntax_worker {
            worker.request(self.buffer.revision(), self.buffer.text(), Vec::new());
        }
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

    pub fn break_undo_group(&mut self) {
        self.buffer.break_undo_group();
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Select a result from a versioned external service without creating an
    /// edit or undo entry. Clamp endpoints to the buffer's grapheme boundaries.
    pub fn select_range(&mut self, range: std::ops::Range<usize>) -> bool {
        self.execute(EditCommand::SelectRange(range))
    }

    pub fn set_annotations(&mut self, annotations: Annotations) -> Result<(), AnnotationError> {
        annotations.validate(&self.buffer)?;
        self.annotations = Some(Arc::new(annotations));
        self.refresh_hover_annotations();
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

    /// Replace the entire document as one undoable edit.
    pub fn set_text(&mut self, text: &str) {
        self.execute(EditCommand::ReplaceAll(text));
    }

    #[tracing::instrument(level = "debug", skip_all, fields(bytes = text.len(), revision = self.buffer.revision()))]
    pub fn insert(&mut self, text: &str) {
        self.execute(EditCommand::Insert(text));
    }

    /// Apply editor intent and refresh syntax, annotations, hover, and caret state.
    pub fn execute(&mut self, command: EditCommand<'_>) -> bool {
        let group = match &command {
            EditCommand::Insert(text)
                if !text.contains(['\n', '\r']) && self.buffer.selection().is_empty() =>
            {
                Some(velk_core::UndoGroup::Typing)
            }
            EditCommand::Backspace if self.buffer.selection().is_empty() => {
                Some(velk_core::UndoGroup::Backspace)
            }
            EditCommand::Delete if self.buffer.selection().is_empty() => {
                Some(velk_core::UndoGroup::Delete)
            }
            _ => None,
        };
        if let Some(kind) = group {
            self.buffer.begin_undo_group(kind);
        } else {
            self.buffer.break_undo_group();
        }
        let accepted = self.execute_inner(command);
        self.buffer.end_undo_group();
        accepted
    }

    fn execute_inner(&mut self, command: EditCommand<'_>) -> bool {
        self.note_input();
        #[cfg(any(test, feature = "performance"))]
        {
            self.input_started = Some(Instant::now());
        }
        if matches!(command, EditCommand::Undo | EditCommand::Redo) {
            self.editing.clear_pairs();
        }
        let was_visual = self.editing.has_visual_selection();
        match self.editing.edit_command(&mut self.buffer, &command) {
            Ok(true) => {}
            Ok(false) => {
                if !command.apply(&mut self.buffer) {
                    return false;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "multi-selection edit failed");
                return false;
            }
        }
        if was_visual && !self.editing.has_visual_selection() {
            self.keymap("", false, false, false, false);
        }
        self.composition = None;
        self.edited();
        true
    }

    /// Commit programmatic edits as one undo step and one UI/syntax update.
    pub fn apply_transaction(
        &mut self,
        transaction: Transaction,
    ) -> Result<bool, TransactionError> {
        self.editing.cancel_snippet();
        self.buffer.break_undo_group();
        let changed = self.buffer.apply_transaction(transaction)?;
        self.composition = None;
        self.edited();
        Ok(changed)
    }

    pub fn checkout_history(&mut self, revision: u64, state: u64) -> bool {
        if !self.buffer.checkout_history(revision, state) {
            return false;
        }
        self.editing = self.editing.new_view();
        self.composition = None;
        self.edited();
        true
    }

    pub fn mark_saved(&mut self, checkpoint: Checkpoint) -> bool {
        self.buffer.mark_saved(checkpoint)
    }

    fn note_input(&mut self) {
        if tracing::enabled!(tracing::Level::DEBUG) {
            static INPUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let id = INPUT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.geometry.borrow_mut().traced_input = Some((std::time::Instant::now(), id));
            tracing::debug!(
                input_id = id,
                revision = self.buffer.revision(),
                "input received"
            );
        }
    }
    pub fn start_snippet(
        &mut self,
        stops: Vec<velk_commands::snippet::Tabstop>,
        provider: Option<std::sync::Arc<dyn velk_commands::snippet::Provider>>,
    ) -> Result<(), String> {
        self.dismiss_completions();
        self.editing
            .start_snippet(&mut self.buffer, stops, provider)?;
        self.edited();
        Ok(())
    }
    pub fn snippet_active(&self) -> bool {
        self.editing.snippet_active()
    }
    pub fn navigate_snippet(&mut self, backwards: bool) -> bool {
        let handled = self.editing.navigate_snippet(&mut self.buffer, backwards);
        if handled {
            self.dismiss_completions();
            self.edited();
        }
        handled
    }
    fn edited(&mut self) {
        self.update_completion();
        self.document_revision = self.buffer.revision();
        let visual = self.editing.has_visual_selection();
        self.editing.synchronize(&self.buffer);
        if visual && !self.editing.has_visual_selection() {
            self.keymap("", false, false, false, false);
        }
        tracing::debug!(
            revision = self.buffer.revision(),
            bytes = self.buffer.text().len_bytes(),
            "editor state updated"
        );
        if self.syntax_revision != self.buffer.revision() {
            self.syntax_revision = self.buffer.revision();
            if let Some(worker) = &self.syntax_worker {
                worker.request(
                    self.buffer.revision(),
                    self.buffer.text(),
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

    #[tracing::instrument(level = "debug", skip_all, fields(revision = self.buffer.revision()))]
    fn key(&mut self, event: &KeyInput, cx: &mut Context<'_, Self>) {
        self.note_input();
        #[cfg(any(test, feature = "performance"))]
        let input_started = Instant::now();
        let key = &event.event;
        if !key.pressed || self.composition.is_some() {
            return;
        }
        let had_hover = self.has_hover();
        self.hide_hover();
        if key.key == KeyboardKey::Escape {
            self.editing.cancel_snippet();
        }
        if key.key == KeyboardKey::Tab
            && !key.modifiers.control
            && !key.modifiers.meta
            && !key.modifiers.alt
            && self.navigate_snippet(key.modifiers.shift)
        {
            return;
        }
        if self.snippet_choice_key(event) {
            return;
        }
        if self.signature_key(event) {
            return;
        }
        if self.completion_key(event) {
            return;
        }
        use velk_editor_model::commands::{Key, Modifiers, Platform, key_command};
        let input_key = match &key.key {
            KeyboardKey::Escape => Some("Escape"),
            KeyboardKey::ArrowLeft => Some("ArrowLeft"),
            KeyboardKey::ArrowRight => Some("ArrowRight"),
            KeyboardKey::ArrowUp => Some("ArrowUp"),
            KeyboardKey::ArrowDown => Some("ArrowDown"),
            KeyboardKey::Home => Some("Home"),
            KeyboardKey::End => Some("End"),
            KeyboardKey::Backspace => Some("Backspace"),
            KeyboardKey::Delete => Some("Delete"),
            KeyboardKey::Enter => Some("Enter"),
            KeyboardKey::Tab => Some("Tab"),
            KeyboardKey::Character(text)
                if key.modifiers.control || key.modifiers.meta || key.modifiers.alt =>
            {
                Some(text.as_str())
            }
            _ => None,
        };
        if input_key.is_some_and(|input| {
            self.keymap(
                input,
                key.modifiers.control,
                key.modifiers.alt,
                key.modifiers.meta,
                false,
            )
        }) {
            event.prevent_default();
            return;
        }
        let key_name = match key.key {
            KeyboardKey::Home => Key::Home,
            KeyboardKey::End => Key::End,
            KeyboardKey::ArrowLeft => Key::Left,
            KeyboardKey::ArrowRight => Key::Right,
            KeyboardKey::ArrowUp => Key::Up,
            KeyboardKey::ArrowDown => Key::Down,
            KeyboardKey::Backspace => Key::Backspace,
            KeyboardKey::Delete => Key::Delete,
            KeyboardKey::Enter => Key::Enter,
            KeyboardKey::Tab => Key::Tab,
            _ => Key::Other,
        };
        let modifiers = Modifiers {
            shift: key.modifiers.shift,
            control: key.modifiers.control,
            alt: key.modifiers.alt,
            meta: key.modifiers.meta,
        };
        let platform = if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Other
        };
        let intent = if !modifiers.control && !modifiers.alt && !modifiers.meta {
            match key_name {
                Key::Tab => Some(if modifiers.shift {
                    velk_editing::Intent::Outdent
                } else {
                    velk_editing::Intent::Indent
                }),
                Key::Enter if !modifiers.shift => Some(velk_editing::Intent::Newline),
                Key::Backspace => Some(velk_editing::Intent::Backspace),
                _ => None,
            }
        } else {
            None
        };
        if intent.is_some_and(|intent| self.interactive(intent)) {
            event.prevent_default();
        } else if let Some(command) = key_command(key_name, modifiers, platform) {
            self.execute(command);
            #[cfg(any(test, feature = "performance"))]
            {
                self.input_started = Some(input_started);
            }
            event.prevent_default();
        } else if !had_hover {
            cx.unchanged();
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(drag))]
    fn pointer(&mut self, event: &PointerInput, drag: bool) {
        self.note_input();
        if !drag && event.button != Some(PointerButton::Primary) {
            return;
        }
        if drag && !event.buttons.contains(PointerButton::Primary) {
            return;
        }
        #[cfg(any(test, feature = "performance"))]
        let input_started = Instant::now();
        self.dismiss_completions();
        let position = self.geometry.borrow().hit(event.local_position);
        if let Some(position) = position {
            #[cfg(any(test, feature = "performance"))]
            {
                self.input_started = Some(input_started);
            }
            EditCommand::SetCaret {
                position,
                extend: drag || event.modifiers.shift,
            }
            .apply(&mut self.buffer);
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
        let mut reserved = cx.command(ReservedShortcut, |_, _, _| {});
        for shortcut in &self.reserved_shortcuts {
            reserved = reserved.shortcut(shortcut.clone());
        }
        let copy = cx
            .command(commands::Copy, |this, _, cx| {
                if let Err(error) = cx.write_clipboard_text(this.selected_text()) {
                    eprintln!("Velk clipboard: {error}");
                }
            })
            .enabled(self.editing.has_selection(&self.buffer));
        let copy = self.default_shortcut(copy, Shortcut::primary("c"));
        let cut = cx
            .command(commands::Cut, |this, _, cx| {
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
                match cx.write_clipboard_text(this.selected_text()) {
                    Ok(()) => {
                        this.composition = None;
                        this.execute(EditCommand::Delete);
                    }
                    Err(error) => eprintln!("Velk clipboard: {error}"),
                }
            })
            .enabled(self.editing.has_selection(&self.buffer) && self.composition.is_none());
        let cut = self.default_shortcut(cut, Shortcut::primary("x"));
        let undo = cx
            .command(commands::Undo, |this, _, _| {
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
                this.note_input();
                this.editing.clear_pairs();
                this.execute(EditCommand::Undo);
            })
            .enabled(self.buffer.can_undo() && self.composition.is_none());
        let undo = self.default_shortcut(undo, Shortcut::primary("z"));
        let redo = cx
            .command(commands::Redo, |this, _, _| {
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
                this.note_input();
                this.editing.clear_pairs();
                this.execute(EditCommand::Redo);
            })
            .enabled(self.buffer.can_redo() && self.composition.is_none());
        let redo = self.default_shortcut(redo, Shortcut::primary("z").shift());
        let redo = self.default_shortcut(redo, Shortcut::primary("y"));
        let select_all = cx
            .command(commands::SelectAll, |this, _, _| {
                #[cfg(any(test, feature = "performance"))]
                {
                    this.input_started = Some(Instant::now());
                }
                this.execute(EditCommand::SelectAll);
            })
            .enabled(self.composition.is_none());
        let select_all = self.default_shortcut(select_all, Shortcut::primary("a"));
        let mut surface = rxui::custom(viewport::ViewportSurface(Surface {
            rope: self.buffer.text(),
            revision: self.buffer.revision(),
            selection: self.buffer.selection(),
            caret: Some(self.caret_position()),
            selected_line_break: false,
            regions: self.selection_regions(),
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
                this.leave_hover(cx);
            } else {
                cx.unchanged();
            }
        }))
        .on_pointer_up(cx.listener(|_, event: &PointerInput, _| event.release_pointer()))
        .on_text_insert(cx.listener(|this, event: &TextInsertInput, _| {
            this.committed_text(&event.text, event.source)
        }))
        .on_ime(cx.listener(|this, event: &ImeInput, _| {
            if this.editing.input_state().capture_text {
                return;
            }
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
        .on_command(reserved.clone());
        // RXUI resolves commands by type, so each declared shortcut has its own slot.
        macro_rules! bind_keymap { ($($slot:literal),*) => { $(surface = self.bind_keymap_shortcut::<$slot>(surface, cx);)* }; }
        bind_keymap!(0, 1, 2, 3, 4, 5, 6, 7);
        // Paste uses RXUI's default native clipboard handler, delivered through
        // on_text_insert with TextInsertSource::Paste.
        // A container root keeps native input and preparation on the same
        // path as RXUI's built-in editor examples.
        let mut root = column()
            .fill_width()
            .fill_height()
            .on_command(reserved)
            .on_command(copy)
            .on_command(cut)
            .on_command(undo)
            .on_command(redo)
            .on_command(select_all)
            .child(surface);
        if let Some((anchor, card)) = self.hover_card() {
            root = root.child(
                popover(
                    anchor,
                    column()
                        .gap(6.)
                        .padding(12.)
                        .on_hover(cx.listener(|this, event: &HoverEvent, cx| {
                            this.card_hover(event.hovered, cx)
                        }))
                        .on_pointer_move(cx.listener(|this, event: &PointerInput, cx| {
                            if event.buttons.contains(PointerButton::Primary) {
                                this.card_hover(true, cx);
                            } else {
                                cx.unchanged();
                            }
                        }))
                        .on_pointer_up(cx.listener(|this, event: &PointerInput, cx| {
                            this.card_hover(event.bounds.contains(event.position), cx)
                        }))
                        .child(
                            label(card.title.clone())
                                .font_size(14.)
                                .color(ThemeColor::Text),
                        )
                        .child(
                            scroll_area(
                                self.hover_body(&card)
                                    .selectable(true)
                                    .paint_style(
                                        PaintStyle::new()
                                            .focus_color(ThemeColor::Focus)
                                            .focus_width(1.),
                                    )
                                    .font_size(13.)
                                    .color(ThemeColor::Text)
                                    .fill_width(),
                            )
                            .handle(self.hover.scroll.clone())
                            .scrollbar_style(ScrollbarStyle::platform())
                            .fill_width()
                            .into_element()
                            .key("editor-hover-body")
                            .layout(|style| {
                                style.flex_grow = 0.;
                                style.flex_basis = rxui::taffy::prelude::auto();
                            })
                            .max_height(
                                (self.geometry.borrow().viewport[1] - 100.).clamp(40., 360.),
                            ),
                        ),
                )
                .key("editor-hover")
                .gap(0.)
                .width((self.geometry.borrow().viewport[0] - 24.).clamp(80., 520.))
                .padding(0.)
                .autofocus(false)
                .transition(rxui::Transition::None)
                .on_dismiss(cx.listener(|this, _: &DismissEvent, _| this.hide_hover()))
                .into_element()
                .pointer_events(PointerEvents::Auto),
            );
        }
        if let Some(popup) = self.snippet_choices_popup(cx) {
            root = root.child(popup);
        }
        if let Some(popup) = self.signature_popup(cx) {
            root = root.child(popup);
        }
        if let Some(popup) = self.completion_popup(cx) {
            root = root.child(popup);
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

    #[derive(Debug)]
    struct Vim;
    impl velk_editing::Provider for Vim {
        fn evaluate(
            &self,
            request: &velk_editing::Request,
        ) -> Result<velk_editing::Decision, String> {
            <velk_plugin_vim::Vim as velk_plugin_sdk::EditingBehavior>::evaluate(request)
        }
    }
    #[test]
    fn visual_modes_paint_inclusive_ranges_and_empty_lines_with_an_independent_caret() {
        let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
        for source in ["abc\n\nthird", "e\u{301}界z\n\nlast"] {
            let mut editor = Editor::new(source);
            editor
                .set_appearance(EditorAppearance {
                    selection: [1., 0., 0., 1.],
                    ..Default::default()
                })
                .unwrap();
            editor.set_editing_providers(std::collections::BTreeMap::from([(
                velk_editing::Role::Keymap,
                Arc::new(Vim) as Arc<dyn velk_editing::Provider>,
            )]));
            let mut test = TestUi::new(&graphics, [640, 240], editor).unwrap();
            test.painter_mut().fonts_mut().load_system_fonts();
            let id = test.element("editor-surface").unwrap().unwrap().id;
            test.parts().1.focus(id);
            test.render().unwrap();
            let initial = test.read(|e| e.geometry.borrow().caret.unwrap());
            test.text_input(TextInputEvent::Insert("v".into())).unwrap();
            let first = test.render().unwrap();
            let red = |pixels: &[u8]| {
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|pixel| pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40)
                    .count()
            };
            assert!(red(&first) > 0, "Visual selection must paint");
            test.read(|e| {
                assert_eq!(e.caret_position(), 0);
                assert_eq!(
                    e.buffer.selection().range().end,
                    e.buffer.next_grapheme_boundary(0)
                );
                assert_eq!(e.geometry.borrow().caret.unwrap(), initial);
            });
            let entity = test.entity().clone();
            test.runtime().update(|cx| {
                entity.update(cx, |editor, _| {
                    editor.set_semantic_hover(true);
                    editor.request_hover_at_caret();
                    assert_eq!(
                        editor.hover_request().unwrap().position,
                        editor.caret_position()
                    );
                })
            });
            test.text_input(TextInputEvent::Insert("l".into())).unwrap();
            let second = test.render().unwrap();
            assert!(red(&second) > red(&first));
            let caret = test.read(|e| e.geometry.borrow().caret.unwrap());
            test.key(KeyEvent {
                key: KeyboardKey::Escape,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            })
            .unwrap();
            let normal = test.render().unwrap();
            assert_eq!(red(&normal), 0);
            test.read(|e| assert_eq!(e.geometry.borrow().caret.unwrap(), caret));
            for key in ["V", "j"] {
                test.text_input(TextInputEvent::Insert(key.into())).unwrap();
            }
            let lines = test.render().unwrap();
            let blank = test.read(|e| {
                assert_eq!(e.input_mode(), Some("V-LINE"));
                let expected = e.buffer.text().line_to_char(1);
                assert_eq!(e.caret_position(), expected);
                assert_eq!(e.buffer.selection().head, e.buffer.text().line_to_char(2));
                e.geometry.borrow().caret_area()
            });
            let row = (blank.y + blank.height / 2.) as usize;
            assert!(
                red(&lines[row * 640 * 4..(row + 1) * 640 * 4]) > 0,
                "selected empty line must paint its line break"
            );
            test.text_input(TextInputEvent::Paste("replacement".into()))
                .unwrap();
            let pasted = test.render().unwrap();
            assert_eq!(red(&pasted), 0);
            test.read(|e| {
                assert_eq!(e.input_mode(), Some("NORMAL"));
                assert!(e.buffer.selection().is_empty());
                assert!(e.caret_position() <= e.buffer.text().len_chars());
            });
        }
    }
    #[test]
    fn visual_block_paints_every_row_caret_and_routes_multi_row_typing() {
        let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
        let mut editor = Editor::new("\n\n");
        editor
            .set_appearance(EditorAppearance {
                foreground: [0., 1., 0., 1.],
                selection: [1., 0., 0., 1.],
                ..Default::default()
            })
            .unwrap();
        editor.set_editing_providers(std::collections::BTreeMap::from([(
            velk_editing::Role::Keymap,
            Arc::new(Vim) as Arc<dyn velk_editing::Provider>,
        )]));
        let mut test = TestUi::new(&graphics, [640, 240], editor).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        let id = test.element("editor-surface").unwrap().unwrap().id;
        test.parts().1.focus(id);
        let block = || KeyEvent {
            key: KeyboardKey::Character("q".into()),
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                control: true,
                ..Default::default()
            },
        };
        test.key(block()).unwrap();
        for key in ["2", "j"] {
            test.text_input(TextInputEvent::Insert(key.into())).unwrap();
        }
        let pixels = test.render().unwrap();
        let rows = test.read(|e| {
            assert_eq!(e.input_mode(), Some("V-BLOCK"));
            assert_eq!(e.selection_regions().unwrap().len(), 3);
            let geometry = e.geometry.borrow();
            geometry
                .lines
                .iter()
                .map(|line| {
                    (geometry.origin[1] + crate::surface::PADDING + line.top - geometry.offset[1]
                        + line.height / 2.) as usize
                })
                .collect::<Vec<_>>()
        });
        let green = |pixels: &[u8], row: usize| {
            pixels[row * 640 * 4..(row + 1) * 640 * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[1] > 200 && pixel[0] < 40 && pixel[2] < 40)
                .count()
        };
        for row in &rows {
            assert!(
                green(&pixels, *row) > 0,
                "each block row must paint a caret"
            );
        }
        test.key(block()).unwrap();
        let single = test.render().unwrap();
        assert_eq!(green(&single, rows[0]), 0);
        assert_eq!(green(&single, rows[1]), 0);
        assert!(green(&single, rows[2]) > 0);
        test.key(block()).unwrap();
        for key in ["2", "k", "I", "Z"] {
            test.text_input(TextInputEvent::Insert(key.into())).unwrap();
        }
        test.render().unwrap();
        test.read(|e| {
            assert_eq!(e.buffer.text().to_string(), "Z\nZ\nZ");
            assert_eq!(e.selection_regions().unwrap().len(), 3);
        });
        test.key(KeyEvent {
            key: KeyboardKey::Backspace,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        })
        .unwrap();
        test.render().unwrap();
        test.read(|e| assert_eq!(e.buffer.text().to_string(), "\n\n"));
        test.text_input(TextInputEvent::Paste("a\nb\nc".into()))
            .unwrap();
        test.render().unwrap();
        test.read(|e| assert_eq!(e.buffer.text().to_string(), "a\nb\nc"));
        test.key(KeyEvent {
            key: KeyboardKey::Escape,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        })
        .unwrap();
        test.render().unwrap();
        test.read(|e| {
            assert!(e.selection_regions().is_none());
            assert_eq!(e.input_mode(), Some("NORMAL"));
        });
    }
    #[test]
    fn hover_remains_painted_during_text_selection() {
        let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
        let mut test = TestUi::new(&graphics, [640, 240], Editor::new("let value = 42;")).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        test.render().unwrap();
        let entity = test.entity().clone();
        test.runtime().update(|cx| {
            entity.update(cx, |e, _| {
                e.set_semantic_hover(true);
                e.request_hover_at_caret();
                let target = e.hover_request().unwrap();
                assert!(e.resolve_formatted_hover(
                    target,
                    None,
                    "Rust".into(),
                    Arc::new(velk_markup::Document {
                        runs: vec![
                            velk_markup::Run {
                                text: "let value: usize\n\n".into(),
                                style: velk_markup::Style {
                                    code: true,
                                    ..Default::default()
                                }
                            },
                            velk_markup::Run {
                                text: "Variable".into(),
                                style: velk_markup::Style {
                                    bold: true,
                                    ..Default::default()
                                }
                            },
                            velk_markup::Run {
                                text: " documentation".into(),
                                style: Default::default()
                            },
                        ],
                    }),
                ));
            })
        });
        let before = test.render().unwrap();
        let bounds = test.element("editor-hover-body").unwrap().unwrap().bounds;
        let from = [bounds.x + 4., bounds.y + 8.];
        let to = [bounds.x + 90., bounds.y + 8.];
        test.pointer(rxui::PointerEvent::Moved(from)).unwrap();
        test.pointer(rxui::PointerEvent::Down {
            position: from,
            button: PointerButton::Primary,
            modifiers: Default::default(),
        })
        .unwrap();
        test.pointer(rxui::PointerEvent::Moved(to)).unwrap();
        test.pointer(rxui::PointerEvent::Up {
            position: to,
            button: PointerButton::Primary,
            modifiers: Default::default(),
        })
        .unwrap();
        let after = test.render().unwrap();
        assert!(test.read(|e| e.hover_card().is_some()));
        // Background below the body is unaffected by selection and must stay opaque.
        let x = (bounds.x + bounds.width / 2.) as usize;
        let y = (bounds.y + bounds.height + 4.) as usize;
        let pixel = (y * 640 + x) * 4;
        assert!(before[pixel + 3] > 0);
        assert_eq!(&before[pixel..pixel + 4], &after[pixel..pixel + 4]);
        let (_, ui, _) = test.parts();
        assert!(
            ui.elements().any(|e| e
                .editing
                .is_some_and(|s| s.selection.anchor != s.selection.focus)),
            "popup body should have a text selection"
        );
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

#[cfg(test)]
mod interactive_tests {
    use super::*;
    #[derive(Debug)]
    struct Provider;
    impl velk_editing::Provider for Provider {
        fn evaluate(
            &self,
            request: &velk_editing::Request,
        ) -> Result<velk_editing::Decision, String> {
            use velk_editing::*;
            Ok(Decision::Apply(Proposal {
                revision: request.revision,
                edits: vec![Edit {
                    start: request.selection.range().start,
                    end: request.selection.range().end,
                    text: "()".into(),
                }],
                selection: Some(Selection::caret(request.selection.range().start + 1)),
                pairs: vec![],
            }))
        }
    }
    #[derive(Debug)]
    struct Failing;
    impl velk_editing::Provider for Failing {
        fn evaluate(&self, _: &velk_editing::Request) -> Result<velk_editing::Decision, String> {
            Err("provider failure".into())
        }
    }
    #[test]
    fn keyboard_typing_and_deletion_group_but_paste_ime_and_motion_are_boundaries() {
        let mut editor = Editor::new("");
        for text in ["a", "b", "c"] {
            editor.committed_text(text, rxui::TextInsertSource::Keyboard);
        }
        editor.execute(EditCommand::Undo);
        assert_eq!(editor.buffer().text().to_string(), "");
        editor.execute(EditCommand::Redo);
        for _ in 0..2 {
            assert!(!editor.keymap("Backspace", false, false, false, false));
            editor.execute(EditCommand::Backspace);
        }
        assert_eq!(editor.buffer().text().to_string(), "a");
        editor.execute(EditCommand::Undo);
        assert_eq!(editor.buffer().text().to_string(), "abc");
        for source in [rxui::TextInsertSource::Paste, rxui::TextInsertSource::Ime] {
            editor.committed_text("X", source);
            editor.committed_text("Y", rxui::TextInsertSource::Keyboard);
            editor.execute(EditCommand::Undo);
            assert_eq!(editor.buffer().text().to_string(), "abcX");
            editor.execute(EditCommand::Undo);
            assert_eq!(editor.buffer().text().to_string(), "abc");
        }
        editor.committed_text("d", rxui::TextInsertSource::Keyboard);
        editor.execute(EditCommand::Horizontal {
            right: false,
            extend: false,
        });
        editor.committed_text("e", rxui::TextInsertSource::Keyboard);
        editor.execute(EditCommand::Undo);
        assert_eq!(editor.buffer().text().to_string(), "abcd");
    }
    #[test]
    fn failed_or_oversized_indentation_never_replaces_selected_text() {
        for text in ["selected".into(), "x".repeat(velk_editing::MAX_TEXT + 1)] {
            let mut editor = Editor::new(&text);
            editor.execute(EditCommand::SelectAll);
            editor.set_editing_providers(std::collections::BTreeMap::from([(
                velk_editing::Role::Indentation,
                Arc::new(Failing) as Arc<dyn velk_editing::Provider>,
            )]));
            assert!(editor.interactive(velk_editing::Intent::Indent));
            assert!(editor.interactive(velk_editing::Intent::Outdent));
            assert_eq!(editor.buffer().text().to_string(), text);
            assert!(!editor.buffer().can_undo());
        }
    }
    #[test]
    fn typing_uses_provider_but_paste_and_ime_keep_literal_text_and_undo() {
        for source in [
            rxui::TextInsertSource::Keyboard,
            rxui::TextInsertSource::Paste,
            rxui::TextInsertSource::Ime,
        ] {
            let mut editor = Editor::new("");
            editor.set_editing_providers(std::collections::BTreeMap::from([(
                velk_editing::Role::Pairing,
                Arc::new(Provider) as Arc<dyn velk_editing::Provider>,
            )]));
            editor.committed_text("(", source);
            assert_eq!(
                editor.buffer().text().to_string(),
                if source == rxui::TextInsertSource::Keyboard {
                    "()"
                } else {
                    "("
                }
            );
            editor.execute(EditCommand::Undo);
            assert_eq!(editor.buffer().text().to_string(), "");
            assert!(!editor.buffer().can_undo());
        }
        let mut plain = Editor::new("");
        plain.committed_text("(", rxui::TextInsertSource::Keyboard);
        assert_eq!(plain.buffer().text().to_string(), "(");
    }
}

#[cfg(test)]
mod syntax_service_tests {
    use super::*;
    use velk_syntax::{Edit, Highlight, Provider, Session};
    #[derive(Debug)]
    struct SyntaxProvider;
    impl Provider for SyntaxProvider {
        fn open(&self, _: Language) -> Result<Box<dyn Session>, String> {
            Ok(Box::new(SyntaxSession))
        }
    }
    struct SyntaxSession;
    impl Session for SyntaxSession {
        fn update(
            &mut self,
            _: u64,
            _: ropey::Rope,
            _: &[Edit],
            _: &dyn Fn() -> bool,
        ) -> Result<bool, String> {
            Ok(true)
        }
        fn highlights(
            &mut self,
            _: std::ops::Range<usize>,
            _: &dyn Fn() -> bool,
        ) -> Result<Option<Vec<Highlight>>, String> {
            Ok(Some(vec![]))
        }
    }
    #[test]
    fn language_without_a_provider_stays_plain_and_provider_replacement_clears_cached_results() {
        let mut editor = Editor::new("fn main() {}");
        editor.set_language(Some(Language::new("rust"))).unwrap();
        assert!(editor.syntax_worker.is_none());
        let provider = Arc::new(SyntaxProvider) as Arc<dyn Provider>;
        editor.set_syntax_provider(Some(provider.clone())).unwrap();
        let worker = editor.syntax_worker.as_ref().unwrap().clone();
        worker.request_viewport(0, std::iter::once(0..12).collect());
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let snapshot = loop {
            if let Ok(result) = worker.updates.try_recv() {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        editor.syntax_snapshot = Some(snapshot.clone());
        editor.set_syntax_provider(Some(provider)).unwrap();
        assert_eq!(editor.syntax_worker.as_ref().unwrap().id, worker.id);
        assert!(Arc::ptr_eq(
            editor.syntax_snapshot.as_ref().unwrap(),
            &snapshot
        ));
        editor
            .set_syntax_provider(Some(Arc::new(SyntaxProvider)))
            .unwrap();
        assert_ne!(editor.syntax_worker.as_ref().unwrap().id, worker.id);
        assert!(editor.syntax_snapshot.is_none());
        editor.set_syntax_provider(None).unwrap();
        assert!(editor.syntax_worker.is_none());
        assert!(editor.syntax_demand.is_none());
        assert!(editor.syntax_snapshot.is_none());
        assert!(!editor.buffer().is_dirty());
    }
}
