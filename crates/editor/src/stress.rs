use velk_editor_widget::{
    Annotations, Editor, OverlayKind, OverlayPlacement, SpanStyle, StyledRange, TextOverlay,
};

/// Deterministic 2.5 MB document with one style and inline hint on every line.
pub fn editor() -> Editor {
    const LINES: usize = 100_000;
    const LINE: &str = "let count = 42; // value\n";
    let mut editor = Editor::new(&LINE.repeat(LINES));
    let mut annotations = Annotations::default();
    for line in 0..LINES {
        let start = line * LINE.len();
        annotations.spans.push(StyledRange::new(
            start..start + 3,
            SpanStyle {
                foreground: Some([0.3, 0.6, 0.9, 1.]),
                weight: Some(700),
                ..SpanStyle::default()
            },
        ));
        annotations.overlays.push(TextOverlay {
            anchor: start + 9,
            text: ": i32".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [0.3, 0.7, 1., 1.],
            background: Some([0.1, 0.35, 0.7, 0.15]),
            hover: None,
        });
    }
    editor.set_annotations(annotations).unwrap();
    editor
}
