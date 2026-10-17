use super::*;
use velk_core::{Buffer, Selection, TextEdit, Transaction};

#[allow(unsafe_code)]
pub(super) fn run(output: &Path, pack: bool) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let id = velk_core::history::DEFAULT_PROVIDER;
    let specs = if pack {
        stage_builtins(output, directory.path())?
            .into_iter()
            .filter(|spec| spec.id == id || spec.id == "velk.undo.browser")
            .collect()
    } else {
        vec![stage_plugin(
            &output.join("examples"),
            directory.path(),
            id,
            "velk_plugin_undo",
        )?]
    };
    publish(directory.path(), specs)?;
    let mut manager = velk_plugin_host::Manager::new(directory.path().into());
    // SAFETY: these artifacts are built locally from the bundled SDK provider.
    unsafe {
        manager.load()?;
    }
    let provider = manager.undo_provider(id)?;
    let mut buffer = Buffer::with_history("abcdef", provider.clone());
    buffer
        .apply_transaction(Transaction {
            revision: 0,
            edits: vec![
                TextEdit {
                    range: 0..1,
                    text: "A".into(),
                },
                TextEdit {
                    range: 5..6,
                    text: "F".into(),
                },
            ],
            selection: None,
        })
        .map_err(|e| e.to_string())?;
    let mut peer = buffer.new_view();
    peer.set_caret(3, false);
    let snapshot = buffer.snapshot();
    manager.unload(id);
    assert!(manager.undo_provider(id).is_err());
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "abcdef");
    assert_eq!(peer.selection().head, 3);
    buffer.redo();
    assert_eq!(buffer.text().to_string(), "AbcdeF");
    buffer.undo();
    buffer.insert("new ");
    buffer.undo();
    assert_eq!(buffer.redo_branches().len(), 2);
    let tree = buffer
        .inspect_history()?
        .tree
        .ok_or("missing native undo inspection")?;
    assert_eq!(tree.nodes.len(), 3);
    let original = tree
        .nodes
        .iter()
        .find(|n| n.inserted == 2 && n.removed == 2)
        .ok_or("missing original branch")?
        .state;
    assert!(buffer.checkout_history(buffer.revision(), original));
    assert_eq!(buffer.text().to_string(), "AbcdeF");
    drop(buffer);
    drop(peer);
    drop(manager);
    drop(provider);
    // Snapshot encoding must work on a worker after all owning sessions retire.
    let bytes = std::thread::spawn(move || snapshot.encode())
        .join()
        .map_err(|_| "undo snapshot worker panicked")??;
    let mut manager = velk_plugin_host::Manager::new(directory.path().into());
    unsafe {
        manager.load()?;
    }
    let provider = manager.undo_provider(id)?;
    let mut restored = Buffer::restore_with_history(&bytes, provider.clone())?;
    assert_eq!(restored.text().to_string(), "AbcdeF");
    let mut peer = restored.new_view();
    peer.set_caret(3, false);
    restored.undo();
    assert_eq!(peer.selection(), Selection { anchor: 3, head: 3 });
    restored.redo();
    restored.set_history_limits(velk_core::HistoryLimits {
        entries: 1,
        bytes: 1024,
    });
    assert!(restored.history_bytes() <= 1024);
    assert_eq!(restored.history_provider().as_deref(), Some(id));
    drop(manager);
    restored.undo();
    assert_eq!(restored.text().to_string(), "abcdef");
    // The runtime worker supplies the same factory used during application startup.
    let runtime = unsafe { velk_plugin_host::Runtime::start(directory.path().into())? };
    let report = runtime
        .client()
        .load_selected(vec![id.to_owned()])?
        .recv_blocking()
        .map_err(|e| e.to_string())??;
    let mut document = Buffer::with_history(
        "runtime",
        report
            .undo
            .get(id)
            .cloned()
            .ok_or("missing runtime undo factory")?,
    );
    let browser = if pack {
        Some(browser_smoke(&runtime, document.new_view())?)
    } else {
        None
    };
    drop(runtime);
    if let Some(browser) = browser {
        browser.join().map_err(|_| "undo browser host panicked")?;
    }
    document.insert("new ");
    document.undo();
    assert_eq!(document.text().to_string(), "runtime");
    println!(
        "Undo service smoke passed: pack={pack}, exact patches, independent views, branches, RAM limits, pinned sessions after unload, worker snapshots after destruction, and restoration into a new native generation"
    );
    Ok(())
}

fn browser_smoke(
    runtime: &velk_plugin_host::Runtime,
    mut model: Buffer,
) -> Result<std::thread::JoinHandle<()>, String> {
    use velk_plugin_host::host::{self, Request, Response};
    let client = runtime.client();
    model.insert("browser ");
    let requests = client.host_requests();
    let responder = std::thread::spawn(move || {
        while let Ok(request) = requests.recv_blocking() {
            let info = host::Buffer {
                id: 1,
                title: "Undo smoke".into(),
                path: None,
                revision: model.revision(),
                dirty: model.is_dirty(),
                active: true,
                anchor: 0,
                head: 0,
            };
            let response = match request.request {
                Request::Buffers => Ok(Response::Buffers {
                    buffers: vec![info],
                }),
                Request::InspectHistory { id: 1 } => {
                    model.inspect_history().map(|history| Response::History {
                        buffer: info,
                        history,
                    })
                }
                Request::CheckoutHistory {
                    id: 1,
                    revision,
                    state,
                } => {
                    if model.checkout_history(revision, state) {
                        Ok(Response::Accepted)
                    } else {
                        Err("stale history".into())
                    }
                }
                _ => Err("unexpected undo browser request".into()),
            };
            request.respond(response);
        }
    });
    pollster::block_on(async {
        client
            .load_selected(vec!["velk.undo.browser".into()])?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        let show = client
            .bind_hook("velk.undo.browser".into(), "velk.undo.browser.show".into())?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        let outcome: velk_commands::Outcome =
            serde_json::from_slice(&client.call_hook(show, b"{}".to_vec()).await?)
                .map_err(|e| e.to_string())?;
        assert!(
            matches!(outcome, velk_commands::Outcome::ShowPanel { panel } if panel == "undo-tree")
        );
        let handle = client
            .bind_hook("velk.undo.browser".into(), velk_panels::HOOK.into())?
            .recv()
            .await
            .map_err(|e| e.to_string())??;
        let describe = velk_panels::encode(&velk_panels::Request::Describe {
            context: Default::default(),
        })?;
        let bytes = client.call_hook(handle.clone(), describe).await?;
        let velk_panels::Reply::Ready { snapshot } = velk_panels::decode(&bytes)? else {
            return Err("missing undo panel".into());
        };
        snapshot.validate()?;
        let velk_panels::Content::Tree(rows) = &snapshot.panels[0].content else {
            panic!()
        };
        assert!(rows.iter().any(|r| r.label.contains("[current]")));
        let bytes = client
            .call_hook(
                handle.clone(),
                velk_panels::encode(&velk_panels::Request::Changes)?,
            )
            .await?;
        assert!(matches!(
            velk_panels::decode::<velk_panels::Reply>(&bytes)?,
            velk_panels::Reply::Unchanged
        ));
        let bytes = client
            .call_hook(
                handle,
                velk_panels::encode(&velk_panels::Request::Invoke {
                    panel: "undo-tree".into(),
                    revision: snapshot.panels[0].revision,
                    node: "go-0".into(),
                    action: "checkout".into(),
                })?,
            )
            .await?;
        let velk_panels::Reply::Ready { snapshot } = velk_panels::decode(&bytes)? else {
            panic!()
        };
        snapshot.validate()?;
        let velk_panels::Content::Tree(rows) = &snapshot.panels[0].content else {
            panic!()
        };
        assert!(
            rows.iter()
                .find(|r| r.id == "0")
                .unwrap()
                .label
                .contains("[current]")
        );
        println!(
            "Native undo browser smoke passed: independent plugin, command, inspection, panel actions and refresh."
        );
        Ok::<_, String>(())
    })?;
    Ok(responder)
}
