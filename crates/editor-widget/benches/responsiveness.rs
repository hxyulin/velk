//! Dispatch-to-submission CPU timings through the real RXUI/widget pipeline.
//! Offscreen rendering excludes native event delivery, surface waits and scanout.
use rxui::{
    KeyEvent, KeyboardKey, Modifiers, TestUi, TextInputEvent,
    astrelis::{Framebuffer, FramebufferOptions, GraphicsContext, wgpu},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use velk_editor_widget::{
    Annotations, Editor, OverlayKind, OverlayPlacement, SpanStyle, StyledRange, TextOverlay,
};

fn key(test: &mut TestUi<Editor>, key: KeyboardKey) {
    test.key(KeyEvent {
        key,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    })
    .unwrap();
}

fn frame(test: &mut TestUi<Editor>, target: &mut Framebuffer, clock: Instant) {
    test.runtime().poll_tasks();
    test.runtime().begin_frame(clock.elapsed());
    test.prepare().unwrap();
    let (_, ui, painter) = test.parts();
    painter.prepare(ui, &target.render_format(), 1.).unwrap();
    // Native RXUI publishes the candidate rectangle at this same boundary.
    std::hint::black_box(ui.ime_cursor_area(painter).unwrap());
    let mut frame = target.begin_frame().unwrap();
    painter
        .compose(ui, &mut frame, 1., |frame, composed| {
            let mut pass = frame
                .render_pass()
                .clear_color(wgpu::Color::BLACK)
                .begin()?;
            composed.paint(&mut pass)
        })
        .unwrap();
    frame.finish().unwrap();
}

fn rich() -> Editor {
    let mut editor = Editor::new(&"let value = call(); ".repeat(1000));
    let mut annotations = Annotations::default();
    for i in 0..1000 {
        let start = i * 20;
        annotations.spans.push(StyledRange::new(
            start..start + 3,
            SpanStyle {
                weight: Some(700),
                ..SpanStyle::default()
            },
        ));
        annotations.overlays.push(TextOverlay {
            anchor: start + 9,
            text: ": i32".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [1.; 4],
            background: None,
            hover: None,
        });
    }
    editor.set_annotations(annotations).unwrap();
    editor
}

fn main() {
    let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
    let clock = Instant::now();
    let wait_for_gpu = std::env::var_os("VELK_BENCH_GPU_WAIT").is_some();
    println!(
        "case,sample,event_ms,first_submit_ms,settled_ms,frames,gpu_done_ms,cached_source_bytes,composition_fallback,visible_lines,gpu_prepared_lines"
    );
    for case in [
        "plain_navigation",
        "rich_navigation",
        "large_document_typing",
        "rust_plain_typing",
        "rust_syntax_typing",
        "long_line_unicode_typing",
        "ime_preedit",
        "large_document_ime",
        "long_line_ime",
        "long_line_emoji_ime",
        "long_line_rtl_ime",
        "multiline_replacement_ime",
        "unicode_island_navigation",
    ] {
        if std::env::var("VELK_BENCH_CASE").is_ok_and(|filter| filter != case) {
            continue;
        }
        let editor = match case {
            "plain_navigation" => Editor::new(&"x".repeat(100_000)),
            "rich_navigation" => rich(),
            "rust_plain_typing" | "rust_syntax_typing" => {
                Editor::new(&"fn example() { let count = 42; } // value\n".repeat(100_000))
            }
            "large_document_typing" => Editor::new(&"let count = 42; // value\n".repeat(100_000)),
            "long_line_unicode_typing" => Editor::new(&format!(
                "{}日本語 café é 👩‍💻 🇭🇰",
                " abcdefghij".repeat(10_000)
            )),
            "large_document_ime" => Editor::new(&"let count = 42; // value\n".repeat(10_000)),
            "long_line_ime" | "long_line_emoji_ime" | "long_line_rtl_ime" => {
                Editor::new(&"x".repeat(100_000))
            }
            "multiline_replacement_ime" => {
                Editor::new(&"let count = 42; // value\n".repeat(10_000))
            }
            "unicode_island_navigation" => Editor::new(&format!(
                "{}日本語 👩‍💻{}",
                "x".repeat(5000),
                "y".repeat(4000)
            )),
            _ => Editor::new("Unicode: café 世界 👩‍💻\n"),
        };
        let mut test = TestUi::new(&graphics, [1200, 800], editor).unwrap();
        if matches!(case, "rust_plain_typing" | "rust_syntax_typing") {
            test.runtime()
                .configure_tasks(
                    Arc::new(rxui::ThreadPoolExecutor::new(2, 2).unwrap()),
                    || {},
                )
                .unwrap();
            if case == "rust_syntax_typing" {
                let entity = test.entity().clone();
                test.runtime().update(|cx| {
                    entity.update(cx, |editor, _| {
                        editor
                            .set_syntax_provider(Some(Arc::new(
                                velk_plugin_tree_sitter::TreeSitter,
                            )))
                            .unwrap();
                        editor
                            .set_language(Some(velk_editor_widget::Language::new("rust")))
                            .unwrap()
                    })
                });
            }
        }
        test.painter_mut().fonts_mut().load_system_fonts();
        test.prepare().unwrap();
        let id = test.element("editor-surface").unwrap().unwrap().id;
        test.parts().1.focus(id);
        let mut target = graphics
            .create_framebuffer(FramebufferOptions::new(1200, 800))
            .unwrap();
        frame(&mut test, &mut target, clock);
        while test.runtime().animation_frame_requested() {
            frame(&mut test, &mut target, clock);
        }
        if matches!(
            case,
            "long_line_ime"
                | "long_line_emoji_ime"
                | "long_line_rtl_ime"
                | "long_line_unicode_typing"
        ) {
            key(&mut test, KeyboardKey::End);
            frame(&mut test, &mut target, clock);
        }
        if case == "multiline_replacement_ime" {
            for _ in 0..1000 {
                test.key(KeyEvent {
                    key: KeyboardKey::ArrowDown,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    },
                })
                .unwrap();
            }
            frame(&mut test, &mut target, clock);
        }
        if case == "rust_syntax_typing" {
            let deadline = Instant::now() + Duration::from_secs(10);
            while test.read(|editor| editor.syntax_revision().is_none()) {
                test.runtime().poll_tasks();
                assert!(Instant::now() < deadline, "syntax warmup timed out");
                std::thread::sleep(Duration::from_millis(1));
            }
            frame(&mut test, &mut target, clock);
        }
        // Warm up the event path and both window ends before collecting samples.
        let (warmup, samples) = if matches!(
            case,
            "large_document_ime"
                | "long_line_ime"
                | "long_line_emoji_ime"
                | "long_line_rtl_ime"
                | "multiline_replacement_ime"
        ) {
            (2, 21)
        } else {
            (20, 201)
        };
        let warmup = std::env::var("VELK_BENCH_WARMUP")
            .ok()
            .map(|v| v.parse::<usize>().expect("numeric warmup count"))
            .unwrap_or(warmup);
        let samples = std::env::var("VELK_BENCH_SAMPLES")
            .ok()
            .map(|v| v.parse::<usize>().expect("numeric sample count"))
            .unwrap_or(samples);
        assert!(samples > 0);
        for i in 0..warmup + samples {
            let start = Instant::now();
            match case {
                "plain_navigation" | "rich_navigation" | "unicode_island_navigation" => key(
                    &mut test,
                    if i % 2 == 0 {
                        KeyboardKey::End
                    } else {
                        KeyboardKey::Home
                    },
                ),
                "long_line_unicode_typing" if i % 2 == 0 => {
                    test.text_input(TextInputEvent::Insert("é".into())).unwrap();
                }
                "long_line_unicode_typing" => key(&mut test, KeyboardKey::Backspace),
                "large_document_typing" | "rust_plain_typing" | "rust_syntax_typing"
                    if i % 2 == 0 =>
                {
                    test.text_input(TextInputEvent::Insert("x".into())).unwrap();
                }
                "large_document_typing" | "rust_plain_typing" | "rust_syntax_typing" => {
                    key(&mut test, KeyboardKey::Backspace)
                }
                "large_document_ime" | "long_line_ime" | "multiline_replacement_ime" => {
                    test.text_input(TextInputEvent::Preedit {
                        text: if i % 2 == 0 { "かんじ" } else { "にほん" }.into(),
                        cursor: Some((0, 9)),
                    })
                    .unwrap();
                }
                "long_line_emoji_ime" => {
                    let text = if i % 2 == 0 {
                        "👩‍💻"
                    } else {
                        "👨‍💻"
                    };
                    test.text_input(TextInputEvent::Preedit {
                        text: text.into(),
                        cursor: Some((0, text.len())),
                    })
                    .unwrap();
                }
                "long_line_rtl_ime" => {
                    let text = if i % 2 == 0 {
                        "العربية"
                    } else {
                        "עברית"
                    };
                    test.text_input(TextInputEvent::Preedit {
                        text: text.into(),
                        cursor: Some((0, text.len())),
                    })
                    .unwrap();
                }
                _ if i % 2 == 0 => {
                    test.text_input(TextInputEvent::Preedit {
                        text: "かんじ".into(),
                        cursor: Some((0, 9)),
                    })
                    .unwrap();
                }
                _ => {
                    test.text_input(TextInputEvent::CancelComposition).unwrap();
                }
            }
            let event = start.elapsed();
            frame(&mut test, &mut target, clock);
            let first = start.elapsed();
            let mut frames = 1;
            while test.runtime().animation_frame_requested() {
                assert!(frames < 4, "editor failed to settle");
                frame(&mut test, &mut target, clock);
                frames += 1;
            }
            let settled = start.elapsed();
            let gpu_done = if wait_for_gpu {
                graphics
                    .device()
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(Duration::from_secs(10)),
                    })
                    .unwrap();
                format!("{:.6}", start.elapsed().as_secs_f64() * 1000.)
            } else {
                String::new()
            };
            if i >= warmup {
                let metrics = test.read(|e| e.frame_metrics());
                let ms = |d: Duration| d.as_secs_f64() * 1000.;
                println!(
                    "{case},{},{:.6},{:.6},{:.6},{frames},{gpu_done},{},{},{},{}",
                    i - warmup,
                    ms(event),
                    ms(first),
                    ms(settled),
                    metrics.cached_source_bytes,
                    metrics.composition_fallback,
                    metrics.visible_lines,
                    metrics.gpu_prepared_lines
                );
            }
        }
    }
}
