use ropey::Rope;
use std::{path::Path, time::Duration};
use velk_plugin_lsp::lsp::LspBackend;
use velk_semantic::{DocumentSnapshot, Event, PositionRequest, Service, Status};

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
    service.hover(PositionRequest {
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
    service.hover(PositionRequest {
        id: 2,
        revision: 99,
        position: 5,
    });
    let event = until(&service.events, |e| {
        matches!(
            e,
            Event::Hover {
                request: PositionRequest { id: 2, .. },
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
            service.hover(PositionRequest {
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
    let config = LspBackend {
        executable: "/velk-test-nonexistent-language-server".into(),
        arguments: vec![],
        root: None,
        initialization_options: serde_json::Value::Null,
        configuration: serde_json::Value::Null,
    };
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
    let service = Service::start(LspBackend {
        executable: std::env::var_os("VELK_RUST_ANALYZER")
            .unwrap_or_else(|| "rust-analyzer".into()),
        arguments: vec![],
        root: None,
        initialization_options: serde_json::json!({"checkOnSave":false}),
        configuration: serde_json::json!({"rust-analyzer":{"checkOnSave":false}}),
    })
    .unwrap();
    service.update(document(&path, 0, source));
    until(&service.events, |e| {
        matches!(e, Event::Status(Status::Ready(_)))
    })
    .await;
    let updated = "fn main() { let value: i64 = 1; }\n";
    service.update(document(&path, 1, updated));
    let mut found = false;
    for id in 1..=30 {
        service.hover(PositionRequest {
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

#[tokio::test(flavor = "current_thread")]
async fn declared_navigation_and_rust_commands_cross_the_backend_with_negotiated_positions() {
    use velk_plugin_sdk::{
        Plugin,
        commands::{Encoding, Handler, Outcome},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("command 世界.rs");
    let logfile = dir.path().join("commands.jsonl");
    let service = Service::start(backend(&logfile, "normal")).unwrap();
    service.update(document(&path, 9, "a𐐀界 target"));
    until(&service.events, |e| {
        matches!(e, Event::Status(Status::Ready(_)))
    })
    .await;
    let commands = velk_plugin_lsp::Lsp::commands()
        .into_iter()
        .chain(velk_plugin_rust_analyzer::RustAnalyzer::commands());
    for (id, command) in commands
        .filter(|command| {
            !matches!(
                command.id.as_str(),
                "velk.lsp.format"
                    | "velk.lsp.rename"
                    | "velk.lsp.code-actions"
                    | "velk.lsp.complete"
            )
        })
        .enumerate()
    {
        let Handler::Semantic { payload } = command.handler else {
            panic!("expected semantic command");
        };
        let request = PositionRequest {
            id: id as u64,
            revision: 9,
            position: 3,
        };
        service.command(request, payload, None).unwrap();
        let Event::Command {
            request: reply,
            result,
        } = until(&service.events, |e| matches!(e, Event::Command { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(reply, request);
        match result.unwrap() {
            Outcome::Locations { locations } => {
                assert_eq!(locations[0].path, path);
                assert_eq!(locations[0].position.encoding, Encoding::Utf16);
                assert_eq!(locations[0].position.column, 4);
            }
            Outcome::Document { text, .. } => assert_eq!(text, "fn expanded() {}"),
            _ => panic!("unexpected command result"),
        }
    }
    service.command(PositionRequest { id:99, revision:9, position:3 }, serde_json::json!({"method":"textDocument/declaration","result":"locations","capability":"/declarationProvider"}), None).unwrap();
    assert!(matches!(
        until(&service.events, |e| matches!(e, Event::Command { .. })).await,
        Event::Command { result: Err(_), .. }
    ));
    service.shutdown_and_join().unwrap();
    let messages = log(&logfile);
    let request = messages
        .iter()
        .find(|m| m["method"] == "textDocument/definition")
        .unwrap();
    assert_eq!(request["params"]["position"]["character"], 4);
    assert!(
        messages
            .iter()
            .any(|m| m["method"] == "textDocument/references"
                && m["params"]["context"]["includeDeclaration"] == true)
    );
    assert!(
        !messages
            .iter()
            .any(|m| m["method"] == "textDocument/declaration")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn source_changes_cancel_commands_without_interfering_with_hover() {
    let dir = tempfile::tempdir().unwrap();
    let logfile = dir.path().join("cancel.jsonl");
    let path = dir.path().join("source.rs");
    let service = Service::start(backend(&logfile, "hold-command")).unwrap();
    service.update(document(&path, 0, "source"));
    until(&service.events, |e| {
        matches!(e, Event::Status(Status::Ready(_)))
    })
    .await;
    let request = PositionRequest {
        id: 30,
        revision: 0,
        position: 1,
    };
    service.command(request, serde_json::json!({"method":"textDocument/definition","result":"locations","capability":"/definitionProvider"}), None).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !log(&logfile)
            .iter()
            .any(|m| m["method"] == "textDocument/definition")
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    service.hover(PositionRequest { id: 31, ..request });
    service.cancel_hover();
    assert!(
        service
            .command(request, serde_json::json!({}), None)
            .is_err()
    );
    service.update(document(&path, 1, "changed"));
    assert!(
        matches!(until(&service.events, |e| matches!(e, Event::Command { .. })).await, Event::Command { request:r, result:Err(message) } if r == request && message.contains("cancelled"))
    );
    service.shutdown_and_join().unwrap();
    let messages = log(&logfile);
    let id = &messages
        .iter()
        .find(|m| m["method"] == "textDocument/definition")
        .unwrap()["id"];
    assert!(
        messages
            .iter()
            .any(|m| m["method"] == "$/cancelRequest" && &m["params"]["id"] == id)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn formatting_rename_actions_and_completion_return_checked_edit_proposals() {
    use velk_plugin_sdk::{
        Plugin,
        commands::{ChoiceAction, Handler, Outcome},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.rs");
    let logfile = dir.path().join("edits.jsonl");
    let service = Service::start(backend(&logfile, "normal")).unwrap();
    service.update(document(&path, 42, "fn old() {}"));
    until(&service.events, |e| {
        matches!(e, Event::Status(Status::Ready(_)))
    })
    .await;
    let mut id = 100;
    for name in ["format", "rename", "code-actions", "complete"] {
        let declaration = velk_plugin_lsp::Lsp::commands()
            .into_iter()
            .chain(
                velk_plugin_lsp::Lsp::contributions()
                    .into_iter()
                    .map(|c| c.command),
            )
            .find(|d| d.id == format!("velk.lsp.{name}"))
            .unwrap();
        let Handler::Semantic { payload } = declaration.handler else {
            unreachable!()
        };
        let request = PositionRequest {
            id,
            revision: 42,
            position: 6,
        };
        id += 1;
        service
            .command(
                request,
                payload,
                declaration.input.map(|_| "renamed".into()),
            )
            .unwrap();
        let Event::Command { result, .. } =
            until(&service.events, |e| matches!(e, Event::Command { .. })).await
        else {
            unreachable!()
        };
        let result = match result.unwrap() {
            Outcome::Completions { items } => {
                assert_eq!(items[0].filter_text, "call");
                assert_eq!(items[0].sort_text, "001");
                assert!(!items[0].id.is_empty());
                Outcome::Choices {
                    title: "Completions".into(),
                    items: items.into_iter().map(|item| item.choice).collect(),
                }
            }
            outcome => outcome,
        };
        match result {
            Outcome::Edit { edit } => {
                assert_eq!(edit.documents[0].revision, Some(42));
                if name == "rename" {
                    assert_eq!(edit.documents.len(), 2);
                    assert_eq!(edit.documents[1].revision, None);
                    assert_eq!(edit.documents[0].edits[0].text, "renamed");
                } else {
                    assert_eq!(edit.documents[0].edits[0].text, "// formatted\n");
                }
            }
            Outcome::Choices { items, .. } => {
                assert_eq!(items.len(), 2);
                for item in items {
                    if let ChoiceAction::Snippet { snippet } = &item.action {
                        snippet.validate().unwrap();
                        assert_eq!(snippet.edit.documents[0].edits[0].text, "call(value)");
                        assert_eq!(snippet.stops[0].range, 5..10);
                        continue;
                    }
                    let ChoiceAction::Semantic { payload } = item.action else {
                        continue;
                    };
                    service
                        .command(PositionRequest { id, ..request }, payload, None)
                        .unwrap();
                    id += 1;
                    let Event::Command { result, .. } =
                        until(&service.events, |e| matches!(e, Event::Command { .. })).await
                    else {
                        unreachable!()
                    };
                    let Outcome::Edit { edit } = result.unwrap() else {
                        panic!("resolve returned no edit")
                    };
                    assert_eq!(edit.documents[0].revision, Some(42));
                    assert_eq!(
                        edit.documents[0].edits.len(),
                        if name == "complete" { 2 } else { 1 }
                    );
                }
            }
            _ => panic!("unexpected edit command result"),
        }
    }
    // Invalid user input is a command failure, not a failed server session.
    service.command(PositionRequest {id,revision:42,position:6},serde_json::json!({"method":"textDocument/rename","result":"workspace_edit","capability":"/renameProvider"}),None).unwrap();
    assert!(matches!(
        until(&service.events, |e| matches!(e, Event::Command { .. })).await,
        Event::Command { result: Err(_), .. }
    ));
    service.shutdown_and_join().unwrap();
    let messages = log(&logfile);
    assert!(
        messages
            .iter()
            .any(|m| m["method"] == "textDocument/rename" && m["params"]["newName"] == "renamed")
    );
    assert!(messages.iter().any(|m| {
        m["method"] == "textDocument/codeAction"
            && !m["params"]["context"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
    }));
    let init = &messages
        .iter()
        .find(|m| m["method"] == "initialize")
        .unwrap()["params"]["capabilities"];
    assert_eq!(
        init["textDocument"]["completion"]["completionItem"]["snippetSupport"],
        true
    );
}

struct SharedBackend {
    client: std::sync::Arc<velk_plugin_lsp::lsp::Client>,
    backend: LspBackend,
}
impl velk_semantic::Backend for SharedBackend {
    fn run(self: Box<Self>, session: velk_semantic::Session) -> velk_semantic::BackendFuture {
        Box::pin(async move { self.client.run(self.backend, session).await })
    }
}
fn shared(
    client: &std::sync::Arc<velk_plugin_lsp::lsp::Client>,
    logfile: &Path,
    path: &Path,
    revision: u64,
) -> std::sync::Arc<Service> {
    let service = Service::start(SharedBackend {
        client: client.clone(),
        backend: backend(logfile, "normal"),
    })
    .unwrap();
    service.update(document(path, revision, "fn hello() {}\n"));
    service
}
async fn ready(service: &Service) {
    until(&service.events, |e| {
        matches!(e, Event::Status(Status::Ready(_)))
    })
    .await;
}
async fn definition(service: &Service, revision: u64) -> std::path::PathBuf {
    service.command(PositionRequest { id: 1, revision, position: 4 }, serde_json::json!({"method":"textDocument/definition","result":"locations","capability":"/definitionProvider"}), None).unwrap();
    let Event::Command {
        result: Ok(velk_plugin_sdk::commands::Outcome::Locations { locations }),
        ..
    } = until(&service.events, |e| matches!(e, Event::Command { .. })).await
    else {
        panic!("definition failed")
    };
    locations[0].path.clone()
}
#[tokio::test(flavor = "current_thread")]
async fn workspace_client_multiplexes_documents_reuses_idle_servers_and_retires_them() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    let logfile = dir.path().join("workspace.jsonl");
    let client = std::sync::Arc::new(velk_plugin_lsp::lsp::Client::default());
    let first = shared(&client, &logfile, &a, 0);
    let second = shared(&client, &logfile, &b, 0);
    ready(&first).await;
    ready(&second).await;
    until(&first.events, |e| matches!(e, Event::Diagnostics { .. })).await;
    until(&second.events, |e| matches!(e, Event::Diagnostics { .. })).await;
    let messages = log(&logfile);
    assert_eq!(
        messages
            .iter()
            .filter(|v| v["method"] == "initialize")
            .count(),
        1
    );
    assert_eq!(
        messages
            .iter()
            .filter(|v| v["method"] == "textDocument/didOpen")
            .count(),
        2
    );
    let (target_a, target_b) = tokio::join!(definition(&first, 0), definition(&second, 0));
    assert_eq!(target_a, a);
    assert_eq!(target_b, b);
    first.update(document(&a, 2, "fn changed() {}\n"));
    until(&first.events, |e| {
        matches!(e, Event::Diagnostics { revision: 2, .. })
    })
    .await;
    assert_eq!(definition(&second, 0).await, b);
    first.shutdown_and_join().unwrap();
    assert_eq!(definition(&second, 0).await, b);
    second.shutdown_and_join().unwrap();
    assert!(!log(&logfile).iter().any(|v| v["method"] == "shutdown"));
    let reopened = shared(&client, &logfile, &a, 7);
    ready(&reopened).await;
    until(&reopened.events, |e| {
        matches!(e, Event::Diagnostics { revision: 7, .. })
    })
    .await;
    assert_eq!(definition(&reopened, 7).await, a);
    assert_eq!(
        log(&logfile)
            .iter()
            .filter(|v| v["method"] == "initialize")
            .count(),
        1
    );
    reopened.retire_provider();
    reopened.shutdown_and_join().unwrap();
    assert!(log(&logfile).iter().any(|v| v["method"] == "shutdown"));
    assert!(log(&logfile).iter().any(|v| v["method"] == "exit"));
    let restarted = shared(&client, &logfile, &b, 4);
    ready(&restarted).await;
    assert_eq!(
        log(&logfile)
            .iter()
            .filter(|v| v["method"] == "initialize")
            .count(),
        2
    );
    restarted.shutdown_and_join().unwrap();
    drop(client);
    assert_eq!(
        log(&logfile)
            .iter()
            .filter(|v| v["method"] == "exit")
            .count(),
        2
    );
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_clients_isolate_roots_and_configuration_and_retire_idle_only() {
    let dir = tempfile::tempdir().unwrap();
    let root_b = dir.path().join("other");
    std::fs::create_dir(&root_b).unwrap();
    let logfile = dir.path().join("isolated.jsonl");
    let client = std::sync::Arc::new(velk_plugin_lsp::lsp::Client::default());
    let mut services = Vec::new();
    for index in 0..3 {
        let mut config = backend(&logfile, "normal");
        if index == 1 {
            config.root = Some(root_b.clone());
        }
        if index == 2 {
            config.configuration = serde_json::json!({"other":true});
        }
        let service = Service::start(SharedBackend {
            client: client.clone(),
            backend: config,
        })
        .unwrap();
        service.update(document(
            &dir.path().join(format!("file-{index}.rs")),
            0,
            "fn hello() {}\n",
        ));
        ready(&service).await;
        until(&service.events, |e| matches!(e, Event::Diagnostics { .. })).await;
        services.push(service);
    }
    assert_eq!(
        log(&logfile)
            .iter()
            .filter(|v| v["method"] == "initialize")
            .count(),
        3
    );
    client.retire_idle();
    assert_eq!(
        definition(&services[0], 0).await,
        dir.path().join("file-0.rs")
    );
    for service in services {
        service.shutdown_and_join().unwrap();
    }
    client.retire_idle();
    assert_eq!(
        log(&logfile)
            .iter()
            .filter(|v| v["method"] == "exit")
            .count(),
        3
    );
}

#[tokio::test(flavor = "current_thread")]
async fn closing_a_document_during_shared_initialization_detaches_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let logfile = dir.path().join("initializing.jsonl");
    let client = std::sync::Arc::new(velk_plugin_lsp::lsp::Client::default());
    let service = Service::start(SharedBackend {
        client: client.clone(),
        backend: backend(&logfile, "stall-init"),
    })
    .unwrap();
    service.update(document(&dir.path().join("file.rs"), 0, "fn hello() {}\n"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !logfile.exists() || !log(&logfile).iter().any(|v| v["method"] == "initialize") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let started = std::time::Instant::now();
    client.retire_idle();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "an initializing attachment is active, not idle"
    );
    let started = std::time::Instant::now();
    service.shutdown_and_join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    client.retire_idle();
}

#[tokio::test(flavor = "current_thread")]
async fn pull_diagnostics_negotiate_cache_refresh_and_clear_after_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.rs");
    let logfile = dir.path().join("pull.jsonl");
    let service = Service::start(backend(&logfile, "pull")).unwrap();
    let source = "fn café() {}\n";
    service.update(document(&path, 1, source));
    let Event::Diagnostics { diagnostics, .. } = until(&service.events, |e| {
        matches!(e, Event::Diagnostics { revision: 1, diagnostics } if diagnostics.iter().any(|d| d.message == "pulled"))
    })
    .await
    else {
        panic!()
    };
    assert!(diagnostics.iter().any(|d| d.message == "pulled"));
    assert!(
        diagnostics.iter().any(|d| d.message == "current"),
        "push compiler diagnostics coexist with pulled native diagnostics"
    );
    assert_eq!(diagnostics[0].range, 3..7);
    service.saved(document(&path, 1, source));
    let Event::Diagnostics { diagnostics, .. } = until(&service.events, |e| {
        matches!(e, Event::Diagnostics { revision: 1, diagnostics } if diagnostics.iter().any(|d| d.message == "pulled"))
    })
    .await
    else {
        panic!()
    };
    assert!(
        diagnostics.iter().any(|d| d.message == "pulled"),
        "unchanged reports retain their full cache"
    );
    service.update(document(&path, 2, "fn fixed() {}\n"));
    let Event::Diagnostics { diagnostics, .. } = until(&service.events, |e| {
        matches!(e, Event::Diagnostics { revision: 2, .. })
    })
    .await
    else {
        panic!()
    };
    assert!(diagnostics.is_empty());
    service.shutdown_and_join().unwrap();
    let messages = log(&logfile);
    let pulls: Vec<_> = messages
        .iter()
        .filter(|v| v["method"] == "textDocument/diagnostic")
        .collect();
    assert!(pulls.len() >= 3);
    assert_eq!(pulls[0]["params"]["identifier"], "fixture");
    assert!(pulls[0]["params"].get("previousResultId").is_none());
    assert!(
        pulls
            .iter()
            .skip(1)
            .any(|v| v["params"]["previousResultId"] == "r1")
    );
    assert!(
        messages
            .iter()
            .any(|v| v["id"] == "refresh-1" && v.get("result").is_some())
    );
    assert!(messages.iter().any(|v| v["method"] == "shutdown"));
}

#[tokio::test(flavor = "current_thread")]
async fn edits_cancel_pull_requests_and_ignore_late_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.rs");
    let logfile = dir.path().join("pull-stale.jsonl");
    let service = Service::start(backend(&logfile, "pull-stale")).unwrap();
    service.update(document(&path, 1, "fn café() {}\n"));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if logfile.exists()
                && log(&logfile)
                    .iter()
                    .any(|v| v["method"] == "textDocument/diagnostic")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    service.update(document(&path, 2, "fn café() { }\n"));
    let Event::Diagnostics {
        revision,
        diagnostics,
    } = until(&service.events, |e| matches!(e, Event::Diagnostics { .. })).await
    else {
        panic!()
    };
    assert_eq!(revision, 2);
    assert!(diagnostics.iter().all(|d| d.message == "pulled"));
    service.shutdown_and_join().unwrap();
    assert!(
        log(&logfile)
            .iter()
            .any(|v| v["method"] == "$/cancelRequest")
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires rust-analyzer and rust-src; runs a real Cargo build script"]
async fn real_rust_analyzer_shutdown_releases_an_active_cargo_build_lock() {
    real_cargo_shutdown(false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires rust-analyzer and rust-src; suspends a server during a real Cargo check"]
async fn stopped_rust_analyzer_shutdown_releases_an_active_cargo_build_lock() {
    real_cargo_shutdown(true).await;
}

#[cfg(unix)]
async fn real_cargo_shutdown(stop_server: bool) {
    use rustix::fs::{FlockOperation, flock};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"shutdown-lock-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn example() {}\n").unwrap();
    std::fs::write(
        root.join("build.rs"),
        r#"fn main() {
        std::fs::write("build-running", std::process::id().to_string()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
    }"#,
    )
    .unwrap();
    let service = Service::start(LspBackend {
        executable: "python3".into(),
        arguments: vec!["-c".into(), "import os,pathlib; pathlib.Path('server-pid').write_text(str(os.getpid())); os.execvp('rust-analyzer',['rust-analyzer'])".into()],
        root: Some(root.clone()),
        initialization_options: serde_json::json!({"checkOnSave":true}),
        configuration: serde_json::json!({"rust-analyzer":{"checkOnSave":true}}),
    })
    .unwrap();
    service.update(document(
        &root.join("src/lib.rs"),
        0,
        "pub fn example() {}\n",
    ));
    until(&service.events, |event| {
        matches!(event, Event::Status(Status::Ready(_)))
    })
    .await;
    let lock = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if root.join("build-running").exists()
                && let Ok(lock) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(root.join("target/debug/.cargo-lock"))
            {
                match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
                    Err(rustix::io::Errno::WOULDBLOCK) => break lock,
                    Ok(()) => flock(&lock, FlockOperation::Unlock).unwrap(),
                    Err(error) => panic!("cannot inspect Cargo lock: {error}"),
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("rust-analyzer did not start the build-script fixture");
    if stop_server {
        let pid = std::fs::read_to_string(root.join("server-pid"))
            .unwrap()
            .parse()
            .unwrap();
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::Signal::STOP,
        )
        .unwrap();
    }
    service.shutdown_and_join().unwrap();
    let released = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if flock(&lock, FlockOperation::NonBlockingLockExclusive).is_ok() {
                flock(&lock, FlockOperation::Unlock).unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok();
    if !released
        && let Ok(pid) = std::fs::read_to_string(root.join("build-running"))
        && let Ok(pid) = pid.parse::<i32>()
        && let Some(pid) = rustix::process::Pid::from_raw(pid)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
    assert!(
        released,
        "Cargo retained its build lock after rust-analyzer shutdown"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completion_triggers_use_server_capabilities_and_forward_the_invocation_reason() {
    use velk_plugin_sdk::{
        Plugin,
        commands::{Handler, Outcome},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.rs");
    let logfile = dir.path().join("completion-triggers.jsonl");
    let service = Service::start(backend(&logfile, "normal")).unwrap();
    service.update(document(&path, 1, "fn old() {}"));
    until(&service.events, |event| {
        matches!(event, Event::Status(Status::Ready(_)))
    })
    .await;
    let Handler::Semantic { payload } = velk_plugin_lsp::Lsp::contributions()
        .remove(0)
        .command
        .handler
    else {
        panic!()
    };
    for (id, character, expected) in [(1, ".", true), (2, ":", true), (3, "!", false)] {
        let input = serde_json::to_string(&velk_completion_contracts::Invocation::Character {
            character: character.into(),
        })
        .unwrap();
        service
            .command(
                PositionRequest {
                    id,
                    revision: 1,
                    position: 6,
                },
                payload.clone(),
                Some(input),
            )
            .unwrap();
        let Event::Command { result, .. } = until(&service.events, |event| {
            matches!(event, Event::Command { .. })
        })
        .await
        else {
            panic!()
        };
        let Outcome::Completions { items } = result.unwrap() else {
            panic!()
        };
        assert_eq!(!items.is_empty(), expected);
    }
    service.shutdown_and_join().unwrap();
    let messages = log(&logfile);
    let requests: Vec<_> = messages
        .iter()
        .filter(|message| message["method"] == "textDocument/completion")
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]["params"]["context"],
        serde_json::json!({"triggerKind":2,"triggerCharacter":"."})
    );
    assert_eq!(
        requests[1]["params"]["context"],
        serde_json::json!({"triggerKind":2,"triggerCharacter":":"})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn selected_completion_documentation_resolves_without_returning_or_applying_edits() {
    use velk_plugin_sdk::{
        Plugin,
        commands::{Handler, Outcome},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.rs");
    let logfile = dir.path().join("completion-docs.jsonl");
    let service = Service::start(backend(&logfile, "normal")).unwrap();
    service.update(document(&path, 7, "fn old() {}"));
    until(&service.events, |event| {
        matches!(event, Event::Status(Status::Ready(_)))
    })
    .await;
    let Handler::Semantic { payload } = velk_plugin_lsp::Lsp::contributions()
        .remove(0)
        .command
        .handler
    else {
        panic!()
    };
    service
        .command(
            PositionRequest {
                id: 1,
                revision: 7,
                position: 6,
            },
            payload,
            None,
        )
        .unwrap();
    let Event::Command {
        result: Ok(Outcome::Completions { mut items }),
        ..
    } = until(&service.events, |event| {
        matches!(event, Event::Command { .. })
    })
    .await
    else {
        panic!()
    };
    let item = items.remove(0);
    assert!(item.documentation.is_none());
    service
        .command(
            PositionRequest {
                id: 2,
                revision: 7,
                position: 6,
            },
            item.documentation_resolve.unwrap(),
            None,
        )
        .unwrap();
    let Event::Command { result, .. } = until(&service.events, |event| {
        matches!(event, Event::Command { .. })
    })
    .await
    else {
        panic!()
    };
    let Outcome::Document { text, language, .. } = result.unwrap() else {
        panic!("documentation resolution returned an edit")
    };
    assert_eq!(language.as_deref(), Some("markdown"));
    assert!(text.contains("**Complete a call**"));
    // Accepting remains a separate resolve, with checked edits including imports.
    let velk_plugin_sdk::commands::ChoiceAction::Semantic { payload } = item.choice.action else {
        panic!()
    };
    service
        .command(
            PositionRequest {
                id: 3,
                revision: 7,
                position: 6,
            },
            payload,
            None,
        )
        .unwrap();
    let Event::Command {
        result: Ok(Outcome::Edit { edit }),
        ..
    } = until(&service.events, |event| {
        matches!(event, Event::Command { .. })
    })
    .await
    else {
        panic!()
    };
    assert_eq!(edit.documents[0].revision, Some(7));
    assert_eq!(edit.documents[0].edits.len(), 2);
    service.shutdown_and_join().unwrap();
    let messages = log(&logfile);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["method"] == "completionItem/resolve")
            .count(),
        2
    );
    let capabilities = &messages
        .iter()
        .find(|message| message["method"] == "initialize")
        .unwrap()["params"]["capabilities"];
    assert_eq!(
        capabilities["textDocument"]["completion"]["completionItem"]["documentationFormat"],
        serde_json::json!(["markdown", "plaintext"])
    );
}
