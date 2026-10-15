use ropey::Rope;
use std::{path::Path, time::Duration};
use velk_plugin_lsp::lsp::LspBackend;
use velk_semantic::{DocumentSnapshot, Event, HoverRequest, Service, Status};

fn backend(log: &Path, mode: &str) -> LspBackend {
    LspBackend {
        executable: "python3".into(),
        arguments: vec![
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/server.py")
                .into_os_string(),
            log.as_os_str().to_owned(),
            mode.into(),
        ],
        initialization_options: serde_json::json!({"checkOnSave":false}),
        configuration: serde_json::json!({"rust-analyzer":{"checkOnSave":false}}),
        root: Some(log.parent().unwrap().to_path_buf()),
    }
}
fn document(path: &Path, revision: u64, text: &str) -> DocumentSnapshot {
    DocumentSnapshot {
        path: path.to_path_buf(),
        language: "rust".into(),
        revision,
        text: Rope::from_str(text),
    }
}
async fn until(events: &velk_semantic::Events, predicate: impl Fn(&Event) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events
                .recv()
                .await
                .expect("service ended before expected result");
            if let Event::Status(Status::Failed(message)) = &event {
                panic!("semantic backend failed: {message}");
            }
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .expect("semantic result timed out")
}
fn log(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[tokio::test(flavor = "current_thread")]
async fn lsp_sync_hover_cancellation_diagnostics_save_and_shutdown_work_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file 世界.rs");
    let logfile = dir.path().join("messages.jsonl");
    let service = Service::start(backend(&logfile, "normal")).unwrap();
    let source = "fn café() { let text = \"👩‍💻\"; }\n";
    service.update(document(&path, 0, source));
    until(&service.events, |e| {
        matches!(e, Event::Status(Status::Ready(_)))
    })
    .await;
    let event = until(&service.events, |e| {
        matches!(e, Event::Diagnostics { revision: 0, .. })
    })
    .await;
    let Event::Diagnostics { diagnostics, .. } = event else {
        unreachable!()
    };
    assert_eq!(diagnostics[0].range, 3..7);
    // Coalescing can skip revisions; the adapter derives one correct incremental
    // replacement between the last sent and newest Rope snapshots.
    for revision in 1..100 {
        service.update(document(
            &path,
            revision,
            &format!("{source}// {revision} 世界\n"),
        ));
    }
    let newest = format!("{source}// 99 世界\n");
    let event = until(&service.events, |e| {
        matches!(e, Event::Diagnostics { revision: 99, .. })
    })
    .await;
    let Event::Diagnostics { diagnostics, .. } = event else {
        unreachable!()
    };
    assert_eq!(diagnostics[0].message, "current");
    service.hover(HoverRequest {
        id: 1,
        revision: 99,
        position: 4,
    });
    // Wait for the first request to reach the peer before superseding it.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if log(&logfile)
                .iter()
                .any(|v| v["method"] == "textDocument/hover")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    service.hover(HoverRequest {
        id: 2,
        revision: 99,
        position: 5,
    });
    let event = until(&service.events, |e| {
        matches!(
            e,
            Event::Hover {
                request: HoverRequest { id: 2, .. },
                ..
            }
        )
    })
    .await;
    let Event::Hover {
        hover: Some(hover), ..
    } = event
    else {
        panic!("missing hover")
    };
    assert_eq!(hover.contents, "CURRENT");
    assert!(!hover.markdown);
    service.saved(document(&path, 99, &newest));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if log(&logfile)
                .iter()
                .any(|v| v["method"] == "textDocument/didSave")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Coalescing an edit and its undo can produce identical content at a newer
    // revision. It must advance the version without a full-document payload.
    service.update(document(&path, 100, &newest));
    until(&service.events, |e| {
        matches!(e, Event::Diagnostics { revision: 100, .. })
    })
    .await;
    let events = service.events.clone();
    drop(service);
    until(&events, |e| matches!(e, Event::Status(Status::Stopped))).await;
    let messages = log(&logfile);
    let last_change = messages
        .iter()
        .rev()
        .find(|v| v["method"] == "textDocument/didChange")
        .unwrap();
    assert_eq!(last_change["params"]["contentChanges"][0]["text"], "");
    assert_eq!(
        last_change["params"]["contentChanges"][0]["range"]["start"]["character"],
        0
    );
    assert!(messages.iter().any(|v| v["reconstructed"] == newest));
    assert!(
        messages
            .iter()
            .any(|v| v["id"] == "config" && v["result"][0]["checkOnSave"] == false)
    );
    assert!(
        messages
            .iter()
            .any(|v| v["id"] == "edit" && v["result"]["applied"] == false)
    );
    assert!(messages.iter().any(|v| v["method"] == "$/cancelRequest"));
    assert!(
        messages
            .iter()
            .any(|v| v["method"] == "textDocument/didClose")
    );
    assert!(messages.iter().any(|v| v["method"] == "shutdown"));
    assert!(messages.iter().any(|v| v["method"] == "exit"));
    assert!(
        messages
            .iter()
            .any(|v| v["method"] == "textDocument/didSave" && v["params"]["text"] == newest)
    );
}
#[tokio::test(flavor = "current_thread")]
async fn hover_deadline_reports_failure_and_drop_cancels_stalled_initialization() {
    for mode in ["timeout", "stall-init"] {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("messages.jsonl");
        let service = Service::start(backend(&logfile, mode)).unwrap();
        service.update(document(&dir.path().join("file.rs"), 0, "fn main() {}"));
        if mode == "timeout" {
            until(&service.events, |e| {
                matches!(e, Event::Status(Status::Ready(_)))
            })
            .await;
            service.hover(HoverRequest {
                id: 1,
                revision: 0,
                position: 4,
            });
            until(
                &service.events,
                |e| matches!(e,Event::RequestFailed {message,..} if message.contains("timed out")),
            )
            .await;
        } else {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !logfile.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        }
        let events = service.events.clone();
        drop(service);
        until(&events, |e| matches!(e, Event::Status(Status::Stopped))).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn missing_server_is_an_async_failure() {
    let mut config = LspBackend::rust_analyzer();
    config.executable = "/velk-test-nonexistent-language-server".into();
    let service = Service::start(config).unwrap();
    service.update(document(Path::new("/tmp/file.rs"), 0, "fn main() {}"));
    let event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = service.events.recv().await.unwrap();
            if matches!(event, Event::Status(Status::Failed(_))) {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(event,Event::Status(Status::Failed(message)) if message.contains("Cannot start"))
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires rust-analyzer and rust-src; exercises a real Cargo project"]
async fn real_rust_analyzer_hover_uses_unsaved_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src/main.rs");
    std::fs::create_dir(path.parent().unwrap()).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"velk_semantic_test\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let source = "fn main() { let value: usize = 1; }\n";
    std::fs::write(&path, source).unwrap();
    let service = Service::start(LspBackend::rust_analyzer()).unwrap();
    service.update(document(&path, 0, source));
    until(&service.events, |e| {
        matches!(e, Event::Status(Status::Ready(_)))
    })
    .await;
    let updated = "fn main() { let value: i64 = 1; }\n";
    service.update(document(&path, 1, updated));
    let mut found = false;
    for id in 1..=30 {
        service.hover(HoverRequest {
            id,
            revision: 1,
            position: 17,
        });
        let event = until(&service.events, |e| {
            matches!(e,Event::Hover {request,..} if request.id == id)
                || matches!(e,Event::RequestFailed {request,..} if request.id == id)
        })
        .await;
        if let Event::Hover {
            hover: Some(hover), ..
        } = event
            && hover.contents.contains("i64")
        {
            println!("real rust-analyzer hover: {}", hover.contents);
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(
        found,
        "rust-analyzer did not produce the unsaved variable's type"
    );
    let events = service.events.clone();
    drop(service);
    until(&events, |e| matches!(e, Event::Status(Status::Stopped))).await;
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}
