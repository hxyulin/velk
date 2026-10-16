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
    manager.set_settings(std::collections::BTreeMap::from([(
        "test.hooks".into(),
        std::collections::BTreeMap::from([
            ("message".into(), "configured before first request".into()),
            ("future_key".into(), true.into()),
        ]),
    )]));
    // SAFETY: only the locally built SDK probe is present in this directory.
    unsafe {
        manager.load()?;
    }
    let editing = manager.editing_provider("test.hooks", velk_editing::Role::Pairing)?;
    let mut editing_context = velk_editing::Request {
        revision: 0,
        text: String::new(),
        offset: 0,
        document_length: 0,
        selection: Default::default(),
        intent: velk_editing::Intent::Type("(".into()),
        settings: Default::default(),
        pairs: vec![],
    };
    assert!(
        matches!(editing.evaluate(&editing_context)?, velk_editing::Decision::Apply(proposal) if proposal.edits[0].text == "()" && proposal.selection.unwrap().head == 1)
    );
    editing_context.intent = velk_editing::Intent::Type("invalid".into());
    assert!(editing.evaluate(&editing_context).is_err());
    let metadata = manager.plugins().next().unwrap().metadata.clone();
    assert_eq!(metadata.name, "Named Hooks Probe");
    assert_eq!(metadata.settings.len(), 1);
    assert!(manager.plugins().next().unwrap().settings_error.is_none());
    assert_eq!(manager.plugins().next().unwrap().settings_values.len(), 1);
    assert!(metadata.readme.contains("**Markdown**"));
    assert_eq!(metadata.capabilities.len(), 5);
    assert!(!metadata.capabilities[0].optional);
    assert!(metadata.capabilities[1].optional);
    let metadata_hook = manager.bind_hook("test.hooks", "velk.plugin.metadata.v1")?;
    let queried: velk_plugin_host::PluginMetadata =
        serde_json::from_slice(&manager.call_hook(&metadata_hook, b"")?)
            .map_err(|e| e.to_string())?;
    assert_eq!(queried, metadata);
    assert!(
        manager
            .call_hook(&metadata_hook, b"unexpected input")
            .is_err()
    );
    let registered = manager.hooks("test.hooks")?;
    assert_eq!(
        registered
            .iter()
            .filter(|hook| hook.name.starts_with("test."))
            .count(),
        7
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
    assert_eq!(metadata.commands.len(), 1);
    let command = manager.bind_hook("test.hooks", "test.command")?;
    let context = velk_commands::Context {
        input: None,
        path: Some("/tmp/source.rs".into()),
        language: Some("rust".into()),
        revision: Some(7),
        position: Some(12),
    };
    let result: velk_commands::Outcome = serde_json::from_slice(
        &manager.call_hook(&command, &serde_json::to_vec(&context).unwrap())?,
    )
    .unwrap();
    assert_eq!(
        result,
        velk_commands::Outcome::Message {
            text: "7:12".into()
        }
    );
    assert!(manager.call_hook(&command, b"invalid-json").is_err());
    let echo = manager.bind_hook("test.hooks", "test.echo")?;
    let state = manager.bind_hook("test.hooks", "test.state")?;
    assert_eq!(
        manager.call_hook(&state, b"")?,
        b"configured before first request"
    );
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
    assert!(copy.exists(), "interactive lease pins native code");
    assert_eq!(
        editing.evaluate(&editing_context)?,
        velk_editing::Decision::Decline
    );
    drop(editing);
    assert!(!copy.exists());
    assert_eq!(owned, input);
    assert_eq!(
        metadata.name, "Named Hooks Probe",
        "host owns metadata after unload"
    );
    assert!(metadata.readme.contains("世界"));
    assert!(manager.call_hook(&metadata_hook, b"").is_err());
    assert!(manager.call_hook(&echo, b"").is_err());
    assert!(manager.call_hook(&command, b"invalid-json").is_err());
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
    manager.set_settings(std::collections::BTreeMap::from([(
        "test.hooks".into(),
        std::collections::BTreeMap::from([("message".into(), false.into())]),
    )]));
    unsafe {
        manager.load()?;
    }
    assert!(
        manager
            .plugins()
            .next()
            .unwrap()
            .settings_error
            .as_ref()
            .is_some_and(|e| e.contains("message"))
    );
    let defaults = manager.bind_hook("test.hooks", "test.state")?;
    assert_eq!(manager.call_hook(&defaults, b"")?, b"initial");
    manager.unload_all();
    let runtime = unsafe { velk_plugin_host::Runtime::start(temp.path().into())? };
    let client = runtime.client();
    let project = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::write(project.path().join("hello.txt"), "native host 世界")
        .map_err(|e| e.to_string())?;
    let project_root = project.path().to_path_buf();
    let requests = client.host_requests();
    let responder = std::thread::spawn(move || {
        use velk_plugin_host::host::{Request, Response, Workspace};
        while let Ok(request) = requests.recv_blocking() {
            assert!(request.is_active());
            let response = match request.request {
                Request::Workspace => Ok(Response::Workspace {
                    workspace: Workspace {
                        current_directory: project_root.clone(),
                        project_root: Some(project_root.clone()),
                    },
                }),
                Request::Buffers => Ok(Response::Buffers { buffers: vec![] }),
                Request::NewBuffer { ref text } if text == "created by native plugin" => {
                    Ok(Response::Created { id: 42 })
                }
                Request::CloseBuffer { id: 42 } => Ok(Response::Accepted),
                _ => Err("unexpected host request".into()),
            };
            request.respond(response);
        }
    });
    pollster::block_on(async {
        client.set_settings(std::collections::BTreeMap::from([(
            "test.hooks".into(),
            std::collections::BTreeMap::from([("message".into(), "async configuration".into())]),
        )]))?;
        assert_eq!(
            client
                .load()?
                .recv()
                .await
                .map_err(|e| e.to_string())??
                .loaded,
            1
        );
        use velk_plugin_host::host::{Request as HostRequest, Response as HostResponse};
        let host = client
            .bind_hook("test.hooks".into(), "test.host".into())?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        for (request, expected) in [
            (
                HostRequest::Buffers,
                HostResponse::Buffers { buffers: vec![] },
            ),
            (
                HostRequest::ReadFile {
                    path: "hello.txt".into(),
                },
                HostResponse::File {
                    bytes: "native host 世界".as_bytes().to_vec(),
                },
            ),
            (
                HostRequest::NewBuffer {
                    text: "created by native plugin".into(),
                },
                HostResponse::Created { id: 42 },
            ),
            (HostRequest::CloseBuffer { id: 42 }, HostResponse::Accepted),
        ] {
            let bytes = client
                .call_hook(host.clone(), serde_json::to_vec(&request).unwrap())
                .await?;
            let response: HostResponse =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            assert_eq!(response, expected);
        }
        let denied = client
            .call_hook(
                host.clone(),
                serde_json::to_vec(&HostRequest::ApplyEdit {
                    edit: Default::default(),
                })
                .unwrap(),
            )
            .await
            .unwrap_err();
        assert!(denied.contains("editor.buffer.write"));
        let escaped = client
            .call_hook(
                host,
                serde_json::to_vec(&HostRequest::ReadFile { path: "..".into() }).unwrap(),
            )
            .await
            .unwrap_err();
        assert!(escaped.contains("outside"));
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
        assert_eq!(
            client.call_hook(state.clone(), vec![]).await?,
            b"async configuration"
        );
        client.set_settings(std::collections::BTreeMap::from([(
            "test.hooks".into(),
            std::collections::BTreeMap::from([("message".into(), "after restart".into())]),
        )]))?;
        assert_eq!(
            client.call_hook(state.clone(), vec![]).await?,
            b"async configuration"
        );
        let restarted = client
            .restart(Some("test.hooks".into()))?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        assert_eq!(restarted.loaded, 1);
        assert!(client.call_hook(state, vec![]).await.is_err());
        let state = client
            .bind_hook("test.hooks".into(), "test.state".into())?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        assert_eq!(
            client.call_hook(state.clone(), vec![]).await?,
            b"after restart"
        );
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
    drop(runtime);
    responder
        .join()
        .map_err(|_| "host service responder panicked")?;
    let runtime = unsafe { velk_plugin_host::Runtime::start(temp.path().into())? };
    let client = runtime.client();
    let requests = client.host_requests();
    pollster::block_on(client.load()?.recv()).map_err(|e| e.to_string())??;
    let hook = pollster::block_on(
        client
            .bind_hook("test.hooks".into(), "test.host".into())?
            .recv(),
    )
    .map_err(|e| e.to_string())??;
    let waiter = std::thread::spawn(move || {
        pollster::block_on(client.call_hook(
            hook,
            serde_json::to_vec(&velk_plugin_host::host::Request::Buffers).unwrap(),
        ))
    });
    let pending = requests.recv_blocking().map_err(|e| e.to_string())?;
    drop(runtime);
    assert!(!pending.is_active());
    assert!(
        waiter
            .join()
            .map_err(|_| "native host waiter panicked")?
            .unwrap_err()
            .contains("retired")
    );
    println!(
        "Named hook smoke passed: host service C callbacks, project reads, capability checks, buffer lifecycle dispatch, settings defaults, overrides, validation and restart, discovery, custom opaque-byte requests, ordered event delivery, errors/panic containment, owned replies, cleanup and stale generation rejection; no feature-specific root ABI fields."
    );
    Ok(())
}
