//! Host command discovery. Entries wrap the same actions used by shortcuts and menus.
use super::Document;
use rxui::{AppContext, CommandAction, Element, prelude::*};
use std::rc::Rc;

pub(super) struct Toggle;
impl Command for Toggle {}
struct Close;
impl Command for Close {}
struct Next;
impl Command for Next {}
struct Previous;
impl Command for Previous {}
struct Accept;
impl Command for Accept {}

#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    pub(super) query: String,
    pub(super) selected: usize,
}

#[derive(Clone)]
pub(super) struct Entry {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) shortcut: String,
    pub(super) enabled: bool,
    invoke: Invocation,
}
#[derive(Clone)]
enum Invocation {
    Edit(super::keybindings::Edit),
    Host(Rc<dyn Fn(&mut AppContext<'_>)>),
    Plugin(Box<super::plugin_commands::Invoke>),
}
impl Entry {
    pub fn plugin(invocation: super::plugin_commands::Invoke, enabled: bool) -> Self {
        Self {
            id: format!("{}:{}", invocation.plugin, invocation.command.id),
            label: invocation.command.title.clone(),
            shortcut: invocation
                .command
                .shortcut
                .as_ref()
                .map_or_else(String::new, |s| {
                    format!(
                        "{}{}{}{}{}",
                        if s.primary {
                            primary("")
                        } else {
                            String::new()
                        },
                        if s.control { "Ctrl+" } else { "" },
                        if s.alt { "Alt+" } else { "" },
                        if s.shift { "Shift+" } else { "" },
                        s.key
                    )
                }),
            enabled,
            invoke: Invocation::Plugin(Box::new(invocation)),
        }
    }

    pub fn editing(edit: super::keybindings::Edit, enabled: bool) -> Self {
        Self {
            id: edit.id().into(),
            label: edit.title().into(),
            shortcut: String::new(),
            enabled,
            invoke: Invocation::Edit(edit),
        }
    }
    pub fn matches_id(&self, id: &str) -> bool {
        self.id == id
            || matches!(&self.invoke, Invocation::Plugin(invocation) if invocation.command.id == id)
    }
    pub fn plugin_id(&self) -> Option<&str> {
        match &self.invoke {
            Invocation::Plugin(invocation) => Some(&invocation.command.id),
            _ => None,
        }
    }
    pub fn new<C: Command>(
        id: &'static str,
        action: CommandAction<C>,
        shortcut: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: action.caption().into(),
            shortcut: shortcut.into(),
            enabled: action.is_enabled(),
            invoke: Invocation::Host(Rc::new(move |cx| {
                let _ = action.invoke(cx);
            })),
        }
    }
}
pub(super) fn primary(key: &str) -> String {
    format!(
        "{}{key}",
        if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl+"
        }
    )
}

// Small command sets do not need a search worker or a full-text index. Match
// Unicode case-insensitive subsequences, favoring contiguous and word starts.
fn score(label: &str, query: &str) -> Option<usize> {
    let label: Vec<_> = label.to_lowercase().chars().collect();
    let mut position = 0;
    let mut cost = 0;
    for needle in query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
    {
        let offset = label[position..].iter().position(|&c| c == needle)?;
        let index = position + offset;
        cost += offset * 4;
        if index > 0 && label[index - 1].is_alphanumeric() && offset > 0 {
            cost += 3;
        }
        position = index + 1;
    }
    Some(cost)
}
fn matches(entries: Vec<Entry>, query: &str) -> Vec<Entry> {
    let mut matches: Vec<_> = entries
        .into_iter()
        .filter_map(|entry| score(&entry.label, query).map(|score| (score, entry)))
        .collect();
    matches.sort_by_key(|(score, _)| *score);
    matches.into_iter().map(|(_, entry)| entry).collect()
}

impl Document {
    fn set_palette(&mut self, open: bool, cx: &mut Context<'_, Self>) {
        self.palette.open = open;
        if open {
            self.palette.query.clear();
            self.palette.selected = 0;
            self.shell.open = false;
            self.plugin_menu_open = false;
        } else {
            let _ = self.editor_focus.focus(cx);
        }
    }
    fn execute_palette(&mut self, entry: &Entry, cx: &mut Context<'_, Self>) {
        self.execute_bound_entry(entry, None, cx);
    }
    pub(super) fn execute_bound_entry(
        &mut self,
        entry: &Entry,
        input: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        if !entry.enabled {
            return;
        }
        if self.palette.open {
            self.set_palette(false, cx);
        }
        let invoke = match entry.invoke.clone() {
            Invocation::Edit(edit) => {
                self.execute_bound_edit(edit, input, cx);
                return;
            }
            Invocation::Plugin(invocation) => {
                self.invoke_plugin_command_with_input(*invocation, input, cx);
                return;
            }
            Invocation::Host(invoke) => {
                if input.is_some() {
                    self.notice(Some("This command does not accept fixed input".into()));
                    return;
                }
                invoke
            }
        };
        // Release the Document borrow before invoking its shared command action.
        // No UI event can interleave before this deferred update is processed.
        cx.defer(move |cx| invoke(cx));
    }
    pub(super) fn command_palette(
        &self,
        cx: &mut ViewContext<'_, Self>,
        entries: Vec<Entry>,
    ) -> (CommandAction<Toggle>, Option<Element>) {
        let toggle = cx
            .command(Toggle, |this, _, cx| {
                this.set_palette(!this.palette.open, cx)
            })
            .label("Command palette")
            .enabled(!self.reloading())
            .allow_in_modal(true)
            .shortcut(Shortcut::primary("p").shift())
            .shortcut(Shortcut::new(
                KeyboardKey::Other("F3".into()),
                Default::default(),
            ))
            .shortcut(Shortcut::new(
                KeyboardKey::Other("Named(F3)".into()),
                Default::default(),
            ));
        if !self.palette.open {
            return (toggle, None);
        }
        let entries = matches(entries, &self.palette.query);
        let count = entries.len();
        let selected = self.palette.selected.min(count.saturating_sub(1));
        let current = entries.get(selected).cloned();
        let accept = cx
            .command(Accept, move |this, _, cx| {
                if let Some(entry) = &current {
                    this.execute_palette(entry, cx);
                }
            })
            .enabled(entries.get(selected).is_some_and(|entry| entry.enabled))
            .shortcut(Shortcut::new(KeyboardKey::Enter, Default::default()));
        let next = cx
            .command(Next, move |this, _, _| {
                this.palette.selected = if count == 0 {
                    0
                } else {
                    (selected + 1) % count
                };
            })
            .repeat(true)
            .shortcut(Shortcut::new(KeyboardKey::ArrowDown, Default::default()));
        let previous = cx
            .command(Previous, move |this, _, _| {
                this.palette.selected = if count == 0 {
                    0
                } else {
                    (selected + count - 1) % count
                };
            })
            .repeat(true)
            .shortcut(Shortcut::new(KeyboardKey::ArrowUp, Default::default()));
        let close = cx
            .command(Close, |this, _, cx| this.set_palette(false, cx))
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let mut content = column()
            .fill_width()
            .gap(6.)
            .padding(8.)
            .on_command(toggle.clone())
            .on_command(next)
            .on_command(previous)
            .on_command(close)
            .child(
                text_input(self.palette.query.clone())
                    .key("command-query")
                    .fill_width()
                    .on_command(accept)
                    .accessibility_label("Search commands")
                    .placeholder("Type a command…")
                    .on_change(cx.listener(|this, event: &TextChangeEvent, _| {
                        this.palette.query = event.value.clone();
                        this.palette.selected = 0;
                    })),
            );
        if entries.is_empty() {
            content = content.child(
                label("No matching commands")
                    .key("command-empty")
                    .padding(8.),
            );
        }
        // Keep the keyboard selection visible without a second scroll/focus
        // owner. A small moving result window also bounds future larger lists.
        let start = selected.saturating_sub(5);
        for (index, entry) in entries.into_iter().enumerate().skip(start).take(8) {
            let selected = index == selected;
            let caption = format!(
                "{}{}{}{}",
                if selected { "› " } else { "  " },
                entry.label,
                if entry.shortcut.is_empty() {
                    ""
                } else {
                    "    "
                },
                entry.shortcut
            );
            let key = entry.id.clone();
            let enabled = entry.enabled;
            content = content.child(
                button(caption)
                    .key(key)
                    .fill_width()
                    .disabled(!enabled)
                    .background(if selected {
                        ThemeColor::Selection
                    } else {
                        ThemeColor::Control
                    })
                    .on_click(cx.listener(move |this, _, cx| this.execute_palette(&entry, cx))),
            );
        }
        content = content.child(
            label(format!(
                "{count} commands · ↑ ↓ navigate · Enter run · Esc close"
            ))
            .font_size(12.)
            .color(ThemeColor::TextMuted),
        );
        (
            toggle,
            Some(
                modal(content)
                    .key("command-palette")
                    .width(560.)
                    .accessibility_label("Command palette")
                    .on_dismiss(
                        cx.listener(|this, _: &DismissEvent, cx| this.set_palette(false, cx)),
                    )
                    .into_element(),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::{KeyEvent, Runtime, TextInputEvent, Ui};

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
        ui.prepare(runtime, [800., 500.], &mut Measure).unwrap();
    }
    fn key(
        ui: &mut Ui<Document>,
        runtime: &mut Runtime,
        key: KeyboardKey,
        modifiers: rxui::Modifiers,
    ) {
        ui.key(
            runtime,
            KeyEvent {
                key,
                modifiers,
                pressed: true,
                repeat: false,
            },
        )
        .unwrap();
        prepare(ui, runtime);
    }
    fn type_query(ui: &mut Ui<Document>, runtime: &mut Runtime, query: &str) {
        ui.text_input(runtime, TextInputEvent::Insert(query.into()), &mut Measure)
            .unwrap();
        prepare(ui, runtime);
    }
    #[test]
    fn fuzzy_matching_handles_case_unicode_gaps_and_no_matches() {
        assert!(score("Plugins: Load from disk", "pld").is_some());
        assert!(score("Settings", "SET").is_some());
        assert!(score("Öffnen", "öf").is_some());
        assert!(score("Settings", " ").is_some());
        assert!(score("Save", "ss").is_none());
        assert!(score("Save", "vs").is_none());
        assert!(
            score("Plugins: Load from disk", "load").unwrap()
                < score("Plugins: Unload all", "load").unwrap()
        );
    }
    #[test]
    fn palette_focus_filter_execute_and_cancel_preserve_document_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path, cx)));
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        prepare(&mut ui, &mut runtime);
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        let bounds = ui.elements().find(|e| e.id == source).unwrap().bounds;
        ui.focus(source);
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Character("p".into()),
            Shortcut::primary("p").shift().modifiers,
        );
        let input = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("command-query")))
            .unwrap()
            .id;
        assert_eq!(
            ui.focused_element(),
            Some(input),
            "palette must immediately accept typing"
        );
        assert!(!ui.focus(source), "modal must contain focus");
        assert_eq!(
            ui.elements().find(|e| e.id == source).unwrap().bounds,
            bounds
        );
        // Disabled Save must consume Enter without closing or editing anything.
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Enter,
            Default::default(),
        );
        runtime.update(|cx| assert!(document.read(cx).palette.open));
        type_query(&mut ui, &mut runtime, "stgs");
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("velk.settings.open")))
        );
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Enter,
            Default::default(),
        );
        runtime.update(|cx| {
            assert!(!document.read(cx).palette.open);
            assert!(
                document.read(cx).shell.open,
                "shared action must run after releasing owner borrow"
            );
        });
        // The global shortcut can replace another modal with the palette.
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Other("Named(F3)".into()),
            Default::default(),
        );
        runtime.update(|cx| assert!(!document.read(cx).shell.open));
        type_query(&mut ui, &mut runtime, "zzzz");
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("command-empty")))
        );
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::ArrowDown,
            Default::default(),
        );
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Enter,
            Default::default(),
        );
        runtime.update(|cx| assert!(document.read(cx).palette.open));
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Escape,
            Default::default(),
        );
        assert_eq!(ui.focused_element(), Some(source));
        // Reopening resets query/selection. Up wraps and keeps the last result visible.
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Other("F3".into()),
            Default::default(),
        );
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::ArrowUp,
            Default::default(),
        );
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("velk.keybindings.commands")))
        );
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::ArrowDown,
            Default::default(),
        );
        runtime.update(|cx| assert_eq!(document.read(cx).palette.selected, 0));
        // Enter on a button reached with Tab/focus runs that button, even when
        // the list's highlighted result is the disabled Save action.
        let save_all = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("velk.file.save-all")))
            .unwrap()
            .id;
        assert!(ui.focus(save_all));
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Enter,
            Default::default(),
        );
        assert!(ui.activate_focused(&mut runtime).unwrap());
        prepare(&mut ui, &mut runtime);
        runtime.update(|cx| assert!(!document.read(cx).palette.open));
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Other("F3".into()),
            Default::default(),
        );
        type_query(&mut ui, &mut runtime, "find");
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Enter,
            Default::default(),
        );
        let search = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("search-query")))
            .unwrap()
            .id;
        assert_eq!(
            ui.focused_element(),
            Some(search),
            "Find must focus its own input"
        );
        runtime.update(|cx| {
            assert!(!document.read(cx).palette.open);
            assert!(document.read(cx).buffer.search.open);
            assert_eq!(editor.read(cx).buffer().text().to_string(), "original");
            assert!(!editor.read(cx).buffer().is_dirty());
            assert!(!editor.read(cx).buffer().can_undo());
        });
    }
}
