//! Parser/query CPU timings; excludes scheduling, shaping, GPU and presentation.
use ropey::Rope;
use std::{hint::black_box, time::Instant};
use velk_syntax::{Edit, Engine, Language};
fn stats(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    (
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100],
    )
}
fn run(name: &str, source: &str) {
    let mut engine = Engine::new(Language::Rust).unwrap();
    let mut text = Rope::from_str(source);
    let start = Instant::now();
    engine.update(0, text.clone(), &[], || false).unwrap();
    println!(
        "{name},initial_parse,{},{:.6},-",
        text.len_bytes(),
        start.elapsed().as_secs_f64() * 1000.
    );
    let mut parse = Vec::new();
    let mut query = Vec::new();
    for revision in 1..=101 {
        let before = text.clone();
        let position = text.line_to_char(text.len_lines() / 2);
        text.insert(position, " ");
        let edit = Edit::between(revision, &before, &text, position..position, 1);
        let start = Instant::now();
        let snapshot = engine
            .update(revision, text.clone(), &[edit], || false)
            .unwrap();
        parse.push(start.elapsed().as_secs_f64() * 1000.);
        let first_line = text.char_to_line(position);
        let end = text.line_to_char((first_line + 40).min(text.len_lines() - 1));
        let start = Instant::now();
        black_box(snapshot.highlights(position..end));
        query.push(start.elapsed().as_secs_f64() * 1000.);
    }
    for (operation, samples) in [("incremental_parse", parse), ("query_40_lines", query)] {
        let (median, p95) = stats(samples);
        println!("{name},{operation},{},{median:.6},{p95:.6}", source.len());
    }
}
fn main() {
    println!("case,operation,bytes,median_ms,p95_ms");
    run("real_main", include_str!("../../editor/src/main.rs"));
    run(
        "10k_lines",
        &"fn example() { let value = Some(42); } // comment\n".repeat(10_000),
    );
    run(
        "100k_lines",
        &"fn example() { let value = Some(42); } // comment\n".repeat(100_000),
    );
}
