//! Velk's application palette. Installed as an explicit RXUI theme, so system
//! accent changes cannot recolor editor controls or documentation.
use rxui::{Theme, rgb8};
use velk_editor_widget::EditorAppearance;

pub const HOVER: &str = "hover";
pub const WARNING: &str = "warning";
pub const ERROR: &str = "error";

// Neutral graphite: chrome must not fight the editor colorscheme.
pub fn dark() -> Theme {
    let editor = EditorAppearance::dark();
    Theme::dark()
        .compact()
        .colors(|c| {
            c.background = editor.background;
            c.surface = rgb8(0x17, 0x1A, 0x1F);
            c.raised = rgb8(0x23, 0x27, 0x2E);
            c.control = rgb8(0x23, 0x27, 0x2E);
            c.control_hover = rgb8(0x2A, 0x2F, 0x37);
            c.control_pressed = rgb8(0x36, 0x3B, 0x45);
            c.control_disabled = rgb8(0x1F, 0x22, 0x28);
            c.text = editor.foreground;
            c.text_muted = rgb8(0x8A, 0x90, 0x9A);
            c.text_disabled = rgb8(0x5A, 0x60, 0x6B);
            c.border = rgb8(0x36, 0x3B, 0x45);
            c.border_disabled = rgb8(0x2A, 0x2E, 0x36);
            c.input = editor.background;
            c.input_border = rgb8(0x4A, 0x50, 0x5C);
            c.divider = rgb8(0x2A, 0x2E, 0x36);
            c.accent = rgb8(0x8C, 0xB4, 0xD8);
            c.accent_hover = rgb8(0xA0, 0xC2, 0xE0);
            c.accent_pressed = rgb8(0x76, 0x9F, 0xC4);
            c.accent_text = rgb8(0x14, 0x17, 0x1C);
            c.focus = rgb8(0x8C, 0xB4, 0xD8);
            c.selection = rgb8(0x2F, 0x45, 0x5C);
            c.selection_text = editor.foreground;
            c.caret = editor.foreground;
            c.preedit = editor.foreground;
        })
        .define_color(HOVER, rgb8(0x2A, 0x2F, 0x37))
        .define_color(WARNING, rgb8(0xD9, 0xB2, 0x6F))
        .define_color(ERROR, rgb8(0xDE, 0x8A, 0x82))
        .metrics(|m| {
            m.focus_width = 1.;
            m.radius = 6.;
        })
}
