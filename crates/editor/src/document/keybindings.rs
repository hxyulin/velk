//! User bindings intercept editor input before default keymaps and reuse command actions.
use super::{Document, palette::Entry};
use rxui::{Element, prelude::*};
use std::{
    cell::RefCell,
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};
use velk_commands::keybindings::{
    Binding, Bindings, Context as BindingContext, Match, Scope, Stroke,
};

const TEMPLATE: &str = "{\n  \"bindings\": []\n}\n";
const CHORD_TIMEOUT: Duration = Duration::from_secs(1);
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    target: rxui::ElementId,
    scope: Scope,
    buffer: Option<(u64, u64)>,
    language: Option<String>,
    mode: Option<String>,
}
struct Pending {
    strokes: Vec<Stroke>,
    source: Source,
    deadline: Instant,
}
pub(super) struct State {
    pub entries: RefCell<Vec<Entry>>,
    pub bindings: Bindings,
    path: Option<PathBuf>,
    hash: Option<blake3::Hash>,
    initialized: bool,
    pending: Option<Pending>,
    timer: Option<Task>,
    loading: Option<Task>,
    opening: Option<Task>,
    error: Option<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            entries: Default::default(),
            bindings: Default::default(),
            path: std::env::var_os("VELK_KEYBINDINGS_PATH")
                .map(PathBuf::from)
                .or_else(|| {
                    crate::settings::settings_path().map(|p| p.with_file_name("keybindings.json"))
                }),
            hash: None,
            initialized: false,
            pending: None,
            timer: None,
            loading: None,
            opening: None,
            error: None,
        }
    }
}
pub(super) struct ReserveWindowKeys;
impl Command for ReserveWindowKeys {}
pub(super) struct Open;
impl Command for Open {}
pub(super) struct Reload;
impl Command for Reload {}
pub(super) struct ShowCommands;
impl Command for ShowCommands {}

fn stroke(event: &KeyEvent) -> Option<Stroke> {
    let key = match &event.key {
        KeyboardKey::Character(text) => text.clone(),
        KeyboardKey::Space => "Space".into(),
        KeyboardKey::Enter => "Enter".into(),
        KeyboardKey::Escape => "Escape".into(),
        KeyboardKey::Tab => "Tab".into(),
        KeyboardKey::Backspace => "Backspace".into(),
        KeyboardKey::Delete => "Delete".into(),
        KeyboardKey::ArrowLeft => "Left".into(),
        KeyboardKey::ArrowRight => "Right".into(),
        KeyboardKey::ArrowUp => "Up".into(),
        KeyboardKey::ArrowDown => "Down".into(),
        KeyboardKey::Home => "Home".into(),
        KeyboardKey::End => "End".into(),
        KeyboardKey::Other(name) => name
            .strip_prefix("Named(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(name)
            .to_owned(),
        _ => return None,
    };
    Some(Stroke {
        key: velk_commands::keybindings::normalize_key(&key).ok()?,
        control: event.modifiers.control,
        meta: event.modifiers.meta,
        alt: event.modifiers.alt,
        shift: event.modifiers.shift,
    })
}
impl Document {
    pub(super) fn cancel_key_chord(&mut self) {
        self.keybindings.pending = None;
        self.keybindings.timer = None;
    }
    fn binding_modal(&self) -> bool {
        self.palette.open
            || self.shell.open
            || self.plugin_menu_open
            || self.command_result.is_some()
            || self.lifecycle.close_intent().is_some()
            || self.reloading()
    }
    pub(super) fn bound_key(&mut self, event: &KeyInput, scope: Scope, cx: &mut Context<'_, Self>) {
        if !event.event.pressed || self.keybindings.bindings.0.is_empty() {
            cx.unchanged();
            return;
        }
        if self.binding_modal()
            || self
                .buffer
                .as_ref()
                .is_some_and(|b| b.editor.read(cx).has_composition())
        {
            self.cancel_key_chord();
            return;
        }
        let Some(stroke) = stroke(&event.event) else {
            cx.unchanged();
            return;
        };
        let context = self.command_context(cx);
        let source = Source {
            target: event.target,
            scope,
            buffer: self
                .buffer
                .as_ref()
                .map(|b| (b.id, b.editor.read(cx).buffer().revision())),
            language: context.language,
            mode: self
                .buffer
                .as_ref()
                .map(|b| b.editor.read(cx).input_mode_id().to_owned()),
        };
        let context = BindingContext {
            scope,
            language: source.language.as_deref(),
            mode: source.mode.as_deref(),
        };
        // A window capture precedes editor capture. Leave an editor-owned chord
        // intact while it travels through its ancestor on the next key press.
        if scope == Scope::Window
            && self.keybindings.pending.as_ref().is_some_and(|p| {
                p.source.scope == Scope::Editor
                    && p.source.target == event.target
                    && p.deadline > Instant::now()
            })
        {
            return;
        }
        if event.event.repeat
            && self.keybindings.pending.as_ref().is_some_and(|pending| {
                pending.source == source && pending.deadline > Instant::now()
            })
        {
            event.prevent_default();
            event.stop_propagation();
            return;
        }
        let had_pending = self.keybindings.pending.is_some();
        let mut keys = match self.keybindings.pending.take() {
            Some(pending) if pending.source == source && pending.deadline > Instant::now() => {
                pending.strokes
            }
            _ => Vec::new(),
        };
        if !keys.is_empty() && stroke.key == "escape" {
            self.cancel_key_chord();
            event.prevent_default();
            event.stop_propagation();
            return;
        }
        keys.push(stroke.clone());
        let mut matched = self.keybindings.bindings.resolve(&keys, &context);
        if matched == Match::None && keys.len() > 1 {
            keys = vec![stroke];
            matched = self.keybindings.bindings.resolve(&keys, &context);
        }
        match matched {
            Match::None => {
                self.cancel_key_chord();
                if !had_pending {
                    cx.unchanged();
                }
            }
            Match::Prefix => {
                self.keybindings.pending = Some(Pending {
                    strokes: keys,
                    source,
                    deadline: Instant::now() + CHORD_TIMEOUT,
                });
                self.keybindings.timer = None;
                if let Ok(timer) = cx.try_spawn(rxui::sleep(CHORD_TIMEOUT), |this, _, _| {
                    this.cancel_key_chord()
                }) {
                    self.keybindings.timer = Some(timer);
                }
                event.prevent_default();
                event.stop_propagation();
            }
            Match::Complete(binding) => {
                self.cancel_key_chord();
                event.prevent_default();
                event.stop_propagation();
                if !event.event.repeat || binding.repeat {
                    self.execute_binding(binding, cx);
                }
            }
        }
    }
    fn execute_binding(&mut self, binding: Binding, cx: &mut Context<'_, Self>) {
        let Some(id) = binding.command else {
            return;
        };
        let entries: Vec<_> = self
            .keybindings
            .entries
            .borrow()
            .iter()
            .filter(|entry| entry.matches_id(&id))
            .cloned()
            .collect();
        match entries.as_slice() {
            [entry] if entry.enabled => self.execute_bound_entry(entry, binding.input, cx),
            [_] => self.notice(Some(format!(
                "Command is unavailable in this context: {id}"
            ))),
            [] => self.notice(Some(format!("Unknown or unloaded command: {id}"))),
            _ => self.notice(Some(format!(
                "Ambiguous command ID; use plugin-id:command-id: {id}"
            ))),
        }
    }
    pub(super) fn decorate_binding_entries(
        &self,
        entries: &mut [Entry],
        cx: &impl rxui::ReadContext,
    ) {
        let language = self.command_context(cx).language;
        let mode = self
            .buffer
            .as_ref()
            .map(|b| b.editor.read(cx).input_mode_id().to_owned());
        let context = |scope| BindingContext {
            scope,
            language: language.as_deref(),
            mode: mode.as_deref(),
        };
        for entry in entries {
            if !entry.shortcut.is_empty() {
                let portable = entry.shortcut.replace('⌘', "Primary+").replace('⌥', "Alt+");
                if let Ok(default) = Stroke::parse(&portable) {
                    let matched = self
                        .keybindings
                        .bindings
                        .resolve(std::slice::from_ref(&default), &context(Scope::Window));
                    let matched = if matched == Match::None {
                        self.keybindings
                            .bindings
                            .resolve(&[default], &context(Scope::Editor))
                    } else {
                        matched
                    };
                    if matched != Match::None {
                        entry.shortcut.clear();
                    }
                }
            }
            let mut keys = Vec::new();
            for compiled in &self.keybindings.bindings.0 {
                let binding = &compiled.binding;
                if !binding
                    .command
                    .as_ref()
                    .is_some_and(|id| entry.matches_id(id))
                {
                    continue;
                }
                if self
                    .keybindings
                    .bindings
                    .resolve(&compiled.strokes, &context(binding.when.scope))
                    != Match::Complete(binding.clone())
                {
                    continue;
                }
                if binding.when.scope == Scope::Editor
                    && self
                        .keybindings
                        .bindings
                        .resolve(&compiled.strokes, &context(Scope::Window))
                        != Match::None
                {
                    continue;
                }
                if !keys.contains(&binding.keys) {
                    keys.push(binding.keys.clone());
                }
            }
            for key in keys {
                if !entry.shortcut.is_empty() {
                    entry.shortcut.push_str(", ");
                }
                entry.shortcut.push_str(&key);
            }
        }
    }
    fn reserved_keybindings(
        &self,
        window_only: bool,
        cx: &impl rxui::ReadContext,
    ) -> Vec<Shortcut> {
        if self.binding_modal()
            || self
                .buffer
                .as_ref()
                .is_some_and(|b| b.editor.read(cx).has_composition())
        {
            return Vec::new();
        }
        let language = self.command_context(cx).language;
        let mode = self
            .buffer
            .as_ref()
            .map(|b| b.editor.read(cx).input_mode_id().to_owned());
        let mut keys = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for compiled in &self.keybindings.bindings.0 {
            if window_only && compiled.binding.when.scope != Scope::Window {
                continue;
            }
            let context = BindingContext {
                scope: compiled.binding.when.scope,
                language: language.as_deref(),
                mode: mode.as_deref(),
            };
            if !compiled.binding.when.matches(&context) {
                continue;
            }
            let stroke = &compiled.strokes[0];
            if !seen.insert(stroke) {
                continue;
            }
            let key = match stroke.key.as_str() {
                "space" => KeyboardKey::Space,
                "enter" => KeyboardKey::Enter,
                "escape" => KeyboardKey::Escape,
                "tab" => KeyboardKey::Tab,
                "backspace" => KeyboardKey::Backspace,
                "delete" => KeyboardKey::Delete,
                "left" => KeyboardKey::ArrowLeft,
                "right" => KeyboardKey::ArrowRight,
                "up" => KeyboardKey::ArrowUp,
                "down" => KeyboardKey::ArrowDown,
                "home" => KeyboardKey::Home,
                "end" => KeyboardKey::End,
                key if key.starts_with('f') && key.len() > 1 => {
                    KeyboardKey::Other(key.to_uppercase())
                }
                "pageup" => KeyboardKey::Other("Named(PageUp)".into()),
                "pagedown" => KeyboardKey::Other("Named(PageDown)".into()),
                key => KeyboardKey::Character(key.into()),
            };
            let shortcut = Shortcut::new(
                key,
                rxui::Modifiers {
                    control: stroke.control,
                    meta: stroke.meta,
                    alt: stroke.alt,
                    shift: stroke.shift,
                },
            );
            keys.push(shortcut);
        }
        keys
    }
    pub(super) fn window_key_reservation(
        &self,
        cx: &ViewContext<'_, Self>,
    ) -> CommandAction<ReserveWindowKeys> {
        let mut action = cx.command(ReserveWindowKeys, |_, _, _| {});
        for shortcut in self.reserved_keybindings(true, cx) {
            action = action.shortcut(shortcut);
        }
        action
    }
    pub(super) fn sync_buffer_keybindings(&mut self, cx: &mut Context<'_, Self>) -> bool {
        let shortcuts = self.reserved_keybindings(false, cx);
        if let Some(buffer) = self.buffer.as_ref() {
            buffer.editor.update(cx, |editor, cx| {
                let changed = editor.set_reserved_shortcuts(shortcuts);
                if !changed {
                    cx.unchanged();
                }
                changed
            })
        } else {
            false
        }
    }
    fn sync_all_keybindings(&mut self, cx: &mut Context<'_, Self>) {
        for id in self.ids() {
            self.with_buffer(id, cx, |this, cx| {
                this.sync_buffer_keybindings(cx);
            });
        }
    }
    pub(super) fn keybinding_actions(
        &self,
        cx: &ViewContext<'_, Self>,
    ) -> (
        CommandAction<Open>,
        CommandAction<Reload>,
        CommandAction<ShowCommands>,
    ) {
        (
            cx.command(Open, |this, _, cx| this.open_keybindings(cx))
                .label("Keybindings: Open configuration"),
            cx.command(Reload, |this, _, cx| this.reload_keybindings(cx))
                .label("Keybindings: Reload configuration"),
            cx.command(ShowCommands, |this, _, cx| this.show_binding_commands(cx))
                .label("Keybindings: Show available commands"),
        )
    }
    fn show_binding_commands(&mut self, cx: &mut Context<'_, Self>) {
        let text = {
            let entries = self.keybindings.entries.borrow();
            let mut text = String::from(
                "Available command IDs\n\nUse the exact ID in keybindings.json. Plugin IDs can be shortened to the command ID when unique.\n\n",
            );
            for entry in entries.iter() {
                text.push_str(&format!(
                    "{}\n  {}{}{}\n  Keys: {}\n",
                    entry.id,
                    entry.label,
                    entry
                        .plugin_id()
                        .map_or(String::new(), |id| format!(" [command: {id}]")),
                    if entry.enabled {
                        ""
                    } else {
                        " (currently unavailable)"
                    },
                    if entry.shortcut.is_empty() {
                        "No default or user binding"
                    } else {
                        &entry.shortcut
                    },
                ));
            }
            text
        };
        if let Err(error) = self.host_request(
            velk_plugin_contracts::host::Request::NewBuffer { text },
            None,
            cx,
        ) {
            self.notice(Some(error));
        }
    }
    fn open_keybindings(&mut self, cx: &mut Context<'_, Self>) {
        if self.keybindings.opening.is_some() {
            return;
        }
        let Some(path) = self.keybindings.path.clone() else {
            self.notice(Some("Keybinding file location unavailable".into()));
            return;
        };
        match cx.try_spawn_blocking(
            move || {
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."));
                std::fs::create_dir_all(parent)?;
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut file) => {
                        use std::io::Write;
                        file.write_all(TEMPLATE.as_bytes())?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                path.canonicalize()
            },
            |this, result, cx| {
                this.keybindings.opening = None;
                match result {
                    Ok(Ok(path)) => this.open_path(path, cx),
                    Ok(Err(error)) => this.notice(Some(error.to_string())),
                    Err(error) => this.notice(Some(error.to_string())),
                }
            },
        ) {
            Ok(task) => self.keybindings.opening = Some(task),
            Err(error) => self.notice(Some(error.to_string())),
        }
    }
    pub(super) fn reload_keybindings(&mut self, cx: &mut Context<'_, Self>) {
        self.load_keybindings(true, cx);
    }
    pub(super) fn refresh_keybindings(&mut self, cx: &mut Context<'_, Self>) {
        self.load_keybindings(false, cx);
    }
    fn load_keybindings(&mut self, force: bool, cx: &mut Context<'_, Self>) {
        if self.keybindings.loading.is_some() {
            return;
        }
        let Some(path) = self.keybindings.path.clone() else {
            return;
        };
        let previous = self.keybindings.hash;
        let initialized = self.keybindings.initialized;
        match cx.try_spawn_blocking(
            move || {
                let bytes = match std::fs::File::open(path) {
                    Ok(file) => {
                        let mut bytes = Vec::new();
                        file.take(velk_commands::keybindings::MAX_BYTES as u64 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|e| e.to_string())?;
                        Some(bytes)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.to_string()),
                };
                let hash = bytes.as_ref().map(|b| blake3::hash(b));
                if !force && initialized && hash == previous {
                    return Ok(None);
                }
                let bindings =
                    bytes.map_or_else(|| Ok(Bindings::default()), |bytes| Bindings::parse(&bytes));
                Ok(Some((hash, bindings)))
            },
            |this, result, cx| {
                this.keybindings.loading = None;
                match result {
                    Ok(Ok(None)) => cx.unchanged(),
                    Ok(Ok(Some((hash, bindings)))) => {
                        this.cancel_key_chord();
                        this.keybindings.hash = hash;
                        this.keybindings.initialized = true;
                        match bindings {
                            Ok(bindings) => {
                                this.keybindings.bindings = bindings;
                                this.keybindings.error = None;
                                this.sync_all_keybindings(cx);
                            }
                            Err(error) => this.keybindings.error = Some(error),
                        }
                    }
                    Ok(Err(error)) => this.keybindings.error = Some(error),
                    Err(error) => this.keybindings.error = Some(error.to_string()),
                }
            },
        ) {
            Ok(task) => self.keybindings.loading = Some(task),
            Err(error) => self.keybindings.error = Some(error.to_string()),
        }
    }
    pub(super) fn keybinding_notice(&self) -> Option<Element> {
        if let Some(error) = &self.keybindings.error {
            return Some(
                label(format!(
                    "Keybindings: {error}. Previous valid bindings remain active."
                ))
                .key("keybindings-error")
                .font_size(12.)
                .padding(8.)
                .into_element(),
            );
        }
        self.keybindings.pending.as_ref().map(|pending| {
            label(format!(
                "Key chord: {} … (Escape to cancel)",
                pending
                    .strokes
                    .iter()
                    .map(|s| s.key.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
            .key("key-chord")
            .font_size(12.)
            .padding(6.)
            .into_element()
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum Edit {
    Undo,
    Redo,
    SelectAll,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
    Indent,
    Outdent,
    Newline,
    InsertText,
    Copy,
    Cut,
    Paste,
}
impl Edit {
    fn all() -> Vec<Self> {
        use Edit::*;
        vec![
            Undo,
            Redo,
            SelectAll,
            Backspace,
            Delete,
            Left,
            Right,
            Up,
            Down,
            SelectLeft,
            SelectRight,
            SelectUp,
            SelectDown,
            LineStart,
            LineEnd,
            DocumentStart,
            DocumentEnd,
            Indent,
            Outdent,
            Newline,
            InsertText,
            Copy,
            Cut,
            Paste,
        ]
    }
    pub fn id(self) -> &'static str {
        use Edit::*;
        match self {
            Undo => "velk.edit.undo",
            Redo => "velk.edit.redo",
            SelectAll => "velk.edit.select-all",
            Backspace => "velk.edit.backspace",
            Delete => "velk.edit.delete",
            Left => "velk.edit.left",
            Right => "velk.edit.right",
            Up => "velk.edit.up",
            Down => "velk.edit.down",
            SelectLeft => "velk.edit.select-left",
            SelectRight => "velk.edit.select-right",
            SelectUp => "velk.edit.select-up",
            SelectDown => "velk.edit.select-down",
            LineStart => "velk.edit.line-start",
            LineEnd => "velk.edit.line-end",
            DocumentStart => "velk.edit.document-start",
            DocumentEnd => "velk.edit.document-end",
            Indent => "velk.edit.indent",
            Outdent => "velk.edit.outdent",
            Newline => "velk.edit.newline",
            InsertText => "velk.edit.insert-text",
            Copy => "velk.edit.copy",
            Cut => "velk.edit.cut",
            Paste => "velk.edit.paste",
        }
    }
    pub fn title(self) -> &'static str {
        use Edit::*;
        match self {
            Undo => "Edit: Undo",
            Redo => "Edit: Redo",
            SelectAll => "Edit: Select all",
            Backspace => "Edit: Backspace",
            Delete => "Edit: Delete",
            Left => "Edit: Move left",
            Right => "Edit: Move right",
            Up => "Edit: Move up",
            Down => "Edit: Move down",
            SelectLeft => "Edit: Select left",
            SelectRight => "Edit: Select right",
            SelectUp => "Edit: Select up",
            SelectDown => "Edit: Select down",
            LineStart => "Edit: Start of line",
            LineEnd => "Edit: End of line",
            DocumentStart => "Edit: Start of document",
            DocumentEnd => "Edit: End of document",
            Indent => "Edit: Indent",
            Outdent => "Edit: Outdent",
            Newline => "Edit: New line",
            InsertText => "Edit: Insert bound text",
            Copy => "Edit: Copy",
            Cut => "Edit: Cut",
            Paste => "Edit: Paste",
        }
    }
}
impl Document {
    pub(super) fn editing_command_entries(&self, cx: &impl rxui::ReadContext) -> Vec<Entry> {
        let enabled = self
            .buffer
            .as_ref()
            .is_some_and(|b| !b.editor.read(cx).has_composition())
            && !self.reloading();
        Edit::all()
            .into_iter()
            .map(|edit| Entry::editing(edit, enabled))
            .collect()
    }
    pub(super) fn execute_bound_edit(
        &mut self,
        edit: Edit,
        input: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        use velk_editor_widget::EditCommand as C;
        if self
            .buffer
            .as_ref()
            .is_none_or(|b| b.editor.read(cx).has_composition())
            || self.reloading()
        {
            return;
        }
        if input.is_some() && !matches!(edit, Edit::InsertText) {
            self.notice(Some(
                "Only Insert bound text accepts editing command input".into(),
            ));
            return;
        }
        if matches!(edit, Edit::Copy | Edit::Cut) {
            let text = self.buffer.editor.read(cx).selected_text();
            if text.is_empty() {
                return;
            }
            if let Err(error) = cx.write_clipboard_text(text) {
                self.notice(Some(error.to_string()));
                return;
            }
            if matches!(edit, Edit::Copy) {
                return;
            }
        }
        if matches!(edit, Edit::InsertText) && input.is_none() {
            self.notice(Some(
                "Insert bound text requires an input string in keybindings.json".into(),
            ));
            return;
        }
        let clipboard = if matches!(edit, Edit::Paste) {
            match cx.read_clipboard_text() {
                Ok(text) => Some(text),
                Err(error) => {
                    self.notice(Some(error.to_string()));
                    return;
                }
            }
        } else {
            None
        };
        self.buffer.editor.update(cx, |editor, _| {
            let command = match edit {
                Edit::Undo => C::Undo,
                Edit::Redo => C::Redo,
                Edit::SelectAll => C::SelectAll,
                Edit::Backspace => {
                    if editor.apply_editing_intent(velk_editing::Intent::Backspace) {
                        return;
                    }
                    C::Backspace
                }
                Edit::Delete | Edit::Cut => C::Delete,
                Edit::Left | Edit::SelectLeft => C::Horizontal {
                    right: false,
                    extend: matches!(edit, Edit::SelectLeft),
                },
                Edit::Right | Edit::SelectRight => C::Horizontal {
                    right: true,
                    extend: matches!(edit, Edit::SelectRight),
                },
                Edit::Up | Edit::SelectUp => C::Vertical {
                    down: false,
                    extend: matches!(edit, Edit::SelectUp),
                },
                Edit::Down | Edit::SelectDown => C::Vertical {
                    down: true,
                    extend: matches!(edit, Edit::SelectDown),
                },
                Edit::LineStart | Edit::LineEnd => C::LineEdge {
                    end: matches!(edit, Edit::LineEnd),
                    extend: false,
                },
                Edit::DocumentStart | Edit::DocumentEnd => C::DocumentEdge {
                    end: matches!(edit, Edit::DocumentEnd),
                    extend: false,
                },
                Edit::Newline => {
                    if editor.apply_editing_intent(velk_editing::Intent::Newline) {
                        return;
                    }
                    C::Insert("\n")
                }
                Edit::Indent => {
                    if editor.apply_editing_intent(velk_editing::Intent::Indent) {
                        return;
                    }
                    C::Insert("    ")
                }
                Edit::Outdent => {
                    editor.apply_editing_intent(velk_editing::Intent::Outdent);
                    return;
                }
                Edit::InsertText => {
                    let Some(text) = input.as_deref() else {
                        return;
                    };
                    C::Insert(text)
                }
                Edit::Paste => {
                    let Some(text) = clipboard.as_deref() else {
                        return;
                    };
                    C::Paste(text)
                }
                Edit::Copy => return,
            };
            editor.execute(command);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::{Runtime, Ui};
    struct Measure;
    impl rxui::TextMeasure for Measure {
        fn measure(
            &mut self,
            _: rxui::ElementId,
            request: rxui::TextRequest<'_>,
        ) -> Result<[f32; 2], rxui::UiError> {
            Ok([request.text.len() as f32 * 7., 18.])
        }
    }
    fn prepare(ui: &mut Ui<Document>, runtime: &mut Runtime) {
        runtime.flush().unwrap();
        ui.prepare(runtime, [900., 600.], &mut Measure).unwrap();
    }
    fn setup(json: &str) -> (Runtime, rxui::Entity<Document>, Ui<Document>) {
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| {
            cx.new(|cx| {
                let mut d = Document::empty(1);
                d.new_buffer(cx);
                d.keybindings.bindings = Bindings::parse(json.as_bytes()).unwrap();
                d.sync_all_keybindings(cx);
                d
            })
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        prepare(&mut ui, &mut runtime);
        let surface = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        assert!(ui.focus(surface));
        prepare(&mut ui, &mut runtime);
        (runtime, document, ui)
    }
    fn key(
        ui: &mut Ui<Document>,
        runtime: &mut Runtime,
        name: &str,
        repeat: bool,
    ) -> rxui::InputResult {
        let stroke = Stroke::parse(name).unwrap();
        let key = match stroke.key.as_str() {
            "enter" => KeyboardKey::Enter,
            "escape" => KeyboardKey::Escape,
            "tab" => KeyboardKey::Tab,
            key if key.starts_with('f') && key.len() > 1 => KeyboardKey::Other(key.to_uppercase()),
            key => KeyboardKey::Character(key.into()),
        };
        let result = ui
            .key(
                runtime,
                KeyEvent {
                    key,
                    pressed: true,
                    repeat,
                    modifiers: rxui::Modifiers {
                        control: stroke.control,
                        meta: stroke.meta,
                        alt: stroke.alt,
                        shift: stroke.shift,
                    },
                },
            )
            .unwrap();
        prepare(ui, runtime);
        result
    }
    #[test]
    fn context_repeat_timeout_ime_and_failed_chords_preserve_input_boundaries() {
        let (mut runtime, document, mut ui) = setup(
            r#"{"bindings":[
            {"keys":"F6","command":"velk.edit.insert-text","input":"once","when":{"mode":"insert"}},
            {"keys":"F7 F8","command":"velk.file.new"}
        ]}"#,
        );
        key(&mut ui, &mut runtime, "F6", false);
        key(&mut ui, &mut runtime, "F6", true);
        runtime.update(|cx| {
            assert_eq!(
                document
                    .read(cx)
                    .buffer
                    .editor
                    .read(cx)
                    .buffer()
                    .text()
                    .to_string(),
                "once"
            )
        });
        key(&mut ui, &mut runtime, "F7", false);
        assert!(
            !key(&mut ui, &mut runtime, "x", false).default_prevented,
            "a failed chord's final text key returns to ordinary input"
        );
        key(&mut ui, &mut runtime, "F7", false);
        runtime.update(|cx| {
            document.update(cx, |d, _| {
                d.keybindings.pending.as_mut().unwrap().deadline =
                    Instant::now() - Duration::from_secs(1)
            })
        });
        key(&mut ui, &mut runtime, "F8", false);
        runtime.update(|cx| assert_eq!(document.read(cx).ids().len(), 1));
        ui.text_input(
            &mut runtime,
            rxui::TextInputEvent::Preedit {
                text: "test".into(),
                cursor: None,
            },
            &mut Measure,
        )
        .unwrap();
        prepare(&mut ui, &mut runtime);
        key(&mut ui, &mut runtime, "F6", false);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert!(d.buffer.editor.read(cx).has_composition());
            assert_eq!(d.buffer.editor.read(cx).buffer().text().to_string(), "once");
        });
    }
    #[test]
    fn chords_override_host_shortcuts_and_cancel_without_leaking_prefix_text() {
        let (mut runtime, document, mut ui) = setup(
            r#"{"bindings":[
            {"keys":"Primary+n","command":"velk.edit.insert-text","input":"override"},
            {"keys":"Primary+k Primary+s","command":"velk.file.new"},
            {"keys":"F8","command":null},
            {"keys":"Primary+z","command":"velk.edit.insert-text","input":"reserved-undo"}
        ]}"#,
        );
        assert!(
            ui.query_command::<rxui::standard_commands::Undo>()
                .unwrap()
                .shortcuts
                .is_empty(),
            "native Undo must relinquish an overridden accelerator"
        );
        assert!(key(&mut ui, &mut runtime, "Primary+n", false).default_prevented);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert_eq!(d.ids().len(), 1);
            assert_eq!(
                d.buffer.editor.read(cx).buffer().text().to_string(),
                "override"
            );
        });
        assert!(key(&mut ui, &mut runtime, "Primary+z", false).default_prevented);
        runtime.update(|cx| {
            assert_eq!(
                document
                    .read(cx)
                    .buffer
                    .editor
                    .read(cx)
                    .buffer()
                    .text()
                    .to_string(),
                "overridereserved-undo"
            )
        });
        assert!(key(&mut ui, &mut runtime, "Primary+z", false).default_prevented);
        runtime.update(|cx| {
            assert_eq!(
                document
                    .read(cx)
                    .buffer
                    .editor
                    .read(cx)
                    .buffer()
                    .text()
                    .to_string(),
                "overridereserved-undoreserved-undo"
            )
        });
        assert!(key(&mut ui, &mut runtime, "Primary+k", false).default_prevented);
        runtime.update(|cx| assert!(document.read(cx).keybindings.pending.is_some()));
        key(&mut ui, &mut runtime, "Escape", false);
        runtime.update(|cx| assert!(document.read(cx).keybindings.pending.is_none()));
        key(&mut ui, &mut runtime, "Primary+k", false);
        key(&mut ui, &mut runtime, "Primary+s", false);
        runtime.update(|cx| assert_eq!(document.read(cx).ids().len(), 2));
        let surface = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        assert!(ui.focus(surface));
        prepare(&mut ui, &mut runtime);
        assert!(key(&mut ui, &mut runtime, "F8", false).default_prevented);
    }
    #[test]
    fn window_bindings_work_without_buffers_and_editor_bindings_leave_modals_alone() {
        let (mut runtime, document, mut ui) = setup(
            r#"{"bindings":[
            {"keys":"F6","command":"velk.file.new","when":{"scope":"window"}},
            {"keys":"Primary+n","command":"velk.file.new","when":{"scope":"window"}},
            {"keys":"Enter","command":"velk.edit.insert-text","input":"should-not-type"}
        ]}"#,
        );
        runtime.update(|cx| document.update(cx, |d, cx| d.close_buffer(d.buffer.id, cx)));
        prepare(&mut ui, &mut runtime);
        key(&mut ui, &mut runtime, "F6", false);
        runtime.update(|cx| assert_eq!(document.read(cx).ids().len(), 1));
        key(&mut ui, &mut runtime, "Primary+Shift+p", false);
        runtime.update(|cx| assert!(document.read(cx).palette.open));
        key(&mut ui, &mut runtime, "Escape", false);
        runtime.update(|cx| assert!(!document.read(cx).palette.open));
    }
    #[test]
    fn invalid_configuration_keeps_the_last_valid_map_and_missing_file_restores_defaults() {
        use crate::document::tests::QueuedExecutor;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keybindings.json");
        std::fs::write(
            &path,
            r#"{"bindings":[{"keys":"F6","command":"velk.file.new"}]}"#,
        )
        .unwrap();
        let mut runtime = Runtime::new();
        let executor = std::sync::Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.keybindings.path = Some(path.clone());
                d.reload_keybindings(cx);
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| assert_eq!(document.read(cx).keybindings.bindings.0.len(), 1));
        std::fs::write(&path, "invalid JSON").unwrap();
        runtime.update(|cx| document.update(cx, |d, cx| d.refresh_keybindings(cx)));
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert!(d.keybindings.error.is_some());
            assert_eq!(d.keybindings.bindings.0.len(), 1);
        });
        std::fs::remove_file(&path).unwrap();
        runtime.update(|cx| document.update(cx, |d, cx| d.refresh_keybindings(cx)));
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert!(d.keybindings.error.is_none());
            assert!(d.keybindings.bindings.0.is_empty());
        });
    }

    #[test]
    fn native_reservations_filter_contexts_and_deduplicate_shared_chord_prefixes() {
        let (mut runtime, document, _ui) = setup(
            r#"{"bindings":[
            {"keys":"Primary+j","command":null},
            {"keys":"Primary+j","command":null,"when":{"scope":"window"}},
            {"keys":"F6 F7","command":null},
            {"keys":"F6 F8","command":null},
            {"keys":"Primary+k","command":null,"when":{"mode":"normal"}},
            {"keys":"Primary+l","command":null,"when":{"language":"rust"}}
        ]}"#,
        );
        runtime.update(|cx| {
            let d = document.read(cx);
            let all = d.reserved_keybindings(false, cx);
            assert_eq!(all.len(), 2);
            assert!(all.contains(&Shortcut::primary("j")));
            assert!(all.contains(&Shortcut::new(
                KeyboardKey::Other("F6".into()),
                Default::default()
            )));
            assert_eq!(
                d.reserved_keybindings(true, cx),
                vec![Shortcut::primary("j")]
            );
        });
    }
}
