//! Native text styling shared by documentation surfaces; parsing stays in providers.
use rxui::{Element, prelude::*};
pub(super) fn body(document: &velk_markup::Document) -> Element {
    rxui::rich_text(spans(document))
}
pub(super) fn spans(document: &velk_markup::Document) -> Vec<rxui::TextSpan> {
    document
        .runs
        .iter()
        .map(|run| {
            let mut span = rxui::span(run.text.clone());
            let style = &run.style;
            if style.bold {
                span = span.bold();
            }
            if style.italic {
                span = span.italic();
            }
            if style.code {
                span = span.monospace().color(ThemeColor::Text);
            }
            if style.strikethrough {
                span = span.strikethrough();
            }
            if style.heading > 0 {
                span = span.font_size(19. - f32::from(style.heading.min(6)));
            }
            if style.link.is_some() {
                span = span.underline().color(ThemeColor::Accent);
            }
            span
        })
        .collect()
}
