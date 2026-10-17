use crate::Editor;
use rxui::{Element, prelude::*};
use velk_commands::{completion::Target, signature::Help};
#[derive(Default)]
pub(super) struct State {
    help: Option<(Target, Help)>,
    active: bool,
    epoch: u64,
}
impl Editor {
    pub fn signature_epoch(&self) -> u64 {
        self.signature.epoch
    }
    pub fn signature_active(&self) -> bool {
        self.signature.active
    }
    pub fn signature_refresh(&self) -> bool {
        self.signature.active
            && self.signature.help.as_ref().is_none_or(|(target, _)| {
                target.revision != self.buffer.revision()
                    || target.position != self.caret_position()
            })
    }
    pub fn dismiss_signature(&mut self) {
        self.signature = State {
            epoch: self.signature.epoch + 1,
            ..Default::default()
        };
    }
    pub fn show_signature(&mut self, help: Option<Help>) {
        self.hide_hover();
        if let Some(help) = help.filter(|help| help.validate().is_ok()) {
            self.signature.active = true;
            self.signature.help = Some((
                Target {
                    revision: self.buffer.revision(),
                    position: self.caret_position(),
                },
                help,
            ));
        } else {
            self.dismiss_signature();
        }
    }
    pub(super) fn signature_key(&mut self, event: &KeyInput) -> bool {
        if event.event.key == KeyboardKey::Escape {
            let active = self.signature.active;
            self.dismiss_signature();
            if active {
                event.prevent_default();
            }
            return active && !self.completion_active();
        }
        if self.signature.active
            && event.event.modifiers.alt
            && matches!(
                event.event.key,
                KeyboardKey::ArrowUp | KeyboardKey::ArrowDown
            )
        {
            if let Some((_, help)) = &mut self.signature.help {
                let count = help.signatures.len();
                help.active = (help.active
                    + if event.event.key == KeyboardKey::ArrowUp {
                        count - 1
                    } else {
                        1
                    })
                    % count;
            }
            event.prevent_default();
            return true;
        }
        false
    }
    pub(super) fn signature_popup(&self, cx: &mut ViewContext<'_, Self>) -> Option<Element> {
        if self.snippet_choices().is_some()
            || self.completion_active()
            || self.has_hover()
            || self.has_composition()
            || self.signature_refresh()
        {
            return None;
        }
        let (_, help) = self.signature.help.as_ref()?;
        let signature = &help.signatures[help.active];
        let geometry = self.geometry.borrow();
        let caret = geometry.caret_area();
        let anchor = [
            geometry.origin[0] + caret.x,
            geometry.origin[1] + caret.y + caret.height,
        ];
        let mut content = column().gap(6.).child(
            label(
                signature
                    .label
                    .lines()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .key("signature-label")
            .font_size(13.),
        );
        if let Some(range) = &signature.active_parameter {
            let parameter: String = signature
                .label
                .chars()
                .skip(range.start)
                .take(range.len())
                .collect();
            content = content.child(label(parameter).font_size(13.).color(ThemeColor::Accent));
        }
        if let Some(documentation) = &signature.documentation {
            content = content.child(
                label(
                    documentation
                        .text
                        .lines()
                        .take(6)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .font_size(12.)
                .color(ThemeColor::TextMuted),
            );
        }
        if help.signatures.len() > 1 {
            content = content.child(
                label(format!(
                    "{} / {} · Alt+Up/Down",
                    help.active + 1,
                    help.signatures.len()
                ))
                .font_size(11.)
                .color(ThemeColor::TextMuted),
            );
        }
        Some(
            popover(anchor, content.max_height(320.).clip())
                .key("editor-signature")
                .on_dismiss(cx.listener(|this, _: &DismissEvent, _| this.dismiss_signature()))
                .width((geometry.viewport[0] - 24.).clamp(120., 600.))
                .autofocus(false)
                .transition(rxui::Transition::None)
                .into_element(),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_hide_stale_help_and_dismissal_changes_epoch() {
        let mut editor = Editor::new("call(");
        editor.select_range(5..5);
        editor.show_signature(Some(Help {
            signatures: vec![velk_commands::signature::Signature {
                label: "call(arg)".into(),
                active_parameter: Some(5..8),
                documentation: None,
            }],
            active: 0,
        }));
        assert!(!editor.signature_refresh());
        editor.insert("value");
        assert!(editor.signature_refresh());
        let epoch = editor.signature_epoch();
        editor.dismiss_signature();
        assert!(!editor.signature_active());
        assert_ne!(epoch, editor.signature_epoch());
    }
}
