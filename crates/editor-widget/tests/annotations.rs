use velk_editor_widget::{
    AnnotationError, Annotations, Buffer, Color, Editor, EditorAppearance, HoverCard, LinePattern,
    OverlayKind, OverlayPlacement, SpanStyle, StyledRange, TextOverlay, Underline,
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
fn stale_provider_results_are_rejected_without_replacing_current_annotations() {
    let mut editor = Editor::new("abc");
    editor
        .set_annotations(Annotations {
            spans: vec![span(0..1)],
            ..Annotations::default()
        })
        .unwrap();
    editor.insert("x");
    let current = Annotations {
        document_revision: editor.buffer().revision(),
        spans: vec![span(0..1)],
        ..Annotations::default()
    };
    editor.set_annotations(current.clone()).unwrap();
    assert_eq!(
        editor.set_annotations(Annotations::default()),
        Err(AnnotationError::StaleRevision {
            expected: 1,
            received: 0
        })
    );
    assert_eq!(editor.annotations(), Some(&current));
}

#[test]
fn document_edits_expire_annotations_but_appearance_changes_do_not() {
    let mut editor = Editor::new("abc");
    editor
        .set_annotations(Annotations {
            spans: vec![span(0..3)],
            ..Annotations::default()
        })
        .unwrap();
    editor.set_appearance(EditorAppearance::light()).unwrap();
    assert!(editor.annotations().is_some());
    editor.insert("x");
    assert!(editor.annotations().is_none());
    assert_eq!(editor.buffer().text().to_string(), "xabc");
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

#[test]
fn overlay_anchors_are_validated_and_overlays_never_change_source_text() {
    let mut editor = Editor::new("let count = 42;");
    let overlay = TextOverlay {
        anchor: 14,
        text: "count = 42".into(),
        kind: OverlayKind::DebugValue,
        placement: OverlayPlacement::LineEnd,
        foreground: [0.1, 0.8, 0.2, 1.],
        background: None,
        hover: Some(HoverCard {
            title: "Debugger".into(),
            body: "An external runtime value".into(),
        }),
    };
    let mut annotations = Annotations {
        overlays: vec![overlay],
        ..Annotations::default()
    };
    editor.set_annotations(annotations.clone()).unwrap();
    assert_eq!(editor.buffer().text().to_string(), "let count = 42;");
    annotations.overlays[0].anchor = usize::MAX;
    assert_eq!(
        annotations.validate(editor.buffer()),
        Err(AnnotationError::InvalidRange)
    );
    annotations.overlays[0].anchor = 14;
    annotations.overlays[0].text = "two\nlines".into();
    assert_eq!(
        annotations.validate(editor.buffer()),
        Err(AnnotationError::InvalidOverlayText)
    );
}

#[test]
fn resetting_the_document_is_an_undoable_edit_and_expires_provider_results() {
    let mut editor = Editor::new("original");
    editor
        .set_annotations(Annotations {
            spans: vec![span(0..8)],
            ..Annotations::default()
        })
        .unwrap();
    editor.set_text("replacement");
    assert!(editor.annotations().is_none());
    assert!(editor.buffer().can_undo());
    assert_eq!(editor.buffer().revision(), 1);
    assert_eq!(editor.buffer().text().to_string(), "replacement");
}
