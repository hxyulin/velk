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
    for mode in ["normal", "stall-init", "no-profile", "missing-transport"] {
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
        "LSP plugin smoke passed: native async diagnostics/hover, declared navigation/Rust commands, cancellation, save, unload during requests/initialization, process reaping, latest-document reattachment, resident image reuse, independent Markdown, namespaced Rust definition dependency/unload/reload, no server without its profile or transport."
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
    // SAFETY: parent staged only trusted SDK-based local plugins.
    let runtime = unsafe { velk_plugin_host::Runtime::start(directory.into())? };
    let client = runtime.client();
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
