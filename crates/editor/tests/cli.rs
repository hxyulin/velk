use std::process::Command;

#[test]
fn rejects_missing_and_extra_paths_before_starting_the_ui() {
    for arguments in [vec![], vec!["one.txt", "two.txt"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_velk"))
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("Usage: velk <file>")
        );
    }
}

#[test]
fn reports_missing_files_and_invalid_utf8_without_creating_or_modifying_files() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing file.txt");
    let invalid = directory.path().join("invalid.txt");
    std::fs::write(&invalid, [0xff, 0xfe]).unwrap();
    for path in [&missing, &invalid] {
        let output = Command::new(env!("CARGO_BIN_EXE_velk"))
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("cannot open"), "{error}");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }
    assert!(!missing.exists());
    assert_eq!(std::fs::read(&invalid).unwrap(), [0xff, 0xfe]);
}

#[test]
fn structured_logs_and_startup_capture_flush_even_on_early_open_failure() {
    let directory = tempfile::tempdir().unwrap();
    let logs = directory.path().join("logs");
    let traces = directory.path().join("traces");
    let missing = directory.path().join("missing.txt");
    let output = Command::new(env!("CARGO_BIN_EXE_velk"))
        .env("VELK_LOG_DIR", &logs)
        .env("VELK_LOG_FORMAT", "json")
        .env("VELK_LOG", "error")
        .env("VELK_TRACE", "1")
        .env("VELK_TRACE_DIR", &traces)
        .env("VELK_TRACE_SECONDS", "2")
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!missing.exists());
    let log = std::fs::read_dir(logs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lines = std::fs::read_to_string(log).unwrap();
    let events: Vec<serde_json::Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(
        |event| event["level"] == "ERROR" && event["fields"]["message"] == "application failed"
    ));
    let trace = std::fs::read_dir(traces)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let trace: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(trace).unwrap()).unwrap();
    assert_eq!(
        trace["traceEvents"].as_array().unwrap().last().unwrap()["name"],
        "velk.capture.summary"
    );
}
