//! Same CPU projection/cache/layout path as the widget. GPU work is excluded.
use ropey::Rope;
use rxui::astrelis::{FontFamily, TextBuffer, TextStyle, TextSystem, TextWrap};
use std::{hint::black_box, sync::Arc, time::Instant};
use velk_editor_widget::{
    Annotations, CpuViewportProbe, OverlayKind, OverlayPlacement, SpanStyle, StyledRange,
    TextOverlay,
};
fn sample(mut operation: impl FnMut(), count: usize) -> (f64, f64) {
    let mut times = Vec::with_capacity(count);
    for _ in 0..count {
        let start = Instant::now();
        operation();
        times.push(start.elapsed().as_secs_f64() * 1000.);
    }
    times.sort_by(f64::total_cmp);
    (times[count / 2], times[count * 95 / 100])
}
fn rich(lines: usize, color: [f32; 4]) -> Arc<Annotations> {
    let mut annotations = Annotations::default();
    for line in 0..lines {
        let start = line * 25;
        annotations.spans.push(StyledRange::new(
            start..start + 3,
            SpanStyle {
                foreground: Some(color),
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
            background: Some([0., 0., 0., 0.2]),
            hover: None,
        });
    }
    Arc::new(annotations)
}
fn main() {
    println!("case,lines,bytes,median_ms,p95_ms");
    for lines in [100, 1000, 10000, 100000] {
        let source = "let count = 42; // value\n".repeat(lines);
        assert_eq!(source.len(), lines * 25);
        let mut fonts = TextSystem::new();
        fonts.load_system_fonts();
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
            .line_height(24.)
            .feature(*b"liga", 0);
        let mut full = TextBuffer::new();
        full.set_width(Some(1168.)).unwrap();
        full.set_wrap(TextWrap::None);
        let mut alternate_full = false;
        let (median, p95) = sample(
            || {
                alternate_full = !alternate_full;
                let mut text = source.clone();
                text.insert(2, if alternate_full { 'x' } else { 'y' });
                full.set_text(&text, style.clone()).unwrap();
                black_box(full.layout(&mut fonts).unwrap())
                    .prepare_interaction()
                    .unwrap();
            },
            21,
        );
        println!(
            "full_document_edit_layout,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let original = Rope::from_str(&source);
        let mut probe = CpuViewportProbe::new(original.clone());
        let viewport = [1200., 800.];
        let (median, p95) = sample(
            || {
                black_box(probe.prepare(viewport, 0.));
            },
            101,
        );
        println!(
            "warm_viewport,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let mut revision = 0;
        let (median, p95) = sample(
            || {
                revision += 1;
                let mut rope = original.clone();
                rope.insert(2, if revision % 2 == 0 { "x" } else { "y" });
                probe.set_document(rope, revision);
                black_box(probe.prepare(viewport, 0.));
            },
            101,
        );
        println!(
            "edit_and_viewport_layout,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        probe.set_document(original.clone(), revision + 1);
        probe.set_annotations(Some(rich(lines, [0.3, 0.6, 0.9, 1.])));
        probe.prepare(viewport, 0.);
        let (median, p95) = sample(
            || {
                probe.reset_cache();
                black_box(probe.prepare(viewport, 0.));
            },
            21,
        );
        println!(
            "cold_rich_inline_viewport,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let (median, p95) = sample(
            || {
                black_box(probe.prepare(viewport, 0.));
            },
            101,
        );
        println!(
            "warm_rich_inline_viewport,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        // Prebuild immutable provider results: separately time ingestion/indexing
        // and visible layout, without timing the provider's own allocations.
        let a = rich(lines, [0.3, 0.6, 0.9, 1.]);
        let b = rich(lines, [0.4, 0.7, 0.8, 1.]);
        let mut alternate = false;
        let (median, p95) = sample(
            || {
                alternate = !alternate;
                probe.set_annotations(Some(if alternate { a.clone() } else { b.clone() }));
                black_box(probe.prepare(viewport, 0.));
            },
            31,
        );
        println!(
            "rich_snapshot_and_viewport_layout,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let mut caret = 2;
        let (median, p95) = sample(
            || {
                caret = if caret == 2 { 3 } else { 2 };
                probe.set_caret(caret);
                black_box(probe.prepare(viewport, 0.));
            },
            101,
        );
        println!(
            "caret_and_rich_prepare,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
        let mut line = 0;
        let (median, p95) = sample(
            || {
                line = (line + 40) % lines;
                black_box(probe.prepare(viewport, line as f32 * 24.));
            },
            31,
        );
        println!(
            "scroll_and_viewport_layout,{lines},{},{median:.6},{p95:.6}",
            source.len()
        );
    }
    let original = Rope::from_str(&"a".repeat(100000));
    let mut probe = CpuViewportProbe::new(original.clone());
    probe.set_caret(50000);
    let mut revision = 0;
    let (median, p95) = sample(
        || {
            revision += 1;
            let mut rope = original.clone();
            rope.insert(50000, if revision % 2 == 0 { "x" } else { "y" });
            probe.set_document(rope, revision);
            black_box(probe.prepare([1200., 800.], 0.));
        },
        21,
    );
    println!("long_line_edit_layout,1,100000,{median:.6},{p95:.6}");
    let mut probe = CpuViewportProbe::new(Rope::from_str(&"a".repeat(10000)));
    let mut annotations = Annotations::default();
    for hint in 0..1000 {
        annotations.spans.push(StyledRange::new(
            hint * 10..hint * 10 + 3,
            SpanStyle {
                weight: Some(700),
                ..SpanStyle::default()
            },
        ));
        annotations.overlays.push(TextOverlay {
            anchor: hint * 10 + 5,
            text: ": i32".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [0.3, 0.7, 1., 1.],
            background: None,
            hover: None,
        });
    }
    probe.set_annotations(Some(Arc::new(annotations)));
    let (median, p95) = sample(
        || {
            probe.reset_cache();
            black_box(probe.prepare([1200., 800.], 0.));
        },
        11,
    );
    println!("long_line_1000_inline_layout,1,10000,{median:.6},{p95:.6}");
}
