use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
fn percentile(values: &[f64], fraction: f64) -> f64 {
    values[((values.len() as f64 * fraction).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1)]
}
pub(super) fn run(path: &Path) -> Result<(), String> {
    let trace: Value = serde_json::from_reader(std::io::BufReader::new(
        std::fs::File::open(path).map_err(|e| e.to_string())?,
    ))
    .map_err(|e| e.to_string())?;
    let events = trace["traceEvents"]
        .as_array()
        .ok_or("expected a completed Velk Chrome trace")?;
    let mut durations: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut async_starts = BTreeMap::new();
    let mut latency = Vec::new();
    let mut queue_wait = Vec::new();
    for event in events {
        let name = format!(
            "{}::{}",
            event["cat"].as_str().unwrap_or(""),
            event["name"].as_str().unwrap_or("")
        );
        let key = format!("{}:{}:{name}", event["pid"], event["id"]);
        match event["ph"].as_str() {
            Some("X") => {
                if let Some(us) = event["dur"].as_f64().filter(|n| n.is_finite() && *n >= 0.) {
                    durations.entry(name).or_default().push(us / 1000.);
                }
            }
            Some("b") => {
                if let Some(us) = event["ts"].as_f64() {
                    async_starts.insert(key, us);
                }
            }
            Some("e") => {
                if let (Some(start), Some(end)) = (async_starts.remove(&key), event["ts"].as_f64())
                {
                    durations
                        .entry(format!("[wall] {name}"))
                        .or_default()
                        .push((end - start).max(0.) / 1000.);
                }
            }
            _ => {}
        }
        if let Some(ms) = event["args"]["input_to_prepare_ms"].as_f64() {
            latency.push(ms);
        }
        if let Some(ms) = event["args"]["queue_wait_ms"].as_f64() {
            queue_wait.push(ms);
        }
        if event["name"] == "velk.capture.summary" {
            println!("Capture: {}", event["args"]);
        }
    }
    println!(
        "{} events. Times are inclusive CPU scope elapsed time; [wall] includes async waiting.\n",
        events.len()
    );
    let mut rows: Vec<_> = durations.into_iter().collect();
    for (_, times) in &mut rows {
        times.sort_by(f64::total_cmp);
    }
    rows.sort_by(|a, b| b.1.iter().sum::<f64>().total_cmp(&a.1.iter().sum::<f64>()));
    println!(
        "{:>7} {:>11} {:>10} {:>10} {:>10} {:>10}  Scope",
        "Count", "Total ms", "p50 ms", "p95 ms", "p99 ms", "Max ms"
    );
    for (name, times) in rows.iter().take(40) {
        print_row(name, times);
    }
    for (name, mut times) in [
        ("Input listener → prepared (CPU only)", latency),
        ("Queue wait", queue_wait),
    ] {
        if !times.is_empty() {
            times.sort_by(f64::total_cmp);
            print_row(name, &times);
        }
    }
    if !async_starts.is_empty() {
        println!(
            "{} async spans were still open at the capture boundary.",
            async_starts.len()
        );
    }
    Ok(())
}
fn print_row(name: &str, times: &[f64]) {
    println!(
        "{:>7} {:>11.3} {:>10.3} {:>10.3} {:>10.3} {:>10.3}  {}",
        times.len(),
        times.iter().sum::<f64>(),
        percentile(times, 0.5),
        percentile(times, 0.95),
        percentile(times, 0.99),
        times[times.len() - 1],
        name
    );
}
