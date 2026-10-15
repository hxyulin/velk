use rxui::{AppContext, Entity, Task, prelude::*};
use std::ops::Range;
use std::time::Duration;
use velk_editor_widget::{
    Annotations, Buffer, Color, Editor, EditorAppearance, HoverCard, LinePattern, OverlayKind,
    OverlayPlacement, SpanStyle, StyledRange, TextOverlay, Underline,
};

const SAMPLE: &str = "Velk / rich text playground\n\nForeground: coral   mint   sky   amber   lavender\nBackground: search match   active range   layered tint\nDecorations: underline   dashed line   wavy line   overline   strikethrough\nTypography: italic   bold   larger text\n\nfn greeting(name: &str) -> String {\n    let count = 42;\n    let message = format!(\"Hello, {name}\");\n    println!(\"{message}\");\n    unknown_call(message)\n}\n\nGhost completion: greeting\n\nUnicode: café   世界   👩‍💻   é\n";

pub struct Showcase {
    editor: Entity<Editor>,
    light: bool,
    decorations: bool,
    overlays: bool,
    inline: bool,
    refresh_task: Option<Task>,
}

impl Showcase {
    pub fn new(cx: &mut AppContext<'_>) -> Self {
        let editor = cx.new(|_| {
            let mut editor = Editor::new(SAMPLE);
            configure(&mut editor, false, true, true, true);
            editor
        });
        Self {
            editor,
            light: false,
            decorations: true,
            overlays: true,
            inline: true,
            refresh_task: None,
        }
    }

    fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        self.editor.update(cx, |editor, _| {
            configure(
                editor,
                self.light,
                self.decorations,
                self.overlays,
                self.inline,
            )
        });
        // The pinned native host can prepare input/accessibility before painting
        // and consume the dirty view. One deferred update publishes the refresh
        // after input dispatch; subsequent clicks cancel the previous task.
        self.refresh_task = cx
            .try_spawn(rxui::sleep(Duration::from_millis(16)), |_, _, _| {})
            .ok();
    }
}

impl View for Showcase {
    fn view(&self, cx: &mut ViewContext<'_, Self>) -> impl IntoElement {
        column().fill_width().fill_height().background(ThemeColor::Surface)
            .child(column().padding(16.).gap(8.)
                .child(label("Rich text playground").font_size(22.))
                .child(row().gap(8.)
                    .child(button("Reset sample").on_click(cx.listener(|this, _, cx| {
                        this.editor.update(cx, |editor, _| editor.set_text(SAMPLE));
                        this.refresh(cx);
                    })))
                    .child(button(if self.light { "Switch to dark" } else { "Switch to light" }).on_click(cx.listener(|this, _, cx| {
                        this.light = !this.light; this.refresh(cx);
                    })))
                    .child(button(if self.decorations { "Decorations: on" } else { "Decorations: off" }).on_click(cx.listener(|this, _, cx| {
                        this.decorations = !this.decorations; this.refresh(cx);
                    })))
                    .child(button(if self.overlays { "Overlays: on" } else { "Overlays: off" }).on_click(cx.listener(|this, _, cx| {
                        this.overlays = !this.overlays; this.refresh(cx);
                    })))
                    .child(button(if self.inline { "Inlays: inline" } else { "Inlays: line end" }).on_click(cx.listener(|this, _, cx| {
                        this.inline = !this.inline; this.refresh(cx);
                    })))
                    .child(button("Refresh annotations").on_click(cx.listener(|this, _, cx| this.refresh(cx)))))
                .child(label("Hover styles and hints. Edits clear annotations; Refresh annotations restores them.").font_size(12.).color(ThemeColor::TextMuted)))
            .child(self.editor.clone().into_element().flex_grow(1.).min_height(0.).fill_width())
    }
}

fn find(document: &Buffer, needle: &str) -> Option<Range<usize>> {
    let text = document.text().to_string();
    text.find(needle).map(|byte| {
        document.text().byte_to_char(byte)..document.text().byte_to_char(byte + needle.len())
    })
}

fn configure(editor: &mut Editor, light: bool, decorations: bool, overlays: bool, inline: bool) {
    editor
        .set_appearance(if light {
            EditorAppearance::light()
        } else {
            EditorAppearance::dark()
        })
        .unwrap();
    let document = editor.buffer();
    let mut annotations = Annotations {
        document_revision: document.revision(),
        ..Annotations::default()
    };
    let color = |dark: Color, light_color: Color| if light { light_color } else { dark };
    let coral = color([0.95, 0.24, 0.20, 1.], [0.50, 0.025, 0.02, 1.]);
    let mint = color([0.18, 0.82, 0.47, 1.], [0.015, 0.25, 0.09, 1.]);
    let sky = color([0.20, 0.58, 0.98, 1.], [0.015, 0.17, 0.50, 1.]);
    let amber = color([1., 0.63, 0.15, 1.], [0.42, 0.19, 0.005, 1.]);
    let lavender = color([0.65, 0.38, 0.95, 1.], [0.22, 0.05, 0.45, 1.]);
    if decorations {
        let mut add = |needle: &str, style: SpanStyle, title: &str, body: &str| {
            if let Some(range) = find(document, needle) {
                annotations
                    .spans
                    .push(StyledRange::new(range, style).with_hover(title, body));
            }
        };
        add(
            "Velk / rich text playground",
            SpanStyle {
                font_size: Some(24.),
                weight: Some(700),
                foreground: Some(sky),
                ..SpanStyle::default()
            },
            "Rich text",
            "Styled ranges can override font size, weight, slope, color, and paint decorations.",
        );
        for (needle, foreground) in [
            ("coral", coral),
            ("mint", mint),
            ("sky", sky),
            ("amber", amber),
            ("lavender", lavender),
        ] {
            add(
                needle,
                SpanStyle {
                    foreground: Some(foreground),
                    ..SpanStyle::default()
                },
                "Foreground color",
                "This glyph color is supplied by a rich text span, shaped and painted by Astrelis.",
            );
        }
        add(
            "search match",
            SpanStyle {
                background: Some([0.8, 0.45, 0.02, 0.3]),
                ..SpanStyle::default()
            },
            "Search result",
            "Background rectangles follow actual shaped text geometry.",
        );
        add(
            "active range",
            SpanStyle {
                background: Some([0.15, 0.5, 0.95, 0.25]),
                ..SpanStyle::default()
            },
            "Range highlight",
            "This visual range is independent of the editor selection.",
        );
        add(
            "layered tint",
            SpanStyle {
                foreground: Some(lavender),
                background: Some([0.55, 0.16, 0.8, 0.20]),
                ..SpanStyle::default()
            },
            "Layered style",
            "Foreground and background attributes can overlap other ranges.",
        );
        for (needle, pattern) in [
            ("underline", LinePattern::Solid),
            ("dashed line", LinePattern::Dashed),
            ("wavy line", LinePattern::Wavy),
        ] {
            add(
                needle,
                SpanStyle {
                    underline: Some(Underline {
                        color: coral,
                        pattern,
                    }),
                    ..SpanStyle::default()
                },
                "Underline style",
                "Solid, dashed, and wavy underlines are painted independently from glyph colors.",
            );
        }
        add(
            "overline",
            SpanStyle {
                overline: Some(sky),
                ..SpanStyle::default()
            },
            "Overline",
            "A custom paint decoration above the text range.",
        );
        add(
            "strikethrough",
            SpanStyle {
                strikethrough: Some(true),
                foreground: Some(coral),
                ..SpanStyle::default()
            },
            "Strikethrough",
            "Astrelis draws a strike using font metrics; the underlying text is unchanged.",
        );
        add(
            "italic",
            SpanStyle {
                italic: Some(true),
                foreground: Some(lavender),
                ..SpanStyle::default()
            },
            "Italic",
            "Font selection uses a real italic face when available.",
        );
        add(
            "bold",
            SpanStyle {
                weight: Some(700),
                ..SpanStyle::default()
            },
            "Bold",
            "Font weight is a shaping attribute.",
        );
        add(
            "larger text",
            SpanStyle {
                font_size: Some(22.),
                foreground: Some(amber),
                ..SpanStyle::default()
            },
            "Mixed sizes",
            "The tallest span determines this line's height. Caret and hit testing use the same layout.",
        );
        for needle in ["fn", "let count", "let message", "String", "&str"] {
            add(
                needle,
                SpanStyle {
                    foreground: Some(lavender),
                    ..SpanStyle::default()
                },
                "Mock syntax token",
                "This is a manually supplied token range; a future syntax provider can produce the same annotation data.",
            );
        }
        add(
            "greeting(name: &str)",
            SpanStyle {
                foreground: Some(sky),
                ..SpanStyle::default()
            },
            "fn greeting(name: &str) -> String",
            "Returns a greeting for the supplied name.\nMock LSP hover content; no language server is connected.",
        );
        add(
            "    let count = 42;",
            SpanStyle {
                background: Some([0.07, 0.65, 0.3, 0.12]),
                full_line_background: true,
                ..SpanStyle::default()
            },
            "Debugger current line",
            "A full-line background follows document geometry and viewport width.",
        );
        add(
            "42",
            SpanStyle {
                foreground: Some(amber),
                weight: Some(700),
                ..SpanStyle::default()
            },
            "count: i32",
            "Current mock debugger value: 42.",
        );
        add(
            "unknown_call",
            SpanStyle {
                underline: Some(Underline {
                    color: coral,
                    pattern: LinePattern::Wavy,
                }),
                ..SpanStyle::default()
            },
            "Unresolved function",
            "cannot find function `unknown_call` in this scope\nMock diagnostic: E0425.",
        );
    }
    if overlays {
        let mut add = |needle: &str,
                       text: &str,
                       kind: OverlayKind,
                       foreground: Color,
                       background: Option<Color>,
                       title: &str,
                       body: &str| {
            if let Some(range) = find(document, needle) {
                annotations.overlays.push(TextOverlay {
                    anchor: range.end,
                    text: text.into(),
                    kind,
                    placement: if inline
                        && matches!(kind, OverlayKind::InlayHint | OverlayKind::DebugValue)
                    {
                        OverlayPlacement::Inline
                    } else {
                        OverlayPlacement::LineEnd
                    },
                    foreground,
                    background,
                    hover: Some(HoverCard {
                        title: title.into(),
                        body: body.into(),
                    }),
                });
            }
        };
        add(
            "let count",
            ": i32",
            OverlayKind::InlayHint,
            sky,
            Some([0.10, 0.35, 0.70, 0.15]),
            "Mock LSP inlay hint",
            "The inferred type is i32. This badge is not part of the buffer and is not copied.",
        );
        add(
            "= 42",
            "value: 42",
            OverlayKind::DebugValue,
            mint,
            Some([0.04, 0.55, 0.20, 0.16]),
            "Debugger value",
            "A mock runtime value next to its expression. Inline placement reserves space without changing source text.",
        );
        add(
            "let message",
            ": String",
            OverlayKind::InlayHint,
            sky,
            Some([0.10, 0.35, 0.70, 0.15]),
            "Mock type information",
            "The expression produces an owned String.",
        );
        add(
            "unknown_call(",
            "message:",
            OverlayKind::InlayHint,
            sky,
            Some([0.10, 0.35, 0.70, 0.15]),
            "Mock parameter name hint",
            "The parameter name is displayed before the argument. Selecting or copying the call excludes the hint.",
        );
        add(
            "    unknown_call(message)",
            "E0425 · unresolved function",
            OverlayKind::Diagnostic,
            coral,
            Some([0.8, 0.08, 0.05, 0.13]),
            "Mock diagnostic",
            "A diagnostic text overlay accompanies the squiggle without modifying source text.",
        );
        add(
            "Ghost completion: greeting",
            "(\"world\")",
            OverlayKind::GhostText,
            color([0.30, 0.39, 0.52, 1.], [0.32, 0.38, 0.48, 1.]),
            None,
            "Ghost completion",
            "A visual suggestion at an end-of-line anchor. It is not inserted or accepted automatically.",
        );
        if let Some(overlay) = annotations.overlays.last_mut() {
            overlay.placement = OverlayPlacement::AtAnchor;
        }
    }
    editor
        .set_annotations(annotations)
        .expect("showcase produces valid current-revision ranges");
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::{ElementId, Runtime, TextMeasure, TextRequest, Ui, UiError};
    struct Measure;
    impl TextMeasure for Measure {
        fn measure(&mut self, _: ElementId, request: TextRequest<'_>) -> Result<[f32; 2], UiError> {
            Ok([request.text.chars().count() as f32 * 8., 24.])
        }
    }

    #[test]
    fn embedded_editor_fills_the_space_below_the_toolbar() {
        let mut runtime = Runtime::new();
        let showcase = runtime.update(|cx| cx.new(|cx| Showcase::new(cx)));
        let mut ui = Ui::new(&mut runtime, showcase).unwrap();
        ui.prepare(&mut runtime, [1280., 900.], &mut Measure)
            .unwrap();
        let editor = ui
            .elements()
            .find(|element| element.key == Some(&Key::from("editor-surface")))
            .unwrap();
        assert!(editor.bounds.y > 80.);
        assert!(
            editor.bounds.height > 600.,
            "embedded editor must not collapse to zero height"
        );
        assert_eq!(editor.bounds.width, 1280.);
    }

    #[test]
    fn child_editor_updates_remain_independent_of_toolbar_refreshes() {
        let mut runtime = Runtime::new();
        let showcase = runtime.update(|cx| cx.new(|cx| Showcase::new(cx)));
        let editor = runtime.update(|cx| showcase.read(cx).editor.clone());
        let mut ui = Ui::new(&mut runtime, showcase.clone()).unwrap();
        ui.prepare(&mut runtime, [1280., 900.], &mut Measure)
            .unwrap();
        runtime.update(|cx| {
            editor.update(cx, |editor, _| {
                editor.set_appearance(EditorAppearance::light()).unwrap()
            })
        });
        ui.prepare(&mut runtime, [1280., 900.], &mut Measure)
            .unwrap();
        let surface = ui
            .elements()
            .find(|e| e.key == Some(&Key::from("editor-surface")))
            .unwrap();
        assert_eq!(
            surface.background,
            Some(EditorAppearance::light().background)
        );
        runtime.update(|cx| {
            showcase.update(cx, |this, cx| {
                this.inline = false;
                this.refresh(cx);
            })
        });
        ui.prepare(&mut runtime, [1280., 900.], &mut Measure)
            .unwrap();
        let surface = ui
            .elements()
            .find(|e| e.key == Some(&Key::from("editor-surface")))
            .unwrap();
        assert_eq!(
            surface.background,
            Some(EditorAppearance::dark().background)
        );
        runtime.update(|cx| {
            assert!(
                editor
                    .read(cx)
                    .annotations()
                    .unwrap()
                    .overlays
                    .iter()
                    .all(|o| o.placement != OverlayPlacement::Inline)
            )
        });
    }

    #[test]
    fn showcase_ranges_remain_valid_for_both_appearances() {
        let mut editor = Editor::new(SAMPLE);
        for light in [false, true] {
            configure(&mut editor, light, true, true, true);
            let annotations = editor.annotations().unwrap();
            assert!(annotations.spans.len() > 20);
            assert_eq!(annotations.overlays.len(), 6);
            assert!(annotations.validate(editor.buffer()).is_ok());
        }
        editor.set_text("different content");
        configure(&mut editor, false, true, true, true);
        assert!(
            editor
                .annotations()
                .unwrap()
                .validate(editor.buffer())
                .is_ok()
        );
    }
}
