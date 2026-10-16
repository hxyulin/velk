use velk_core::Buffer;
use velk_editor_model::{
    AnnotationError, Annotations, Color, EditorAppearance, LinePattern, SpanStyle, StyledRange,
    Underline,
};

fn span(range: std::ops::Range<usize>) -> StyledRange {
    StyledRange::new(
        range,
        SpanStyle {
            foreground: Some([0.2, 0.6, 0.9, 1.]),
            ..SpanStyle::default()
        },
    )
}

#[test]
fn ranges_use_rope_char_offsets_and_require_grapheme_boundaries() {
    let document = Buffer::new("aé👩‍💻é");
    let mut annotations = Annotations {
        spans: vec![span(1..2), span(2..5), span(5..7)],
        ..Annotations::default()
    };
    assert!(annotations.validate(&document).is_ok());
    for invalid in [2..3, 5..6, 8..9, std::ops::Range { start: 4, end: 2 }] {
        annotations.spans = vec![span(invalid)];
        assert_eq!(
            annotations.validate(&document),
            Err(AnnotationError::InvalidRange)
        );
    }
}

#[test]
fn invalid_colors_and_font_metrics_cannot_reach_painting() {
    let document = Buffer::new("abc");
    for invalid in [f32::NAN, f32::INFINITY] {
        let annotations = Annotations {
            spans: vec![StyledRange::new(
                0..1,
                SpanStyle {
                    font_size: Some(invalid),
                    ..SpanStyle::default()
                },
            )],
            ..Annotations::default()
        };
        assert_eq!(
            annotations.validate(&document),
            Err(AnnotationError::InvalidFont)
        );
    }
    let annotations = Annotations {
        spans: vec![StyledRange::new(
            0..1,
            SpanStyle {
                underline: Some(Underline {
                    color: [0., 0., 0., 2.],
                    pattern: LinePattern::Wavy,
                }),
                ..SpanStyle::default()
            },
        )],
        ..Annotations::default()
    };
    assert_eq!(
        annotations.validate(&document),
        Err(AnnotationError::InvalidColor)
    );
    let bad: Color = [f32::NAN, 0., 0., 1.];
    assert_eq!(
        EditorAppearance {
            background: bad,
            ..EditorAppearance::dark()
        }
        .validate(),
        Err(AnnotationError::InvalidColor)
    );
}
