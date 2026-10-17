use super::*;
#[allow(unsafe_code)]
pub fn run(output: &Path) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let spec = stage_wasm(output, directory.path())?;
    publish(directory.path(), vec![spec.clone()])?;
    let mut manager = velk_plugin_host::Manager::new(directory.path().into());
    // SAFETY: this fixture stages only the bounded locally built Wasm guest.
    let events = unsafe { manager.load()? };
    assert!(matches!(&events[..], [velk_plugin_host::Event::Loaded(_)]));
    let generation = manager.plugins().next().unwrap().generation;
    let provider = manager.matching_provider(&spec.id)?;
    for query in [
        "",
        "print",
        "PRINT",
        "pritn",
        "pront",
        "compleiton",
        "世介接口",
        "zzzz",
    ] {
        let candidates = [
            "print",
            "println",
            "PRINT",
            "completion",
            "世界接口",
            "unrelated",
            "a\0b",
        ];
        assert_eq!(
            provider.scores(query, &candidates)?,
            velk_completion_matching::scores(query, &candidates)
        );
    }
    let candidates: Vec<_> = (0..1000).map(|n| format!("print_{n}")).collect();
    let candidates: Vec<_> = candidates.iter().map(String::as_str).collect();
    assert_eq!(
        provider.scores("pritn", &candidates)?,
        velk_completion_matching::scores("pritn", &candidates)
    );
    let long = "x".repeat(4096);
    assert_eq!(provider.scores("", &vec![long.as_str(); 1000])?.len(), 1000);
    assert!(provider.scores(&"x".repeat(4097), &[]).is_err());
    assert!(provider.scores("print", &["print"]).is_ok());
    unsafe {
        manager.restart(Some(&spec.id))?;
    }
    assert!(provider.scores("print", &["print"]).is_err());
    assert!(manager.plugins().next().unwrap().generation > generation);
    let next = manager.matching_provider(&spec.id)?;
    assert_eq!(next.scores("print", &["print"])?.len(), 1);
    manager.unload_all();
    assert!(next.scores("", &[]).is_err());
    assert_eq!(manager.plugins().count(), 0);
    let mut tampered = spec;
    tampered.digest = "0".repeat(64);
    publish(directory.path(), vec![tampered])?;
    let events = unsafe { manager.load()? };
    assert!(matches!(
        &events[..],
        [velk_plugin_host::Event::Rejected { .. }]
    ));
    println!(
        "Wasm matching smoke passed: native parity, Unicode/typos, maximum input, rejected oversized requests, restart, stale leases, unload, digest verification."
    );
    Ok(())
}
