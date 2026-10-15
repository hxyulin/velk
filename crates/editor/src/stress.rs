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

/// Long fixed-grid lines for native horizontal scrolling and caret checks.
pub fn horizontal_editor() -> Editor {
    let line = (0..10_000)
        .map(|index| format!("{index:09} "))
        .collect::<String>();
    let source = format!(
        "{line}\n{line}\nUnicode fallback: café 世界 👩‍💻\nUse Home/End or Command+Left/Right to reveal the full line.\n"
    );
    let mut editor = Editor::new(&source);
    let mut annotations = Annotations::default();
    for start in (0..line.len()).step_by(1000) {
        annotations.spans.push(StyledRange::new(
            line.len() + 1 + start..line.len() + 1 + start + 9,
            SpanStyle {
                foreground: Some([0.3, 0.8, 1., 1.]),
                ..SpanStyle::default()
            },
        ));
    }
    editor.set_annotations(annotations).unwrap();
    editor
}

/// Mixed metrics and inline labels across one long logical line.
pub fn rich_horizontal_editor() -> Editor {
    const PART: &str = "let value = call(); ";
    let line = PART.repeat(1000);
    let mut editor = Editor::new(&format!(
        "{line}\nUnicode fallback: café 世界 👩‍💻\nUse Command+Right to reveal the tail; hover the inline labels.\n"
    ));
    let mut annotations = Annotations::default();
    annotations.spans.push(StyledRange::new(
        0..line.len(),
        SpanStyle {
            background: Some([0.02, 0.16, 0.12, 1.]),
            full_line_background: true,
            ..SpanStyle::default()
        },
    ));
    for i in 0..1000 {
        let start = i * PART.len();
        annotations.spans.push(StyledRange::new(
            start..start + 3,
            SpanStyle {
                foreground: Some([0.4, 0.7, 1., 1.]),
                weight: Some(700),
                font_size: if i == 0 { Some(32.) } else { None },
                ..SpanStyle::default()
            },
        ));
        annotations.overlays.push(TextOverlay {
            anchor: start + 9,
            text: ": i32".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [0.5, 0.8, 1., 1.],
            background: Some([0.1, 0.3, 0.4, 0.4]),
            hover: Some(velk_editor_widget::HoverCard {
                title: "Type hint".into(),
                body: "Source text remains unchanged by this inline label.".into(),
            }),
        });
    }
    annotations.overlays.push(TextOverlay {
        anchor: line.len(),
        text: " // done".into(),
        kind: OverlayKind::GhostText,
        placement: OverlayPlacement::Inline,
        foreground: [0.6, 0.7, 0.8, 1.],
        background: None,
        hover: None,
    });
    editor.set_annotations(annotations).unwrap();
    editor
}
