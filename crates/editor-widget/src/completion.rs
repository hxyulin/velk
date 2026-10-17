use crate::Editor;
use rxui::{Element, prelude::*};
use velk_commands::completion::frontend::{self, Provider};
use velk_commands::{
    completion::Item,
    completion::{DocumentationTarget, Session, Target},
};

#[derive(Default)]
pub(super) struct State {
    session: Option<Session>,
    accepted: Option<(Target, Item)>,
    generation: u64,
    typed: Option<(Target, String)>,
    documentation: Option<(DocumentationTarget, std::sync::Arc<velk_markup::Document>)>,
    scroll: rxui::ScrollHandle,
    frontend: Option<std::sync::Arc<dyn frontend::Provider>>,
    view: std::cell::RefCell<Option<(frontend::Snapshot, frontend::View)>>,
}
impl Editor {
    fn completion_target(&self) -> Target {
        Target {
            revision: self.buffer.revision(),
            position: self.caret_position(),
        }
    }
    fn completion_prefix(&self) -> String {
        let text = self.buffer.text();
        let end = self.caret_position();
        let mut start = end;
        while start > 0 {
            let character = text.char(start - 1);
            if character != '_' && !character.is_alphanumeric() {
                break;
            }
            start -= 1;
        }
        text.slice(start..end).to_string()
    }
    pub(super) fn record_completion_typing(&mut self, text: &str) {
        if text.chars().count() == 1
            && !self.has_composition()
            && self.buffer.selection().is_empty()
            && !self.editing.input_state().capture_text
        {
            self.completion.typed = Some((self.completion_target(), text.into()));
        }
    }
    /// Consume one keyboard insertion; paste, undo and programmatic edits do not activate.
    pub fn take_completion_typing(&mut self) -> Option<(Target, String, String)> {
        let (target, text) = self.completion.typed.take()?;
        (target == self.completion_target()).then(|| (target, text, self.completion_prefix()))
    }
    pub fn show_completions(&mut self, items: Vec<Item>, refresh: bool) {
        self.show_completions_with_policy(items, refresh, None);
    }
    /// A policy is retained by this session only; refreshes preserve its lease.
    pub fn show_completions_with_policy(
        &mut self,
        items: Vec<Item>,
        refresh: bool,
        policy: Option<std::sync::Arc<dyn velk_commands::completion::matching::Policy>>,
    ) {
        let target = self.completion_target();
        if refresh {
            if let Some(session) = &mut self.completion.session {
                session.resolve(target, items);
            }
        } else {
            self.completion.session = Some(Session::with_policy(
                target,
                self.completion_prefix(),
                items,
                policy.unwrap_or_else(|| {
                    std::sync::Arc::new(velk_commands::completion::matching::Prefix)
                }),
            ));
        }
        self.hide_hover();
    }
    /// Select presentation for this session without changing candidates or matching.
    pub fn set_completion_frontend(
        &mut self,
        provider: Option<std::sync::Arc<dyn frontend::Provider>>,
    ) {
        self.completion.frontend = provider;
        *self.completion.view.borrow_mut() = None;
    }
    fn completion_view(&self) -> Option<frontend::View> {
        let session = self.completion.session.as_ref()?;
        let snapshot = frontend::Snapshot {
            candidates: session
                .visible
                .iter()
                .map(|i| frontend::Candidate {
                    title: session.items[*i].title.chars().take(1024).collect(),
                    detail: session.items[*i]
                        .detail
                        .as_ref()
                        .map(|s| s.chars().take(1024).collect()),
                })
                .collect(),
            selected: session.selected,
            viewport: self.geometry.borrow().viewport,
            documentation: session.choice().is_some_and(|item| {
                item.documentation.is_some() || item.documentation_resolve.is_some()
            }),
            ready: session.ready,
        };
        if let Some((previous, view)) = self.completion.view.borrow().as_ref()
            && previous == &snapshot
        {
            return Some(view.clone());
        }
        let view = self
            .completion
            .frontend
            .as_ref()
            .and_then(|provider| provider.present(&snapshot).ok())
            .filter(|view| view.validate(&snapshot).is_ok())
            .or_else(|| frontend::Fallback.present(&snapshot).ok())?;
        *self.completion.view.borrow_mut() = Some((snapshot, view.clone()));
        Some(view)
    }
    /// Currently highlighted proposal, which may still await a refreshed revision.
    pub fn completion_choice(&self) -> Option<&Item> {
        self.completion.session.as_ref()?.choice()
    }
    pub fn completion_documentation_target(&self) -> Option<DocumentationTarget> {
        let session = self
            .completion
            .session
            .as_ref()
            .filter(|session| session.ready)?;
        let item = session.choice()?;
        let source = match &item.action {
            velk_commands::ChoiceAction::Contributed {
                plugin, generation, ..
            } => Some((plugin.clone(), *generation)),
            _ => None,
        };
        Some(DocumentationTarget {
            target: session.target,
            epoch: self.completion.generation,
            id: item.id.clone(),
            source,
        })
    }
    pub fn completion_documentation(&self) -> Option<&velk_markup::Document> {
        let (target, document) = self.completion.documentation.as_ref()?;
        (self.completion_documentation_target().as_ref() == Some(target))
            .then_some(document.as_ref())
    }
    pub fn resolve_completion_documentation(
        &mut self,
        target: DocumentationTarget,
        document: std::sync::Arc<velk_markup::Document>,
    ) -> bool {
        if self.completion_documentation_target().as_ref() != Some(&target) {
            return false;
        }
        if self
            .completion
            .documentation
            .as_ref()
            .is_none_or(|(old, _)| *old != target)
        {
            self.completion.scroll = Default::default();
        }
        self.completion.documentation = Some((target, document));
        true
    }
    pub fn completion_active(&self) -> bool {
        self.completion.session.is_some()
    }
    pub fn completion_refresh(&self) -> Option<Target> {
        self.completion
            .session
            .as_ref()
            .filter(|session| !session.ready)
            .map(|session| session.target)
    }
    pub fn take_completion(&mut self) -> Option<(Target, Item)> {
        self.completion.accepted.take()
    }
    pub fn completion_epoch(&self) -> u64 {
        self.completion.generation
    }
    pub fn dismiss_completions(&mut self) {
        self.completion.session = None;
        self.completion.frontend = None;
        *self.completion.view.borrow_mut() = None;
        self.completion.accepted = None;
        self.completion.typed = None;
        self.completion.documentation = None;
        self.completion.generation += 1;
    }
    pub(super) fn update_completion(&mut self) {
        self.completion.typed = None;
        if self.completion.session.is_none() {
            self.completion.generation += 1;
        }
        let target = self.completion_target();
        let prefix = self.completion_prefix();
        if self.composition.is_some()
            || !self.buffer.selection().is_empty()
            || self
                .completion
                .session
                .as_mut()
                .is_some_and(|session| !session.update(target, prefix))
        {
            self.dismiss_completions();
        }
    }
    fn accept_completion(&mut self) {
        let Some(session) = &self.completion.session else {
            return;
        };
        let Some(choice) = session.accept() else {
            return;
        };
        self.completion.accepted = Some((session.target, choice));
        self.completion.session = None;
        self.completion.frontend = None;
        *self.completion.view.borrow_mut() = None;
    }
    pub(super) fn completion_key(&mut self, event: &KeyInput) -> bool {
        if self.completion.session.is_none() && event.event.key == KeyboardKey::Escape {
            self.dismiss_completions();
        }
        let Some(session) = &mut self.completion.session else {
            return false;
        };
        let modifiers = event.event.modifiers;
        if modifiers.control || modifiers.meta || modifiers.alt {
            self.dismiss_completions();
            return false;
        }
        match event.event.key {
            KeyboardKey::ArrowDown => session.next(false),
            KeyboardKey::ArrowUp => session.next(true),
            KeyboardKey::Escape => self.dismiss_completions(),
            KeyboardKey::Enter | KeyboardKey::Tab if !modifiers.shift => {
                if session.choice().is_none() {
                    self.dismiss_completions();
                    return false;
                }
                self.accept_completion();
            }
            _ => return false,
        }
        event.prevent_default();
        true
    }
    pub(super) fn completion_popup(&self, cx: &mut ViewContext<'_, Self>) -> Option<Element> {
        let session = self.completion.session.as_ref()?;
        if session.visible.is_empty() {
            return None;
        }
        let geometry = self.geometry.borrow();
        let caret = geometry.caret_area();
        let anchor = [
            geometry.origin[0] + caret.x,
            geometry.origin[1] + caret.y + caret.height,
        ];
        let view = self.completion_view()?;
        let has_documentation = view.documentation != frontend::Documentation::Hidden;
        let alongside = view.documentation == frontend::Documentation::Alongside;
        let width = view.width.min((geometry.viewport[0] - 24.).max(120.));
        let total_width = if alongside {
            (width + view.documentation_width).min((geometry.viewport[0] - 24.).max(120.))
        } else {
            width
        };
        let max_chars = ((width - 16.) / 10.).max(4.) as usize;
        let summary = |text: &str| {
            let first = text.lines().next().unwrap_or_default();
            let mut result: String = first.chars().take(max_chars).collect();
            if first.chars().count() > max_chars {
                result.push('…');
            }
            result
        };
        let mut rows = column()
            .gap(0.)
            .accessibility_role(SemanticRole::ListBox)
            .accessibility_label("Completions");
        let epoch = self.completion.generation;
        let target = session.target;
        for item in &view.rows {
            let visible = item.candidate;
            let identity = session.items[session.visible[visible]].clone();
            let selected = visible == session.selected;
            let row = column()
                .gap(2.)
                .padding(6.)
                .accessibility_role(SemanticRole::ListBoxOption)
                .accessibility_label(item.title.clone())
                .accessibility_selected(selected)
                .child(label(summary(&item.title)).font_size(13.).clip())
                .children(item.detail.as_ref().map(|detail| {
                    label(summary(detail))
                        .font_size(11.)
                        .clip()
                        .color(ThemeColor::TextMuted)
                }));
            rows = rows.child(
                row.background(if selected {
                    ThemeColor::Selection
                } else {
                    ThemeColor::Surface
                })
                .on_pointer_down(cx.listener(
                    move |this, event: &PointerInput, _| {
                        if this.completion.generation != epoch {
                            return;
                        }
                        let Some(session) = &mut this.completion.session else {
                            return;
                        };
                        if session.target != target
                            || session
                                .visible
                                .get(visible)
                                .is_none_or(|i| session.items[*i] != identity)
                        {
                            return;
                        }
                        session.selected = visible;
                        this.accept_completion();
                        event.prevent_default();
                    },
                )),
            );
        }
        let mut content = if alongside { row() } else { column() }
            .gap(0.)
            .child(rows.width(width));
        if has_documentation {
            let body = match self.completion_documentation() {
                Some(document) if !document.text().trim().is_empty() => {
                    crate::documentation::body(document)
                }
                Some(_) => label("No documentation available").color(ThemeColor::TextMuted),
                None => label("Loading documentation…").color(ThemeColor::TextMuted),
            };
            let panel_width = if alongside {
                total_width - width
            } else {
                width
            };
            content = content.child(
                column()
                    .gap(6.)
                    .padding(8.)
                    .width(panel_width)
                    .child(
                        label("Documentation")
                            .font_size(11.)
                            .color(ThemeColor::TextMuted),
                    )
                    .child(
                        scroll_area(
                            body.selectable(true)
                                .font_size(13.)
                                .color(ThemeColor::Text)
                                .fill_width(),
                        )
                        .handle(self.completion.scroll.clone())
                        .scrollbar_style(ScrollbarStyle::platform())
                        .fill_width()
                        .into_element()
                        .key("completion-documentation")
                        .max_height(
                            view.documentation_height
                                .min((geometry.viewport[1] - 24.).max(60.)),
                        )
                        .accessibility_label("Completion documentation"),
                    ),
            );
        }
        Some(
            popover(anchor, content)
                .key("editor-completions")
                .width(total_width)
                .padding(0.)
                .gap(0.)
                .autofocus(false)
                .transition(rxui::Transition::None)
                .on_dismiss(cx.listener(|this, _: &DismissEvent, _| this.dismiss_completions()))
                .into_element()
                .pointer_events(PointerEvents::Auto),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(title: &str) -> Item {
        velk_commands::Choice {
            title: title.into(),
            detail: None,
            action: velk_commands::ChoiceAction::Semantic {
                payload: Default::default(),
            },
        }
        .into()
    }
    #[test]
    fn typed_prefix_refreshes_without_accepting_stale_edits_and_movement_dismisses() {
        let mut editor = Editor::new("pr");
        editor.select_range(2..2);
        editor.show_completions(vec![item("print"), item("process")], false);
        assert!(editor.completion_active());
        editor.insert("i");
        assert!(editor.completion_refresh().is_some());
        assert_eq!(editor.completion.session.as_ref().unwrap().visible.len(), 1);
        editor.accept_completion();
        assert!(editor.take_completion().is_none());
        editor.show_completions(vec![item("print")], true);
        editor.accept_completion();
        assert_eq!(editor.take_completion().unwrap().1.title, "print");
        assert!(!editor.completion_active());
        editor.show_completions(vec![item("print")], false);
        editor.execute(crate::EditCommand::Horizontal {
            right: false,
            extend: false,
        });
        assert!(!editor.completion_active());
    }
    #[test]
    fn cancelled_session_cannot_be_restored_by_a_refresh() {
        let mut editor = Editor::new("p");
        editor.select_range(1..1);
        editor.show_completions(vec![item("print")], false);
        let epoch = editor.completion_epoch();
        editor.dismiss_completions();
        editor.show_completions(vec![item("print")], true);
        assert!(!editor.completion_active());
        assert_ne!(epoch, editor.completion_epoch());
    }
}

#[cfg(test)]
mod empty_result_tests {
    use super::*;
    #[test]
    fn empty_completion_results_do_not_swallow_enter() {
        let graphics = pollster::block_on(rxui::astrelis::GraphicsContext::headless()).unwrap();
        let mut editor = Editor::new("prefix");
        editor.select_range(6..6);
        editor.show_completions(vec![], false);
        let mut test = rxui::TestUi::new(&graphics, [640, 240], editor).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        let surface = test.element("editor-surface").unwrap().unwrap().id;
        test.parts().1.focus(surface);
        test.key(rxui::KeyEvent {
            key: KeyboardKey::Enter,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        })
        .unwrap();
        test.read(|editor| {
            assert!(!editor.completion_active());
            assert_eq!(editor.buffer().text().to_string(), "prefix\n");
        });
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;
    #[test]
    fn only_committed_keyboard_characters_activate_and_movement_invalidates_pending_policy() {
        let mut editor = Editor::new("p");
        editor.select_range(1..1);
        editor.committed_text("r", rxui::TextInsertSource::Keyboard);
        let (target, text, prefix) = editor.take_completion_typing().unwrap();
        assert_eq!(text, "r");
        assert_eq!(prefix, "pr");
        assert_eq!(target.position, 2);
        assert!(editor.take_completion_typing().is_none());
        let epoch = editor.completion_epoch();
        editor.execute(crate::EditCommand::Horizontal {
            right: false,
            extend: false,
        });
        editor.execute(crate::EditCommand::Horizontal {
            right: true,
            extend: false,
        });
        assert_eq!(editor.caret_position(), 2);
        assert_ne!(editor.completion_epoch(), epoch);
        editor.committed_text("paste", rxui::TextInsertSource::Paste);
        assert!(editor.take_completion_typing().is_none());
        editor.committed_text("界", rxui::TextInsertSource::Ime);
        assert!(editor.take_completion_typing().is_none());
        editor.insert("programmatic");
        assert!(editor.take_completion_typing().is_none());
        editor.execute(crate::EditCommand::Undo);
        assert!(editor.take_completion_typing().is_none());
        editor.committed_text("ab", rxui::TextInsertSource::Keyboard);
        assert!(editor.take_completion_typing().is_none());
    }
    #[test]
    fn dismissing_or_editing_cancels_unconsumed_typing() {
        let mut editor = Editor::new("");
        editor.committed_text("p", rxui::TextInsertSource::Keyboard);
        editor.dismiss_completions();
        assert!(editor.take_completion_typing().is_none());
        editor.committed_text("r", rxui::TextInsertSource::Keyboard);
        editor.insert("x");
        assert!(editor.take_completion_typing().is_none());
    }
}

#[cfg(test)]
mod documentation_tests {
    use super::*;
    use std::sync::Arc;
    fn item(id: &str) -> Item {
        let mut item: Item = velk_commands::Choice {
            title: id.into(),
            detail: None,
            action: velk_commands::ChoiceAction::Semantic {
                payload: Default::default(),
            },
        }
        .into();
        item.documentation = Some(velk_commands::completion::Documentation {
            text: "Documentation".into(),
            markdown: false,
        });
        item
    }
    #[test]
    fn selection_revision_and_dismissal_reject_stale_documentation() {
        let mut editor = Editor::new("");
        editor.show_completions(vec![item("alpha"), item("beta")], false);
        let old = editor.completion_documentation_target().unwrap();
        assert!(editor.resolve_completion_documentation(
            old.clone(),
            Arc::new(velk_markup::Document::plain("Alpha docs"))
        ));
        editor.completion.session.as_mut().unwrap().next(false);
        assert!(editor.completion_documentation().is_none());
        assert!(!editor.resolve_completion_documentation(
            old,
            Arc::new(velk_markup::Document::plain("Late alpha docs"))
        ));
        let current = editor.completion_documentation_target().unwrap();
        editor.insert("b");
        assert!(editor.completion_documentation_target().is_none());
        assert!(!editor.resolve_completion_documentation(
            current,
            Arc::new(velk_markup::Document::plain("Old revision"))
        ));
        editor.show_completions(vec![item("beta")], true);
        let current = editor.completion_documentation_target().unwrap();
        editor.dismiss_completions();
        assert!(!editor.resolve_completion_documentation(
            current,
            Arc::new(velk_markup::Document::plain("After dismissal"))
        ));
    }
    #[test]
    fn documentation_panel_is_bounded_scrollable_and_does_not_scroll_the_document() {
        let graphics = pollster::block_on(rxui::astrelis::GraphicsContext::headless()).unwrap();
        let mut editor = Editor::new("");
        editor.set_completion_frontend(Some(Arc::new(velk_plugin_completion::frontend::Popup)));
        editor.show_completions(vec![item("alpha")], false);
        let target = editor.completion_documentation_target().unwrap();
        editor.resolve_completion_documentation(
            target,
            Arc::new(velk_markup::Document::plain(
                "Documentation line\n".repeat(100),
            )),
        );
        let mut test = rxui::TestUi::new(&graphics, [900, 500], editor).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        test.prepare().unwrap();
        let bounds = test
            .element("completion-documentation")
            .unwrap()
            .unwrap()
            .bounds;
        assert!(bounds.height > 40. && bounds.height <= 220., "{bounds:?}");
        assert!(bounds.width <= 440.);
        let point = [bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.];
        let (runtime, ui, _) = test.parts();
        ui.wheel(runtime, point, [0., 100.], Default::default())
            .unwrap();
        assert!(ui.elements().any(|element| element.scroll_offset[1] > 0.));
        test.read(|editor| assert_eq!(editor.geometry.borrow().offset, [0., 0.]));
        test.prepare().unwrap();
        assert!(
            test.ui()
                .elements()
                .any(|element| element.scroll_offset[1] > 0.)
        );
    }
}

#[cfg(test)]
mod snippet_tests {
    use super::*;
    use velk_editor_model::EditCommand;
    fn snippet(source: &str) -> Editor {
        let expanded = velk_commands::snippet::expand(source).unwrap();
        let mut editor = Editor::new(&expanded.text);
        editor.start_snippet(expanded.stops).unwrap();
        editor
    }
    #[test]
    fn repeated_placeholders_edit_together_and_navigation_reaches_the_final_caret() {
        let mut editor = snippet("${1:世界} + $1, ${2:next}$0");
        assert_eq!(editor.selected_text(), "世界\n世界");
        editor.execute(EditCommand::Insert("value"));
        assert_eq!(editor.buffer().text().to_string(), "value + value, next");
        assert!(editor.snippet_active());
        assert!(editor.navigate_snippet(false));
        assert_eq!(editor.selected_text(), "next");
        assert!(editor.navigate_snippet(true));
        assert_eq!(editor.selected_text(), "value\nvalue");
        editor.navigate_snippet(false);
        editor.execute(EditCommand::Insert("last"));
        editor.navigate_snippet(false);
        assert!(!editor.snippet_active());
        assert_eq!(
            editor.buffer().selection().head,
            "value + value, last".chars().count()
        );
        assert!(!editor.navigate_snippet(false));
    }
    #[test]
    fn undo_movement_and_shared_view_edits_invalidate_navigation_without_undoing_the_session() {
        let mut editor = snippet("call(${1:arg}, ${2:other})$0");
        editor.execute(EditCommand::Insert("changed"));
        editor.execute(EditCommand::Undo);
        assert!(!editor.snippet_active());
        assert_eq!(editor.buffer().text().to_string(), "call(arg, other)");
        editor.execute(EditCommand::Redo);
        assert_eq!(editor.buffer().text().to_string(), "call(changed, other)");
        let mut editor = snippet("call(${1:arg})$0");
        editor.execute(EditCommand::SetCaret {
            position: 0,
            extend: false,
        });
        assert!(!editor.snippet_active());
        let mut editor = snippet("call(${1:arg})$0");
        let mut other = editor.new_view().unwrap();
        assert!(!other.snippet_active());
        other.execute(EditCommand::Insert("different"));
        editor.synchronize_document(None, false);
        assert!(!editor.snippet_active());
    }
    #[test]
    fn nested_defaults_can_be_entered_or_replaced_as_a_whole() {
        let mut editor = snippet("${1:call(${2:arg})}$0");
        editor.navigate_snippet(false);
        assert_eq!(editor.selected_text(), "arg");
        editor.execute(EditCommand::Insert("value"));
        editor.navigate_snippet(true);
        assert_eq!(editor.selected_text(), "call(value)");
        editor.execute(EditCommand::Insert("replacement"));
        editor.navigate_snippet(false);
        assert!(!editor.snippet_active());
        assert_eq!(editor.buffer().selection().head, 11);
        let mut editor = snippet("${1:${2:arg} tail} = $1$0");
        editor.navigate_snippet(false);
        editor.execute(EditCommand::Insert("value"));
        editor.navigate_snippet(true);
        assert_eq!(editor.selected_text(), "value tail\nvalue tail");
    }
    #[test]
    fn vim_insert_mode_preserves_mirrored_placeholder_typing() {
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
        let expansion = velk_commands::snippet::expand("${1:arg} + $1$0").unwrap();
        let mut editor = Editor::new(&expansion.text);
        editor.set_editing_providers(std::collections::BTreeMap::from([(
            velk_editing::Role::Keymap,
            std::sync::Arc::new(Vim) as std::sync::Arc<dyn velk_editing::Provider>,
        )]));
        editor.committed_text("i", rxui::TextInsertSource::Keyboard);
        editor.start_snippet(expansion.stops).unwrap();
        editor.committed_text("z", rxui::TextInsertSource::Keyboard);
        assert_eq!(editor.buffer().text().to_string(), "z + z");
        assert!(editor.snippet_active());
        editor.navigate_snippet(false);
        assert_eq!(editor.buffer().selection().head, 5);
    }
}

#[cfg(test)]
mod snippet_keyboard_tests {
    use super::*;
    #[test]
    fn tab_and_shift_tab_take_priority_over_completion_and_escape_ends_navigation() {
        let graphics = pollster::block_on(rxui::astrelis::GraphicsContext::headless()).unwrap();
        let expansion = velk_commands::snippet::expand("${1:first} ${2:second}$0").unwrap();
        let mut editor = Editor::new(&expansion.text);
        editor.start_snippet(expansion.stops).unwrap();
        editor.show_completions(
            vec![
                velk_commands::Choice {
                    title: "first_option".into(),
                    detail: None,
                    action: velk_commands::ChoiceAction::Semantic {
                        payload: Default::default(),
                    },
                }
                .into(),
            ],
            false,
        );
        let mut test = rxui::TestUi::new(&graphics, [640, 240], editor).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        let surface = test.element("editor-surface").unwrap().unwrap().id;
        test.parts().1.focus(surface);
        for (key, shift, selected) in [
            (KeyboardKey::Tab, false, "second"),
            (KeyboardKey::Tab, true, "first"),
            (KeyboardKey::Escape, false, "first"),
        ] {
            test.key(rxui::KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: rxui::Modifiers {
                    shift,
                    ..Default::default()
                },
            })
            .unwrap();
            test.read(|editor| assert_eq!(editor.selected_text(), selected));
        }
        test.read(|editor| assert!(!editor.snippet_active()));
    }
}

#[cfg(test)]
mod frontend_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Debug)]
    struct Invalid(Arc<AtomicUsize>);
    impl frontend::Provider for Invalid {
        fn present(&self, snapshot: &frontend::Snapshot) -> Result<frontend::View, String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            let mut view = frontend::Fallback.present(snapshot)?;
            view.rows[0].candidate = 9999;
            Ok(view)
        }
    }
    #[test]
    fn bad_frontend_falls_back_and_caches_until_selection_changes() {
        let mut editor = Editor::new("");
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(Invalid(calls.clone()));
        editor.set_completion_frontend(Some(provider.clone()));
        editor.show_completions(
            (0..3)
                .map(|i| {
                    velk_commands::Choice {
                        title: format!("item{i}"),
                        detail: None,
                        action: velk_commands::ChoiceAction::Semantic {
                            payload: Default::default(),
                        },
                    }
                    .into()
                })
                .collect(),
            false,
        );
        assert_eq!(editor.completion_view().unwrap().rows[0].candidate, 0);
        editor.completion_view();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        editor.completion.session.as_mut().unwrap().next(false);
        editor.completion_view();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        editor.accept_completion();
        assert_eq!(editor.take_completion().unwrap().1.title, "item1");
        assert_eq!(Arc::strong_count(&provider), 1);
    }
    #[test]
    fn compact_frontend_changes_rendering_without_changing_acceptance() {
        let graphics = pollster::block_on(rxui::astrelis::GraphicsContext::headless()).unwrap();
        let mut editor = Editor::new("");
        editor.set_completion_frontend(Some(Arc::new(velk_plugin_completion::frontend::Compact)));
        let mut item: Item = velk_commands::Choice {
            title: "print".into(),
            detail: Some("provider detail".into()),
            action: velk_commands::ChoiceAction::Semantic {
                payload: Default::default(),
            },
        }
        .into();
        item.documentation = Some(velk_commands::completion::Documentation {
            text: "docs".into(),
            markdown: false,
        });
        editor.show_completions(vec![item], false);
        let mut test = rxui::TestUi::new(&graphics, [900, 500], editor).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        test.prepare().unwrap();
        assert!(test.element("editor-completions").unwrap().is_some());
        assert!(test.element("completion-documentation").unwrap().is_none());
        test.read(|editor| {
            assert_eq!(editor.completion_choice().unwrap().title, "print");
            assert!(editor.completion_view().unwrap().rows[0].detail.is_none());
        });
    }
}
