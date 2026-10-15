use std::{
    hint::black_box,
    time::{Duration, Instant},
};
fn probe() {
    let _scope = velk_diagnostics::tracing::debug_span!(target: "velk_diagnostics_bench", "benchmark.scope", revision = 7).entered();
    black_box(7);
}
fn run(count: usize) -> f64 {
    let started = Instant::now();
    for _ in 0..count {
        probe();
    }
    started.elapsed().as_nanos() as f64 / count as f64
}
fn main() {
    let directory = tempfile::tempdir().unwrap();
    let _guard = velk_diagnostics::init_with(velk_diagnostics::Config {
        directory: directory.path().into(),
        duration: Duration::from_secs(10),
        filter: "off".into(),
        json_logs: false,
        startup_capture: false,
        targets: vec!["velk".into()],
        log_directory: None,
    })
    .unwrap();
    run(1000);
    let idle = run(100_000);
    velk_diagnostics::start().unwrap();
    let enabled = run(5000);
    let report = velk_diagnostics::stop().unwrap();
    println!(
        "Disabled: {idle:.1} ns/scope; capturing: {enabled:.1} ns/scope; {} events, {} dropped",
        report.events, report.dropped
    );
}
