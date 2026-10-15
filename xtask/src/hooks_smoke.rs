//! Load a test-only DSO with operation names unknown to the core API.
use super::*;
#[allow(unsafe_code)]
pub(super) fn run(root: &Path, output: &Path, release: bool) -> Result<(), String> {
    let mut build = cargo();
    build.current_dir(root).args([
        "build",
        "--locked",
        "-p",
        "velk-plugin-sdk",
        "--example",
        "hooks-probe",
    ]);
    if release {
        build.arg("--release");
    }
    checked(&mut build)?;
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let spec = stage_plugin(
        &output.join("examples"),
        temp.path(),
        "test.hooks",
        "hooks_probe",
    )?;
    publish(temp.path(), vec![spec])?;
    let mut manager = velk_plugin_host::Manager::new(temp.path().into());
    // SAFETY: only the locally built SDK probe is present in this directory.
    unsafe {
        manager.load()?;
    }
    let registered = manager.hooks("test.hooks")?;
    assert_eq!(
        registered
            .iter()
            .filter(|hook| hook.name.starts_with("test."))
            .count(),
        5
    );
    assert!(
        registered
            .iter()
            .any(|hook| hook.name == "velk.diagnostics.attach.v1")
    );
    assert!(
        registered
            .iter()
            .any(|h| h.name == "test.notice" && h.kind == velk_plugin_host::hook_kind::EVENT)
    );
    let echo = manager.bind_hook("test.hooks", "test.echo")?;
    let state = manager.bind_hook("test.hooks", "test.state")?;
    let input = b"\0opaque\xffbytes";
    let owned = manager.call_hook(&echo, input)?;
    assert_eq!(owned, input);
    assert_eq!(manager.emit_hook("test.notice", b"event delivered")?, 1);
    assert_eq!(manager.call_hook(&state, b"")?, b"event delivered");
    assert_eq!(manager.emit_hook("test.unsubscribed", b"")?, 0);
    assert!(manager.bind_hook("test.hooks", "test.notice").is_err());
    assert!(manager.bind_hook("test.hooks", "test.missing").is_err());
    let error = manager.bind_hook("test.hooks", "test.error")?;
    assert_eq!(manager.call_hook(&error, b"").unwrap_err(), "test error");
    let panic = manager.bind_hook("test.hooks", "test.panic")?;
    assert!(manager.call_hook(&panic, b"").unwrap_err().contains("4"));
    assert_eq!(manager.call_hook(&echo, b"after panic")?, b"after panic");
    let copy = manager.plugins().next().unwrap().loaded_path.clone();
    manager.unload_all();
    assert!(!copy.exists());
    assert_eq!(owned, input);
    assert!(manager.call_hook(&echo, b"").is_err());
    unsafe {
        manager.load()?;
    }
    assert!(
        manager.call_hook(&echo, b"").is_err(),
        "old handles cannot bind to a new generation"
    );
    let echo = manager.bind_hook("test.hooks", "test.echo")?;
    assert_eq!(manager.call_hook(&echo, b"new")?, b"new");
    let foreign_handle = echo;
    manager.unload_all();
    let runtime = unsafe { velk_plugin_host::Runtime::start(temp.path().into())? };
    let client = runtime.client();
    pollster::block_on(async {
        assert_eq!(
            client
                .load()?
                .recv()
                .await
                .map_err(|e| e.to_string())??
                .loaded,
            1
        );
        assert!(
            client
                .call_hook(foreign_handle, vec![])
                .await
                .unwrap_err()
                .contains("another runtime")
        );
        let state = client
            .bind_hook("test.hooks".into(), "test.state".into())?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        // Dropping an acknowledgement does not cancel an accepted event.
        drop(client.emit_hook("test.notice".into(), b"ordered event".to_vec())?);
        assert_eq!(
            client.call_hook(state.clone(), vec![]).await?,
            b"ordered event"
        );
        client
            .unload_all()?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        assert!(client.call_hook(state, vec![]).await.is_err());
        Ok::<_, String>(())
    })?;
    println!(
        "Named hook smoke passed: discovery, custom opaque-byte requests, ordered event delivery, errors/panic containment, owned replies, cleanup and stale generation rejection; no feature-specific root ABI fields."
    );
    Ok(())
}
