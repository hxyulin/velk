use super::*;
use velk_plugin_host::{Event, Manager};
#[allow(unsafe_code)]
pub(super) fn run(root: &Path, output: &Path, release: bool) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let specs = stage_builtins(output, temp.path())?;
    publish(temp.path(), specs.clone())?;
    let mut manager = Manager::new(temp.path().into());
    // SAFETY: locally built bundled SDK pack and probe only.
    unsafe {
        assert!(
            manager
                .load()?
                .iter()
                .all(|e| matches!(e, Event::Loaded(_)))
        );
        assert_eq!(manager.plugins().count(), 5);
        let image = manager.plugins().next().unwrap().loaded_path.clone();
        assert!(
            manager
                .plugins()
                .all(|p| p.loaded_path == image && p.resident_code)
        );
        let old = manager.plugins().find(|p| p.id == ID).unwrap().generation;
        assert!(
            manager
                .render_markdown("**packed**")?
                .runs
                .iter()
                .any(|r| r.style.bold)
        );
        let search = manager.bind_hook(velk_search::DEFAULT_BACKEND, velk_search::HOOK)?;
        let panel = manager.bind_hook("velk.files", velk_panels::HOOK)?;
        let project = tempfile::tempdir().map_err(|e| e.to_string())?;
        std::fs::create_dir(project.path().join(".git")).map_err(|e| e.to_string())?;
        std::fs::create_dir(project.path().join("src")).map_err(|e| e.to_string())?;
        let source = project.path().join("src/main.rs");
        std::fs::write(&source, "fn main() {}\n").map_err(|e| e.to_string())?;
        let context = velk_panels::Context {
            document: Some(velk_panels::NativePath::new(&source)),
            ..Default::default()
        };
        let snapshot = panel_request(
            &mut manager,
            &panel,
            velk_panels::Request::Describe {
                context: context.clone(),
            },
        )?;
        snapshot.validate()?;
        assert_eq!(snapshot.panels[0].title, "Files");
        let velk_panels::Content::Tree(rows) = &snapshot.panels[0].content else {
            panic!("expected filesystem tree")
        };
        assert!(!rows.iter().any(|n| n.label == "main.rs"));
        let folder = rows.iter().find(|n| n.label == "src").unwrap();
        assert!(folder.lazy);
        let expanded = panel_request(
            &mut manager,
            &panel,
            velk_panels::Request::Expand {
                panel: "files".into(),
                revision: snapshot.panels[0].revision,
                node: folder.id.clone(),
                expanded: vec![rows[0].id.clone(), folder.id.clone()],
            },
        )?;
        let velk_panels::Content::Tree(rows) = &expanded.panels[0].content else {
            panic!("expected expanded tree")
        };
        let leaf = rows.iter().find(|n| n.label == "main.rs").unwrap();
        let opened = panel_request(
            &mut manager,
            &panel,
            velk_panels::Request::Invoke {
                panel: "files".into(),
                revision: expanded.panels[0].revision,
                node: leaf.id.clone(),
                action: "open".into(),
            },
        )?;
        assert_eq!(
            opened.effects,
            vec![velk_panels::Effect::OpenFile {
                path: velk_panels::NativePath::new(
                    &std::fs::canonicalize(&source).map_err(|e| e.to_string())?
                ),
            }]
        );
        assert!(snapshot.watch);
        std::fs::write(project.path().join("new.txt"), "watched").map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let bytes = manager.call_hook(
                &panel,
                &velk_panels::encode(&velk_panels::Request::Changes)?,
            )?;
            if matches!(
                velk_panels::decode::<velk_panels::Reply>(&bytes)?,
                velk_panels::Reply::Invalidated
            ) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native pack watcher missed a change"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let updated = panel_request(
            &mut manager,
            &panel,
            velk_panels::Request::Describe {
                context: context.clone(),
            },
        )?;
        let velk_panels::Content::Tree(rows) = &updated.panels[0].content else {
            panic!("expected tree")
        };
        assert!(rows.iter().any(|n| n.label == "new.txt"));
        assert!(updated.effects.is_empty());
        // Unload with a queued or completed-but-uncollected job. The plugin joins
        // its owned worker before its instance can be destroyed.
        let pending = manager.call_hook(
            &panel,
            &velk_panels::encode(&velk_panels::Request::Describe {
                context: context.clone(),
            })?,
        )?;
        assert!(matches!(
            velk_panels::decode::<velk_panels::Reply>(&pending)?,
            velk_panels::Reply::Pending { .. }
        ));
        assert!(matches!(
            manager.unload("velk.files"),
            Some(Event::Removed(_))
        ));
        assert!(manager.call_hook(&panel, b"{}").is_err());
        assert_eq!(
            snapshot.panels[0].title, "Files",
            "snapshot remains host owned after unload"
        );
        assert_eq!(manager.load()?.len(), 1);
        let restarted = manager.bind_hook("velk.files", velk_panels::HOOK)?;
        let fresh = panel_request(
            &mut manager,
            &restarted,
            velk_panels::Request::Describe { context },
        )?;
        assert!(fresh.effects.is_empty());

        assert!(matches!(manager.unload(ID), Some(Event::Removed(_))));
        assert!(image.exists());
        assert_eq!(manager.describe("velk.lsp")?, "Generic LSP transport");
        assert!(matches!(manager.load()?.as_slice(), [Event::Loaded(_)]));
        let markdown = manager.plugins().find(|p| p.id == ID).unwrap();
        assert_eq!(markdown.loaded_path, image);
        assert!(markdown.generation > old);
        assert_eq!(manager.unload_all().len(), 5);
        assert!(image.exists(), "LSP's image lease applies to all members");
        assert!(manager.call_hook(&search, b"{}").is_err());
        assert_eq!(manager.load()?.len(), 5);
        assert!(manager.plugins().all(|p| p.loaded_path == image));
        manager.unload_all();
        // Even selecting only a nonresident member must discover pack residency.
        publish(temp.path(), vec![specs[0].clone()])?;
        let mut subset = Manager::new(temp.path().into());
        subset.load()?;
        assert!(subset.plugins().next().unwrap().resident_code);
        subset.unload_all();
        // Cached images never bypass artifact verification.
        std::fs::write(temp.path().join(&specs[0].library), b"corrupt")
            .map_err(|e| e.to_string())?;
        assert!(matches!(
            manager.load()?.as_slice(),
            [Event::Rejected { .. }]
        ));
    }
    probe(root, output, release)?;
    crate::lsp_smoke::run_pack(root, output)?;
    println!(
        "Plugin pack smoke passed: one image, five identities, independent unload/reload, resident subsets, artifact revalidation, nonresident cleanup, initialization failures, stale handles, packed LSP sessions"
    );
    Ok(())
}
#[allow(unsafe_code)]
fn probe(root: &Path, output: &Path, release: bool) -> Result<(), String> {
    let mut build = cargo();
    build.current_dir(root).args([
        "build",
        "--locked",
        "-p",
        "velk-plugin-sdk",
        "--example",
        "pack-probe",
    ]);
    if release {
        build.arg("--release");
    }
    checked(&mut build)?;
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let artifact = stage_plugin(
        &output.join("examples"),
        temp.path(),
        "test.pack.first",
        "pack_probe",
    )?;
    let specs: Vec<_> = [
        "test.pack.first",
        "test.pack.second",
        "test.pack.fails",
        "test.pack.missing",
    ]
    .into_iter()
    .map(|id| PluginSpec {
        id: id.into(),
        pack: true,
        ..artifact.clone()
    })
    .collect();
    publish(temp.path(), specs.clone())?;
    let mut manager = Manager::new(temp.path().into());
    unsafe {
        let events = manager.load()?;
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::Rejected { .. }))
                .count(),
            2
        );
        assert_eq!(manager.plugins().count(), 2);
        let image = manager.plugins().next().unwrap().loaded_path.clone();
        assert!(
            manager
                .plugins()
                .all(|p| p.loaded_path == image && !p.resident_code)
        );
        let first = manager.bind_hook("test.pack.first", "test.pack.increment")?;
        let second = manager.bind_hook("test.pack.second", "test.pack.increment")?;
        assert_eq!(manager.call_hook(&first, b"")?, vec![1]);
        assert_eq!(manager.call_hook(&first, b"")?, vec![2]);
        assert_eq!(manager.call_hook(&second, b"")?, vec![1]);
        manager.unload("test.pack.first");
        assert!(image.exists());
        assert!(manager.call_hook(&first, b"").is_err());
        assert_eq!(manager.call_hook(&second, b"")?, vec![2]);
        publish(temp.path(), specs[..2].to_vec())?;
        manager.load()?;
        assert!(manager.plugins().all(|p| p.loaded_path == image));
        let reloaded = manager.bind_hook("test.pack.first", "test.pack.increment")?;
        assert_eq!(manager.call_hook(&reloaded, b"")?, vec![1]);
        // Coordinated restart preserves opt-in state, retires old handles, and
        // leaves sibling generations unchanged when one plugin restarts.
        let sibling_generation = manager
            .plugins()
            .find(|p| p.id == "test.pack.second")
            .unwrap()
            .generation;
        manager.restart(Some("test.pack.first"))?;
        assert!(manager.call_hook(&reloaded, b"").is_err());
        let restarted = manager.bind_hook("test.pack.first", "test.pack.increment")?;
        assert_eq!(manager.call_hook(&restarted, b"")?, vec![2]);
        assert_eq!(
            manager
                .plugins()
                .find(|p| p.id == "test.pack.second")
                .unwrap()
                .generation,
            sibling_generation
        );
        let set = manager.bind_hook("test.pack.first", "test.pack.set")?;
        manager.call_hook(&set, &[250])?;
        assert!(
            manager.restart(None).is_err(),
            "capture failure aborts before any teardown"
        );
        assert_eq!(manager.plugins().count(), 2);
        assert_eq!(
            manager
                .plugins()
                .find(|p| p.id == "test.pack.second")
                .unwrap()
                .generation,
            sibling_generation
        );
        manager.call_hook(&set, &[251])?;
        let events = manager.restart(None)?;
        assert!(events.iter().any(|event| matches!(event, Event::Rejected { id, message } if id == "test.pack.first" && message.contains("restore"))));
        assert_eq!(manager.plugins().count(), 1);
        assert_eq!(manager.plugins().next().unwrap().id, "test.pack.second");
        manager.restart(None)?;
        assert_eq!(
            manager.plugins().count(),
            1,
            "restart must not enable an inactive plugin"
        );
        manager.unload_all();
        assert!(
            !image.exists(),
            "last nonresident member releases the image"
        );
        manager.load()?;
        let fresh = manager.plugins().next().unwrap().loaded_path.clone();
        assert_ne!(image, fresh);
        manager.unload_all();
        let wrong = PluginSpec {
            pack: false,
            ..specs[0].clone()
        };
        publish(temp.path(), vec![wrong])?;
        assert!(matches!(
            manager.load()?.as_slice(),
            [Event::Rejected { .. }]
        ));
    }
    // Exercise the same worker endpoints used by process handoff. A fresh
    // runtime receives owned state, without inheriting handles or enabled IDs.
    publish(temp.path(), specs[..2].to_vec())?;
    unsafe {
        let runtime = velk_plugin_host::Runtime::start(temp.path().into())?;
        let client = runtime.client();
        let ids = vec!["test.pack.first".to_string()];
        let report = pollster::block_on(client.load_selected(ids.clone())?.recv())
            .map_err(|e| e.to_string())??;
        assert_eq!(report.loaded, 1);
        let handle = pollster::block_on(
            client
                .bind_hook(ids[0].clone(), "test.pack.increment".into())?
                .recv(),
        )
        .map_err(|e| e.to_string())??;
        assert_eq!(
            pollster::block_on(client.call_hook(handle.clone(), vec![]))?,
            vec![1]
        );
        let states = pollster::block_on(client.snapshot_plugins(ids.clone()))?;
        drop(runtime);
        let replacement = velk_plugin_host::Runtime::start(temp.path().into())?;
        let client = replacement.client();
        let report = pollster::block_on(client.load_restored(ids.clone(), states)?.recv())
            .map_err(|e| e.to_string())??;
        assert_eq!(report.loaded, 1);
        assert!(pollster::block_on(client.call_hook(handle, vec![])).is_err());
        let handle = pollster::block_on(
            client
                .bind_hook(ids[0].clone(), "test.pack.increment".into())?
                .recv(),
        )
        .map_err(|e| e.to_string())??;
        assert_eq!(
            pollster::block_on(client.call_hook(handle.clone(), vec![]))?,
            vec![2]
        );
        let report =
            pollster::block_on(client.restart(None)?.recv()).map_err(|e| e.to_string())??;
        assert_eq!(report.loaded, 1);
        assert!(pollster::block_on(client.call_hook(handle, vec![])).is_err());
    }
    let mut build = cargo();
    build.current_dir(root).args([
        "build",
        "--locked",
        "-p",
        "velk-plugin-sdk",
        "--example",
        "duplicate-pack",
    ]);
    if release {
        build.arg("--release");
    }
    checked(&mut build)?;
    let mut duplicate = stage_plugin(
        &output.join("examples"),
        temp.path(),
        "test.duplicate",
        "duplicate_pack",
    )?;
    duplicate.pack = true;
    publish(temp.path(), vec![duplicate])?;
    let events = unsafe { manager.load()? };
    assert!(
        matches!(events.as_slice(), [Event::Rejected { message, .. }] if message.contains("duplicate plugin ID"))
    );
    assert_eq!(manager.plugins().count(), 0);
    Ok(())
}

fn panel_request(
    manager: &mut Manager,
    handle: &velk_plugin_host::HookHandle,
    mut request: velk_panels::Request,
) -> Result<velk_panels::Snapshot, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let bytes = manager.call_hook(handle, &velk_panels::encode(&request)?)?;
        match velk_panels::decode::<velk_panels::Reply>(&bytes)? {
            velk_panels::Reply::Ready { snapshot } => return Ok(snapshot),
            velk_panels::Reply::Pending { id } if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(2));
                request = velk_panels::Request::Poll { id };
            }
            _ => return Err("panel smoke request failed or timed out".into()),
        }
    }
}
