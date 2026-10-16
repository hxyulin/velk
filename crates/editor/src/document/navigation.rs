use super::Document;
use rxui::prelude::*;
use velk_editor_model::{EditCommand, navigation::Anchor};
pub(super) struct Back;
impl Command for Back {}
pub(super) struct Forward;
impl Command for Forward {}
impl Document {
    pub(super) fn navigation_anchor(&self, cx: &impl rxui::ReadContext) -> Option<Anchor> {
        self.buffer.as_ref().map(|b| Anchor {
            buffer: b.id,
            position: b.editor.read(cx).caret_position(),
        })
    }
    pub(super) fn navigate_history(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        if self.busy() || self.opening.is_some() || self.lifecycle.close_intent().is_some() {
            return;
        }
        let Some(current) = self.navigation_anchor(cx) else {
            return;
        };
        if self.buffer.editor.read(cx).has_composition() {
            return;
        }
        let ids = self.ids();
        self.navigation.retain(|id| ids.contains(&id));
        let next = if forward {
            self.navigation.forward(current)
        } else {
            self.navigation.back(current)
        };
        if let Some(next) = next {
            self.select_buffer(next.buffer, cx);
            if self.buffer.id == next.buffer {
                self.buffer.editor.update(cx, |editor, _| {
                    editor.execute(EditCommand::SetCaret {
                        position: next.position,
                        extend: false,
                    });
                });
                self.restore_focus_pending = true;
            }
        }
    }
}
