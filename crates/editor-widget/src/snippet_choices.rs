use crate::Editor;
use rxui::{Element, prelude::*};
impl Editor {
    pub fn snippet_choices(&self) -> Option<(Vec<String>, usize)> {
        self.editing.snippet_choices(&self.buffer)
    }
    pub fn choose_snippet(&mut self, index: usize) -> Result<(), String> {
        self.editing.choose_snippet(&mut self.buffer, index)?;
        self.edited();
        Ok(())
    }
    pub(super) fn snippet_choice_key(&mut self, event: &KeyInput) -> bool {
        let Some((choices, selected)) = self.snippet_choices() else {
            return false;
        };
        let modifiers = event.event.modifiers;
        if modifiers.alt || modifiers.meta || modifiers.control || modifiers.shift {
            return false;
        }
        match event.event.key {
            KeyboardKey::ArrowDown | KeyboardKey::ArrowUp => {
                let next = (selected
                    + if event.event.key == KeyboardKey::ArrowUp {
                        choices.len() - 1
                    } else {
                        1
                    })
                    % choices.len();
                if let Err(error) = self.choose_snippet(next) {
                    tracing::warn!(%error, "snippet choice failed");
                }
            }
            KeyboardKey::Enter => {
                if self.choose_snippet(selected).is_ok() {
                    self.editing.close_snippet_choices();
                }
            }
            _ => return false,
        }
        event.prevent_default();
        true
    }
    pub(super) fn snippet_choices_popup(&self, cx: &mut ViewContext<'_, Self>) -> Option<Element> {
        let (choices, selected) = self.snippet_choices()?;
        let geometry = self.geometry.borrow();
        let caret = geometry.caret_area();
        let anchor = [
            geometry.origin[0] + caret.x,
            geometry.origin[1] + caret.y + caret.height,
        ];
        let revision = self.buffer.revision();
        let selection = self.buffer.selection();
        let expected = std::sync::Arc::new(choices.clone());
        let mut content = column()
            .gap(0.)
            .accessibility_role(SemanticRole::ListBox)
            .accessibility_label("Snippet choices");
        for (index, choice) in choices
            .iter()
            .enumerate()
            .skip(selected.saturating_sub(3))
            .take(8)
        {
            let expected = expected.clone();
            content = content.child(
                label(if choice.is_empty() {
                    "(empty)".into()
                } else {
                    choice
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect::<String>()
                })
                .font_size(13.)
                .padding(6.)
                .clip()
                .accessibility_role(SemanticRole::ListBoxOption)
                .accessibility_selected(index == selected)
                .background(if index == selected {
                    ThemeColor::Selection
                } else {
                    ThemeColor::Surface
                })
                .on_pointer_down(cx.listener(
                    move |this, event: &PointerInput, _| {
                        if this.buffer.revision() != revision
                            || this.buffer.selection() != selection
                            || this.snippet_choices().is_none_or(|(choices, _)| {
                                choices.as_slice() != expected.as_slice()
                            })
                        {
                            return;
                        }
                        if this.choose_snippet(index).is_ok() {
                            this.editing.close_snippet_choices();
                        }
                        event.prevent_default();
                    },
                )),
            );
        }
        Some(
            popover(anchor, content)
                .key("snippet-choices")
                .width((geometry.viewport[0] - 24.).clamp(120., 360.))
                .padding(0.)
                .gap(0.)
                .autofocus(false)
                .transition(rxui::Transition::None)
                .on_dismiss(
                    cx.listener(|this, _: &DismissEvent, _| this.editing.close_snippet_choices()),
                )
                .into_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snippet(source: &str) -> Editor {
        let expansion = velk_commands::snippet::expand(source).unwrap();
        let mut editor = Editor::new(&expansion.text);
        editor.start_snippet(expansion.stops).unwrap();
        editor
    }
    #[test]
    fn choosing_updates_mirrors_typing_hides_choices_and_transform_updates_on_tab() {
        let mut editor =
            snippet("${1|red,green,blue|} = $1 => ${1/(.*)/${1:/upcase}/}; ${2:next}$0");
        assert_eq!(editor.snippet_choices().unwrap().1, 0);
        editor.choose_snippet(1).unwrap();
        assert_eq!(
            editor.buffer().text().to_string(),
            "green = green => RED; next"
        );
        assert_eq!(editor.selected_text(), "green\ngreen");
        assert_eq!(editor.snippet_choices().unwrap().1, 1);
        editor.committed_text("世界", rxui::TextInsertSource::Keyboard);
        assert!(editor.snippet_choices().is_none());
        assert_eq!(
            editor.buffer().text().to_string(),
            "世界 = 世界 => RED; next"
        );
        editor.navigate_snippet(false);
        assert_eq!(
            editor.buffer().text().to_string(),
            "世界 = 世界 => 世界; next"
        );
        assert_eq!(editor.selected_text(), "next");
        editor.navigate_snippet(true);
        assert_eq!(editor.selected_text(), "世界\n世界");
        assert!(editor.snippet_choices().is_some());
        editor.choose_snippet(2).unwrap();
        editor.navigate_snippet(false);
        assert_eq!(
            editor.buffer().text().to_string(),
            "blue = blue => BLUE; next"
        );
        editor.execute(crate::EditCommand::Undo);
        assert!(!editor.snippet_active());
        assert_eq!(
            editor.buffer().text().to_string(),
            "blue = blue => 世界; next"
        );
    }
    #[test]
    fn transforms_map_multiple_derived_occurrences_and_empty_capture_results() {
        let mut editor = snippet(
            "${1:hello_world} ${1/(.*)/${1:/pascalcase}/} ${1/(.*)/${1:/upcase}/} ${2:end}$0",
        );
        editor.committed_text("some_value", rxui::TextInsertSource::Keyboard);
        editor.navigate_snippet(false);
        assert_eq!(
            editor.buffer().text().to_string(),
            "some_value SomeValue SOME_VALUE end"
        );
        assert_eq!(editor.selected_text(), "end");
        editor.navigate_snippet(true);
        editor.committed_text("", rxui::TextInsertSource::Keyboard);
        editor.execute(crate::EditCommand::Backspace);
        editor.navigate_snippet(false);
        assert_eq!(editor.buffer().text().to_string(), "   end");
        assert_eq!(editor.selected_text(), "end");
    }
    #[test]
    fn transform_limit_failure_keeps_the_entire_derived_batch_unchanged() {
        let source = format!(
            "${{1:a}} ${{1/(.*)/${{1:/upcase}}/}} ${{1/./{}/g}}$0",
            "x".repeat(4096)
        );
        let mut editor = snippet(&source);
        editor.committed_text(&"b".repeat(32), rxui::TextInsertSource::Keyboard);
        let before = editor.buffer().text().to_string();
        assert!(editor.navigate_snippet(false));
        assert!(!editor.snippet_active());
        assert_eq!(editor.buffer().text().to_string(), before);
    }
    #[test]
    fn keyboard_choices_render_and_enter_confirms_without_inserting_a_newline() {
        let graphics = pollster::block_on(rxui::astrelis::GraphicsContext::headless()).unwrap();
        let editor = snippet("${1|one,two,three|}$0");
        let mut test = rxui::TestUi::new(&graphics, [640, 240], editor).unwrap();
        test.painter_mut().fonts_mut().load_system_fonts();
        let surface = test.element("editor-surface").unwrap().unwrap().id;
        test.parts().1.focus(surface);
        assert!(
            test.element("snippet-choices")
                .unwrap()
                .unwrap()
                .bounds
                .height
                > 20.
        );
        for key in [KeyboardKey::ArrowDown, KeyboardKey::Enter] {
            test.key(rxui::KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            })
            .unwrap();
        }
        test.read(|editor| {
            assert_eq!(editor.buffer().text().to_string(), "two");
            assert!(editor.snippet_active());
            assert!(editor.snippet_choices().is_none());
        });
        assert!(test.element("snippet-choices").unwrap().is_none());
        test.key(rxui::KeyEvent {
            key: KeyboardKey::Tab,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        })
        .unwrap();
        test.read(|editor| assert!(!editor.snippet_active()));
    }
}
