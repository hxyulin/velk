use rxui::astrelis::{FontSlant, TextSpan};
use std::ops::Range;
use velk_editor_model::SpanStyle;

pub(crate) fn text_span(style: &SpanStyle, range: Range<usize>) -> Option<TextSpan> {
    if style.foreground.is_none()
        && style.weight.is_none()
        && style.italic.is_none()
        && style.font_size.is_none()
        && style.strikethrough.is_none()
    {
        return None;
    }
    Some(TextSpan {
        range,
        color: style.foreground,
        weight: style.weight,
        slant: style.italic.map(|italic| {
            if italic {
                FontSlant::Italic
            } else {
                FontSlant::Normal
            }
        }),
        font_size: style.font_size,
        strikethrough: style.strikethrough,
        ..TextSpan::default()
    })
}
