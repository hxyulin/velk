use super::*;
use velk_core::{Buffer, Selection, TextEdit, Transaction};

#[allow(unsafe_code)]
pub(super) fn run(output: &Path, pack: bool) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let id = velk_core::history::DEFAULT_PROVIDER;
    let specs = if pack {
        stage_builtins(output, directory.path())?
            .into_iter()
            .filter(|spec| spec.id == id)
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
    drop(runtime);
    document.insert("new ");
    document.undo();
    assert_eq!(document.text().to_string(), "runtime");
    println!(
        "Undo service smoke passed: pack={pack}, exact patches, independent views, branches, RAM limits, pinned sessions after unload, worker snapshots after destruction, and restoration into a new native generation"
    );
    Ok(())
}
