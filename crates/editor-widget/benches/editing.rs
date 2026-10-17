//! CPU timings only: no GPU upload, painting, presentation, or input latency.
use rxui::astrelis::{FontFamily, TextBuffer, TextSpan, TextStyle, TextSystem, TextWrap};
use std::{hint::black_box, time::Instant};

fn sample(mut operation: impl FnMut(), count: usize) -> (f64, f64) {
    let mut times = Vec::with_capacity(count);
    for _ in 0..count {
        let start = Instant::now();
        operation();
        times.push(start.elapsed().as_secs_f64() * 1000.);
    }
    times.sort_by(f64::total_cmp);
    (times[count / 2], times[(count * 95 / 100).min(count - 1)])
}

fn main() {
    let mut fonts = TextSystem::new();
    assert!(
        !fonts.load_system_fonts().is_empty(),
        "benchmark requires installed desktop fonts"
    );
    let family = if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    };
    let style = TextStyle::new()
        .family(FontFamily::from(family))
        .font_size(16.)
        .line_height(24.);
    println!("case,lines,bytes,median_ms,p95_ms");
    for lines in [100, 1000, 10000] {
        let source = "let greeting = \"Hello, rope!\"; // a short line\n".repeat(lines);
        let mut document =
            velk_core::Buffer::with_history(&source, std::sync::Arc::new(velk_undo_tree::Tree));
        document.set_caret(document.text().len_chars() / 2, false);
        let (median, p95) = sample(
            || {
                document.insert("x");
                document.undo();
                black_box(document.revision());
            },
            101,
        );
        println!(
            "insert_and_undo,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let (median, p95) = sample(
            || {
                black_box(document.text().to_string());
            },
            101,
        );
        println!(
            "rope_to_string,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let mut buffer = TextBuffer::new();
        buffer.set_wrap(TextWrap::None);
        buffer.set_width(Some(1200.)).unwrap();
        buffer.set_text(&source, style.clone()).unwrap();
        buffer
            .layout(&mut fonts)
            .unwrap()
            .prepare_interaction()
            .unwrap();
        let mut alternate = false;
        let (median, p95) = sample(
            || {
                alternate = !alternate;
                let mut text = document.text().to_string();
                text.insert(source.len() / 2, if alternate { 'x' } else { 'y' });
                buffer.set_text(&text, style.clone()).unwrap();
                black_box(buffer.layout(&mut fonts).unwrap())
                    .prepare_interaction()
                    .unwrap();
            },
            21,
        );
        println!(
            "edit_and_cpu_layout,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let mut spans = (0..lines)
            .step_by(5)
            .map(|line| {
                let start = line * 47;
                TextSpan::new(start..start + 3)
                    .weight(700)
                    .color([0.2, 0.6, 0.9, 1.])
            })
            .collect::<Vec<_>>();
        let (median, p95) = sample(
            || {
                alternate = !alternate;
                for span in &mut spans {
                    span.color = Some(if alternate {
                        [0.2, 0.6, 0.9, 1.]
                    } else {
                        [0.3, 0.7, 0.8, 1.]
                    });
                }
                buffer
                    .set_rich_text(&source, style.clone(), spans.clone())
                    .unwrap();
                black_box(buffer.layout(&mut fonts).unwrap())
                    .prepare_interaction()
                    .unwrap();
            },
            11,
        );
        println!(
            "rich_style_update_and_cpu_layout,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let (median, p95) = sample(
            || {
                black_box(buffer.layout(&mut fonts).unwrap());
            },
            101,
        );
        println!(
            "cached_layout,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
    }
    // A single very long line stresses the current per-line grapheme scan.
    let mut document = velk_core::Buffer::with_history(
        &"a".repeat(100000),
        std::sync::Arc::new(velk_undo_tree::Tree),
    );
    document.set_caret(50000, false);
    let (median, p95) = sample(
        || {
            document.horizontal(true, false);
            document.horizontal(false, false);
        },
        101,
    );
    println!("long_line_left_right,1,100000,{median:.6},{p95:.6}");
}
