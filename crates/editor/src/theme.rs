//! Velk's application palette. Installed as an explicit RXUI theme, so system
//! accent changes cannot recolor editor controls or documentation.
use rxui::{Theme, rgb8};
use velk_editor_widget::EditorAppearance;

pub fn dark() -> Theme {
    let editor = EditorAppearance::dark();
    Theme::dark()
        .colors(|c| {
            c.background = editor.background;
            c.surface = rgb8(28, 39, 54);
            c.raised = rgb8(32, 45, 62);
            c.control = rgb8(38, 52, 71);
            c.control_hover = rgb8(47, 64, 86);
            c.control_pressed = rgb8(56, 75, 99);
            c.control_disabled = rgb8(29, 40, 55);
            c.text = editor.foreground;
            c.text_muted = rgb8(155, 169, 188);
            c.text_disabled = rgb8(101, 116, 136);
            c.border = rgb8(59, 74, 95);
            c.border_disabled = rgb8(40, 54, 72);
            c.input = editor.background;
            c.input_border = rgb8(82, 101, 126);
            c.divider = rgb8(48, 63, 83);
            c.accent = rgb8(122, 162, 247);
            c.accent_hover = rgb8(145, 179, 250);
            c.accent_pressed = rgb8(100, 141, 224);
            c.accent_text = editor.background;
            c.focus = rgb8(130, 147, 170);
            c.selection = rgb8(51, 79, 119);
            c.selection_text = editor.foreground;
            c.caret = editor.foreground;
            c.preedit = editor.foreground;
        })
        .metrics(|m| m.focus_width = 1.)
}
