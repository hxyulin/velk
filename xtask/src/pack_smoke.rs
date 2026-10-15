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
        assert_eq!(manager.plugins().count(), 4);
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
        assert!(matches!(manager.unload(ID), Some(Event::Removed(_))));
        assert!(image.exists());
        assert_eq!(manager.describe("velk.lsp")?, "Generic LSP transport");
        assert!(matches!(manager.load()?.as_slice(), [Event::Loaded(_)]));
        let markdown = manager.plugins().find(|p| p.id == ID).unwrap();
        assert_eq!(markdown.loaded_path, image);
        assert!(markdown.generation > old);
        assert_eq!(manager.unload_all().len(), 4);
        assert!(image.exists(), "LSP's image lease applies to all members");
        assert!(manager.call_hook(&search, b"{}").is_err());
        assert_eq!(manager.load()?.len(), 4);
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
        "Plugin pack smoke passed: one image, four identities, independent unload/reload, resident subsets, artifact revalidation, nonresident cleanup, initialization failures, stale handles, packed LSP sessions"
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
