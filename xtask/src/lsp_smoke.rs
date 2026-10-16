//! Exercise the actual LSP DSO in a child harness so server configuration never
//! mutates the parent process environment or reaches normal editor arguments.
use super::*;
use std::time::Duration;
use velk_semantic::{DocumentSnapshot, Event, Events, PositionRequest, Status};

pub(super) fn run(root: &Path, output: &Path) -> Result<(), String> {
    run_with_format(root, output, false)
}
pub(super) fn run_pack(root: &Path, output: &Path) -> Result<(), String> {
    run_with_format(root, output, true)
}
fn run_with_format(root: &Path, output: &Path, pack: bool) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let directory = temp.path().join("plugins");
    let (markdown, lsp, rust) = if pack {
        let specs = stage_builtins(output, &directory)?;
        (specs[0].clone(), specs[1].clone(), specs[2].clone())
    } else {
        (
            stage(output, &directory)?,
            stage_plugin(output, &directory, "velk.lsp", "velk_plugin_lsp")?,
            stage_plugin(
                output,
                &directory,
                "velk.language-server.rust-analyzer",
                "velk_plugin_rust_analyzer",
            )?,
        )
    };
    let source = temp.path().join("file 世界.rs");
    std::fs::write(&source, "fn café() {}\n").map_err(|e| e.to_string())?;
    std::fs::write(temp.path().join("unopened.rs"), "fn broken() {}\n")
        .map_err(|e| e.to_string())?;
    for mode in [
        "normal",
        "workspace",
        "runtime-stop",
        "stall-init",
        "no-profile",
        "missing-transport",
    ] {
        let specs = match mode {
            "no-profile" => vec![markdown.clone(), lsp.clone()],
            "missing-transport" => vec![markdown.clone(), rust.clone()],
            _ => vec![markdown.clone(), lsp.clone(), rust.clone()],
        };
        publish(&directory, specs)?;
        let log = temp.path().join(format!("{mode}.jsonl"));
        let config = temp.path().join(format!("{mode}.json"));
        std::fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "executable":"python3",
                "arguments":[root.join("plugins/lsp/tests/fixtures/server.py"),log,mode],
                "root":temp.path(),
                "initialization_options":{"checkOnSave":false},
                "configuration":{"rust-analyzer":{"checkOnSave":false}}
            }))
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        checked(
            Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .arg("__test-lsp")
                .arg(&directory)
                .arg(&source)
                .arg(&log)
                .env("VELK_LSP_CONFIG", config)
                .env("VELK_LSP_SMOKE_MODE", mode),
        )?;
    }
    println!(
        "LSP plugin smoke passed: unopened-file workspace publications/empty clearing/provider retirement, native async diagnostics/hover, declared navigation/Rust commands, cancellation, save, unload during requests/initialization, runtime exit with live clients, process reaping, latest-document reattachment, resident image reuse, independent Markdown, namespaced Rust definition dependency/unload/reload, no server without its profile or transport."
    );
    Ok(())
}
fn snapshot(path: &Path, revision: u64, text: &str) -> DocumentSnapshot {
    DocumentSnapshot {
        path: path.into(),
        language: "rust".into(),
        revision,
        text: ropey::Rope::from_str(text),
    }
}
async fn until(events: &Events, predicate: impl Fn(&Event) -> bool) -> Result<Event, String> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.map_err(|e| e.to_string())?;
            if predicate(&event) {
                return Ok(event);
            }
            if let Event::Status(Status::Failed(message)) = &event {
                return Err(message.clone());
            }
        }
    })
    .await
    .map_err(|_| "LSP plugin event timed out".to_string())?
}
fn messages(log: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
async fn logged(
    log: &Path,
    predicate: impl Fn(&[serde_json::Value]) -> bool,
) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate(&messages(log)) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| "LSP plugin peer log timed out".into())
}
#[cfg(unix)]
fn assert_reaped(log: &Path) {
    for pid in messages(log).iter().filter_map(|v| v["pid"].as_u64()) {
        // A test-only process query. The plugin's joining teardown must have
        // waited/reaped its peer before the unload report was delivered.
        assert!(
            !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success(),
            "language server {pid} survived unload"
        );
    }
}
#[cfg(not(unix))]
fn assert_reaped(_: &Path) {}

#[allow(unsafe_code)]
pub(super) fn child(directory: &Path, path: &Path, log: &Path) -> Result<(), String> {
    let mut diagnostics = if std::env::var("VELK_LSP_SMOKE_MODE").as_deref() == Ok("runtime-stop") {
        Some(velk_diagnostics::init_with(velk_diagnostics::Config {
            directory: log.parent().unwrap().join("traces"),
            duration: Duration::from_secs(10),
            filter: "off,velk_plugin=info".into(),
            json_logs: true,
            startup_capture: false,
            targets: vec![],
            log_directory: Some(log.parent().unwrap().join("logs")),
        })?)
    } else {
        None
    };
    // SAFETY: parent staged only trusted SDK-based local plugins.
    let mut runtime = Some(unsafe { velk_plugin_host::Runtime::start(directory.into())? });
    let client = runtime.as_ref().unwrap().client();
    let service = client.semantic_service()?;
    let initial = "fn café() { let text = \"👩‍💻\"; }\n";
    service.update(snapshot(path, 0, initial));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(async {
            let loaded = client.load()?.recv().await.map_err(|e| e.to_string())??;
            let mode=std::env::var("VELK_LSP_SMOKE_MODE").unwrap();
            if mode=="no-profile" || mode=="missing-transport" {
                assert_eq!(loaded.loaded,2);
                until(&service.events,|e| if mode=="no-profile" {matches!(e,Event::Status(Status::Stopped))} else {matches!(e,Event::Status(Status::Failed(message)) if message.contains("requires velk.lsp"))}).await?;
                assert!(!log.exists(),"transport must not start a server without a resolvable language definition");
                return Ok::<_,String>(());
            }
            assert_eq!(loaded.loaded, 3);
            if mode == "workspace" {
                until(&service.events, |e| matches!(e, Event::Diagnostics { .. })).await?;
                let unopened = path.parent().unwrap().join("unopened.rs").canonicalize().map_err(|e|e.to_string())?;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !client.diagnostic_publications().iter().any(|p| p.path == unopened) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.map_err(|_| "unopened workspace diagnostics were dropped")?;
                let publication = client.diagnostic_publications().into_iter().find(|p| p.path == unopened).unwrap();
                assert_eq!(publication.revision, None);
                assert!(publication.disk_matches);
                assert_eq!(publication.diagnostics[0].message, "unopened workspace error");
                let other = path.parent().unwrap().join("second.rs");
                let second = client.semantic_service()?;
                second.update(snapshot(&other, 9, initial));
                until(&second.events, |e| matches!(e, Event::Diagnostics { revision: 9, .. })).await?;
                let initial_messages = messages(log);
                assert_eq!(initial_messages.iter().filter(|v|v["method"] == "initialize").count(), 1);
                assert_eq!(initial_messages.iter().filter(|v|v.get("pid").is_some()).count(), 1);
                service.shutdown();
                logged(log, |messages| messages.iter().any(|v| v["method"] == "textDocument/didClose")).await?;
                second.command(PositionRequest { id:1, revision:9, position:4 }, serde_json::json!({"method":"textDocument/definition","result":"locations","capability":"/definitionProvider"}),None)?;
                let Event::Command { result: Ok(velk_commands::Outcome::Locations { locations }), .. } = until(&second.events, |e|matches!(e, Event::Command { .. })).await? else { panic!("shared definition failed") };
                assert_eq!(locations[0].path, other);
                assert!(client.diagnostic_publications().iter().any(|p| p.path == unopened), "closing a buffer must not discard workspace diagnostics");
                second.saved(snapshot(&other, 9, initial));
                tokio::time::timeout(Duration::from_secs(5), async {
                    while client.diagnostic_publications().iter().any(|p| p.path == unopened && !p.diagnostics.is_empty()) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.map_err(|_| "empty diagnostic publication did not clear workspace results")?;
                second.shutdown();
                logged(log, |messages| messages.iter().filter(|v| v["method"] == "textDocument/didClose").count() == 2).await?;
                assert!(!messages(log).iter().any(|v|v["method"] == "shutdown"));
                client.unload("velk.language-server.rust-analyzer".into())?.recv().await.map_err(|e|e.to_string())??;
                logged(log, |messages|messages.iter().any(|v|v["method"] == "exit")).await?;
                assert_reaped(log);
                assert!(client.diagnostic_publications().is_empty(), "retiring a workspace client clears its results");
                return Ok::<_,String>(());
            }

            let profile_resident=loaded.plugins.iter().find(|p|p.id=="velk.language-server.rust-analyzer").unwrap().resident_code;
            let profile_image=loaded.plugins.iter().find(|p|p.id=="velk.language-server.rust-analyzer").unwrap().loaded_path.clone();
            let image = loaded
                .plugins
                .iter()
                .find(|p| p.id == "velk.lsp")
                .unwrap()
                .loaded_path
                .clone();
            if std::env::var("VELK_LSP_SMOKE_MODE").as_deref() == Ok("stall-init") {
                logged(log, |m| m.iter().any(|v| v["method"] == "initialize")).await?;
                let started = std::time::Instant::now();
                let report = client
                    .unload("velk.lsp".into())?
                    .recv()
                    .await
                    .map_err(|e| e.to_string())??;
                assert_eq!(report.loaded, 2);
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "unload waited for initialize's 30-second timeout"
                );
                assert_reaped(log);
                assert!(
                    client.render_markdown("**still alive**".into()).await?.runs[0]
                        .style
                        .bold
                );
                return Ok::<_, String>(());
            }
            until(&service.events, |e| {
                matches!(e, Event::Status(Status::Ready(_)))
            })
            .await?;
            if mode == "runtime-stop" {
                // Mirror the application's exit hook with UI clients still alive.
                drop(runtime.take());
                let protocol = messages(log);
                let methods: Vec<_> = protocol.iter().filter_map(|v| v["method"].as_str()).collect();
                let close = methods.iter().position(|&m| m == "textDocument/didClose").unwrap();
                let shutdown = methods.iter().position(|&m| m == "shutdown").unwrap();
                let exit = methods.iter().position(|&m| m == "exit").unwrap();
                assert!(close < shutdown && shutdown < exit);
                assert_reaped(log);
                assert!(client.load().is_err());
                drop(diagnostics.take());
                let logfile = log.parent().unwrap().join("logs").join(format!("velk-{}.log", std::process::id()));
                let records = messages(&logfile);
                for marker in ["fixture server started", "fixture server stopped"] {
                    assert!(records.iter().any(|record| record["target"] == "velk_plugin"
                        && record["fields"]["plugin_target"] == "velk_plugin_lsp::server"
                        && record["fields"]["message"].as_str().is_some_and(|message| message.contains(marker))),
                        "missing server stderr record: {marker}");
                }
                return Ok(());
            }
            let first = until(&service.events, |e| {
                matches!(e, Event::Diagnostics { revision: 0, .. })
            })
            .await?;
            assert!(
                matches!(first,Event::Diagnostics{diagnostics,..} if diagnostics[0].range==(3..7))
            );
            for (id, declaration) in loaded.plugins.iter().flat_map(|p| &p.metadata.commands).enumerate() {
                let velk_commands::Handler::Semantic { payload } = &declaration.handler else { continue; };
                let request = PositionRequest { id:1000+id as u64, revision:0, position:4 };
                service.command(request, payload.clone(), declaration.input.as_ref().map(|_|"renamed".into()))?;
                let result = until(&service.events, |e| matches!(e, Event::Command { .. })).await?;
                assert!(matches!(result, Event::Command { request:reply, result:Ok(_) } if reply == request));
            }
            for revision in 1..=99 {
                service.update(snapshot(
                    path,
                    revision,
                    &format!("{initial}// {revision} 世界\n"),
                ));
            }
            until(&service.events, |e| {
                matches!(e, Event::Diagnostics { revision: 99, .. })
            })
            .await?;
            service.hover(PositionRequest {
                id: 1,
                revision: 99,
                position: 4,
            });
            logged(log, |m| {
                m.iter()
                    .filter(|v| v["method"] == "textDocument/hover")
                    .count()
                    == 1
            })
            .await?;
            service.cancel_hover();
            service.hover(PositionRequest {
                id: 2,
                revision: 99,
                position: 5,
            });
            let owned = until(
                &service.events,
                |e| matches!(e,Event::Hover {request,..} if request.id==2),
            )
            .await?;
            assert!(matches!(&owned,Event::Hover {hover:Some(h),..} if h.contents=="CURRENT"));
            service.saved(snapshot(path, 99, &format!("{initial}// 99 世界\n")));
            logged(log, |m| {
                m.iter().any(|v| v["method"] == "textDocument/didSave")
            })
            .await?;
            let report = client
                .unload("velk.lsp".into())?
                .recv()
                .await
                .map_err(|e| e.to_string())??;
            assert_eq!(report.loaded, 2);
            until(&service.events, |e| {
                matches!(e, Event::Status(Status::Failed(message)) if message.contains("requires velk.lsp"))
            })
            .await?;
            assert_reaped(log);
            assert!(matches!(owned,Event::Hover {hover:Some(h),..} if h.contents=="CURRENT"));
            assert!(
                client.render_markdown("**independent**".into()).await?.runs[0]
                    .style
                    .bold
            );
            let newest = format!("{initial}// edited while LSP was unloaded\n");
            service.update(snapshot(path, 100, &newest));
            let report = client.load()?.recv().await.map_err(|e| e.to_string())??;
            assert_eq!(report.loaded, 3);
            assert_eq!(
                report
                    .plugins
                    .iter()
                    .find(|p| p.id == "velk.lsp")
                    .unwrap()
                    .loaded_path,
                image,
                "reload must reuse resident code"
            );
            until(&service.events, |e| {
                matches!(e, Event::Status(Status::Ready(_)))
            })
            .await?;
            until(&service.events, |e| {
                matches!(e, Event::Diagnostics { revision: 100, .. })
            })
            .await?;
            assert!(
                messages(log)
                    .iter()
                    .any(|v| v["method"] == "textDocument/didOpen"
                        && v["params"]["textDocument"]["text"] == newest)
            );
            // First hover in the new server is deliberately held. Unload must cancel
            // it and finish shutdown before returning, with no stale event afterward.
            service.hover(PositionRequest {
                id: 3,
                revision: 100,
                position: 4,
            });
            logged(log, |m| {
                m.iter()
                    .filter(|v| v["method"] == "textDocument/hover")
                    .count()
                    >= 3
            })
            .await?;
            let report = client
                .unload("velk.language-server.rust-analyzer".into())?
                .recv()
                .await
                .map_err(|e| e.to_string())??;
            assert_eq!(report.loaded, 2);
            assert_reaped(log);
            assert_eq!(profile_image.exists(),profile_resident,"profile image follows its standalone or pack lifetime");
            until(&service.events, |e| {
                matches!(e, Event::Status(Status::Stopped))
            })
            .await?;
            let latest=format!("{initial}// edited while Rust profile was unloaded\n");
            service.update(snapshot(path,101,&latest));
            assert_eq!(client.load()?.recv().await.map_err(|e|e.to_string())??.loaded,3);
            until(&service.events,|e|matches!(e,Event::Status(Status::Ready(_)))).await?;
            until(&service.events,|e|matches!(e,Event::Diagnostics {revision:101,..})).await?;
            assert_eq!(client.unload_all()?.recv().await.map_err(|e|e.to_string())??.loaded,0);
            assert_reaped(log);
            Ok(())
        })
}
