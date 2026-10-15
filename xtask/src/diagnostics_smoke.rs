use super::*;
#[allow(unsafe_code)]
pub(super) fn run(root: &Path, output: &Path) -> Result<(), String> {
    build(root, false, false)?;
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let directory = temporary.path().join("plugins");
    publish(&directory, stage_builtins(output, &directory)?)?;
    let _guard = velk_diagnostics::init_with(velk_diagnostics::Config {
        directory: temporary.path().join("traces"),
        duration: std::time::Duration::from_secs(10),
        filter: "off".into(),
        json_logs: true,
        startup_capture: false,
        targets: vec!["velk".into()],
        log_directory: None,
    })?;
    let path = velk_diagnostics::start()?;
    let mut manager = velk_plugin_host::Manager::new(directory);
    unsafe {
        manager.load()?;
    }
    manager.render_markdown("**diagnostics**")?;
    let search = manager.bind_hook(velk_search::DEFAULT_BACKEND, velk_search::HOOK)?;
    for request in [
        velk_search::Request::Begin {
            id: 1,
            revision: 0,
            query: velk_search::Query {
                text: "text".into(),
                ..Default::default()
            },
        },
        velk_search::Request::Append {
            id: 1,
            text: "text text".into(),
        },
        velk_search::Request::Start { id: 1 },
    ] {
        manager.call_hook(&search, &velk_search::encode(&request)?)?;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let reply = manager.call_hook(
            &search,
            &velk_search::encode(&velk_search::Request::Poll { id: 1 })?,
        )?;
        if matches!(velk_search::decode(&reply)?, velk_search::Reply::Ready(_)) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err("search trace test timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    unsafe {
        manager.restart(None)?;
    }
    manager.render_markdown("after restart")?;
    manager.unload_all();
    let report = velk_diagnostics::stop()?;
    assert_eq!(report.dropped, 0);
    let trace: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let events = trace["traceEvents"].as_array().unwrap();
    for name in ["markdown.parse", "search.match"] {
        assert!(
            events
                .iter()
                .any(|e| e["name"] == name && e["args"]["source"] == "native_plugin"),
            "missing native DLL event {name}"
        );
    }
    assert!(
        events
            .iter()
            .any(|e| e["cat"] == "velk_plugin_host::native")
    );
    println!(
        "Diagnostics smoke passed: {} events, separate DLL/worker timings, restart/unload and host tracing share one completed capture",
        report.events
    );
    Ok(())
}
