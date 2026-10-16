use super::*;
#[allow(unsafe_code)]
pub(super) fn run(output: &Path) -> Result<(), String> {
    use velk_commands::{PickerReply, PickerRequest};
    use velk_plugin_host::host::{Buffer, Request, Response, Workspace};
    let plugins = tempfile::tempdir().map_err(|e| e.to_string())?;
    publish(plugins.path(), stage_builtins(output, plugins.path())?)?;
    let project = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::create_dir(project.path().join("src")).map_err(|e| e.to_string())?;
    std::fs::write(project.path().join(".gitignore"), "ignored.txt\n")
        .map_err(|e| e.to_string())?;
    std::fs::write(project.path().join("ignored.txt"), "needle").map_err(|e| e.to_string())?;
    let path = project.path().join("src/main.rs");
    std::fs::write(&path, "disk text").map_err(|e| e.to_string())?;
    let runtime = unsafe { velk_plugin_host::Runtime::start(plugins.path().into())? };
    let client = runtime.client();
    client.set_workspace(Workspace {
        current_directory: project.path().into(),
        project_root: Some(project.path().into()),
    });
    client.set_settings(std::collections::BTreeMap::from([(
        "velk.autosave".into(),
        std::collections::BTreeMap::from([
            ("enabled".into(), true.into()),
            ("delay_ms".into(), 250.into()),
        ]),
    )]))?;
    let buffer = Buffer {
        id: 7,
        title: "main.rs".into(),
        path: Some(path.clone()),
        revision: 9,
        dirty: true,
        active: false,
        anchor: 0,
        head: 0,
    };
    let requests = client.host_requests();
    let info = buffer.clone();
    let (saved, receive_saved) = std::sync::mpsc::channel();
    let responder = std::thread::spawn(move || {
        while let Ok(request) = requests.recv_blocking() {
            if !request.is_active() {
                request.respond(Err("retired".into()));
                continue;
            }
            let response = match request.request {
                Request::Buffers => Ok(Response::Buffers {
                    buffers: vec![info.clone()],
                }),
                Request::ReadBuffer { id: 7 } => Ok(Response::Buffer {
                    buffer: info.clone(),
                    text: "unsaved needle 世界".into(),
                }),
                Request::SaveBuffer {
                    id: 7,
                    expected_revision: Some(9),
                } => {
                    saved.send(()).unwrap();
                    Ok(Response::Accepted)
                }
                _ => Err("unexpected project smoke host request".into()),
            };
            request.respond(response);
        }
    });
    pollster::block_on(async {
        let report = client
            .load_except(vec![
                "velk.terminal".into(),
                "velk.lsp".into(),
                "velk.language-server.rust-analyzer".into(),
            ])?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        assert!(report.plugins.iter().any(|p| p.id == "velk.autosave"));
        for (id, session, hook, text, expected) in [
            (1, 1, "velk.quick-open.query", "smr", Some(path.clone())),
            (3, 1, "velk.quick-open.query", "new-file", None),
            (
                4,
                4,
                "velk.quick-open.query",
                "new-file",
                Some(project.path().join("new-file.rs")),
            ),
            (5, 4, "velk.quick-open.query", "smr", Some(path.clone())),
            (
                2,
                1,
                "velk.workspace-find.query",
                "needle",
                Some(path.clone()),
            ),
        ] {
            let plugin = if hook == "velk.quick-open.query" {
                "velk.quick-open"
            } else {
                "velk.workspace-find"
            };
            let handle = client
                .bind_hook(plugin.into(), hook.into())?
                .recv()
                .await
                .map_err(|e| e.to_string())??;
            let context = velk_commands::Context {
                input: None,
                path: None,
                language: None,
                revision: None,
                position: None,
            };
            let begin = PickerRequest::Begin {
                session,
                id,
                context,
                query: velk_search::Query {
                    text: text.into(),
                    ..Default::default()
                },
            };
            client
                .call_hook(handle.clone(), serde_json::to_vec(&begin).unwrap())
                .await?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let reply: PickerReply = serde_json::from_slice(
                    &client
                        .call_hook(
                            handle.clone(),
                            serde_json::to_vec(&PickerRequest::Poll { id }).unwrap(),
                        )
                        .await?,
                )
                .map_err(|e| e.to_string())?;
                if let PickerReply::Ready { items, .. } = reply {
                    assert_eq!(items.len(), usize::from(expected.is_some()));
                    if let Some(expected) = &expected {
                        let velk_commands::ChoiceAction::Location { location } = &items[0].action
                        else {
                            panic!()
                        };
                        assert_eq!(&location.path, expected);
                        if id == 2 {
                            assert_eq!(location.position.column, 8);
                        }
                    }
                    if id == 1 {
                        std::fs::write(project.path().join("new-file.rs"), "new file")
                            .map_err(|e| e.to_string())?;
                    }
                    break;
                }
                if std::time::Instant::now() > deadline {
                    return Err("native project picker timed out".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            client
                .emit_hook(
                    velk_commands::PICKER_CANCEL_HOOK.into(),
                    serde_json::to_vec(&velk_commands::PickerCancel {
                        plugin: plugin.into(),
                        id,
                    })
                    .unwrap(),
                )?
                .recv()
                .await
                .map_err(|e| e.to_string())??;
            assert!(
                client
                    .call_hook(
                        handle,
                        serde_json::to_vec(&PickerRequest::Poll { id }).unwrap()
                    )
                    .await
                    .is_err()
            );
        }
        Ok::<_, String>(())
    })?;
    client.publish_buffer_events(vec![velk_plugin_host::buffers::Event::Changed {
        buffer: buffer.clone(),
    }])?;
    receive_saved
        .recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    client.publish_buffer_events(vec![velk_plugin_host::buffers::Event::Closed { id: 7 }])?;
    drop(runtime);
    responder
        .join()
        .map_err(|_| "project smoke responder panicked")?;
    println!(
        "Project feature smoke passed: packed native Quick Open session caching and refresh, ignored paths, unsaved workspace search, cancellation, buffer events, opt-in autosave deadline and revision-bound host saves, unload."
    );
    Ok(())
}
