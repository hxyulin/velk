//! Dispatch-to-submission CPU timings through the real RXUI/widget pipeline.
//! Offscreen rendering excludes native event delivery, surface waits and scanout.
use rxui::{
    KeyEvent, KeyboardKey, Modifiers, TestUi, TextInputEvent,
    astrelis::{Framebuffer, FramebufferOptions, GraphicsContext, wgpu},
};
use std::time::{Duration, Instant};
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
    println!("case,sample,event_ms,first_submit_ms,settled_ms,frames,gpu_done_ms");
    for case in [
        "plain_navigation",
        "rich_navigation",
        "large_document_typing",
        "ime_preedit",
        "large_document_ime",
    ] {
        if std::env::var("VELK_BENCH_CASE").is_ok_and(|filter| filter != case) {
            continue;
        }
        let editor = match case {
            "plain_navigation" => Editor::new(&"x".repeat(100_000)),
            "rich_navigation" => rich(),
            "large_document_typing" => Editor::new(&"let count = 42; // value\n".repeat(100_000)),
            "large_document_ime" => Editor::new(&"let count = 42; // value\n".repeat(10_000)),
            _ => Editor::new("Unicode: café 世界 👩‍💻\n"),
        };
        let mut test = TestUi::new(&graphics, [1200, 800], editor).unwrap();
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
        // Warm up the event path and both window ends before collecting samples.
        let (warmup, samples) = if case == "large_document_ime" {
            (2, 21)
        } else {
            (20, 201)
        };
        for i in 0..warmup + samples {
            let start = Instant::now();
            match case {
                "plain_navigation" | "rich_navigation" => key(
                    &mut test,
                    if i % 2 == 0 {
                        KeyboardKey::End
                    } else {
                        KeyboardKey::Home
                    },
                ),
                "large_document_typing" if i % 2 == 0 => {
                    test.text_input(TextInputEvent::Insert("x".into())).unwrap();
                }
                "large_document_typing" => key(&mut test, KeyboardKey::Backspace),
                "large_document_ime" => {
                    test.text_input(TextInputEvent::Preedit {
                        text: if i % 2 == 0 { "かんじ" } else { "にほん" }.into(),
                        cursor: Some((0, 9)),
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
                let ms = |d: Duration| d.as_secs_f64() * 1000.;
                println!(
                    "{case},{},{:.6},{:.6},{:.6},{frames},{gpu_done}",
                    i - warmup,
                    ms(event),
                    ms(first),
                    ms(settled)
                );
            }
        }
    }
}
