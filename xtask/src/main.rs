use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
use velk_plugin_host::{MANIFEST, Manifest, PluginSpec};
mod hooks_smoke;
mod lsp_smoke;
mod pack_smoke;
mod search_smoke;
const PLUGIN: &str = "velk-plugin-markdown";
const ID: &str = "velk.markdown";
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
fn checked(command: &mut Command) -> Result<(), String> {
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {status}"))
    }
}
fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}
fn paths(release: bool) -> Result<(PathBuf, PathBuf), String> {
    let output = cargo()
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let root = PathBuf::from(
        metadata["workspace_root"]
            .as_str()
            .ok_or("missing workspace root")?,
    );
    let output = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("missing target directory")?,
    )
    .join(if release { "release" } else { "debug" });
    Ok((root, output))
}
fn build(root: &Path, release: bool, editor: bool) -> Result<(), String> {
    let mut command = cargo();
    command
        .current_dir(root)
        .args(["build", "--locked", "-p", "velk-plugin-builtin"]);
    if editor {
        command.args(["-p", "velk-editor"]);
    }
    if release {
        command.arg("--release");
    }
    checked(&mut command)
}
fn library_name(stem: &str) -> String {
    format!(
        "{}{stem}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}
fn stage(output: &Path, directory: &Path) -> Result<PluginSpec, String> {
    stage_plugin(output, directory, ID, "velk_plugin_markdown")
}
fn stage_builtins(output: &Path, directory: &Path) -> Result<Vec<PluginSpec>, String> {
    let artifact = stage_plugin(output, directory, ID, "velk_plugin_builtin")?;
    Ok([
        ID,
        "velk.lsp",
        "velk.language-server.rust-analyzer",
        velk_search::DEFAULT_BACKEND,
    ]
    .into_iter()
    .map(|id| PluginSpec {
        id: id.into(),
        pack: true,
        ..artifact.clone()
    })
    .collect())
}
fn build_standalone(root: &Path, release: bool) -> Result<(), String> {
    let mut command = cargo();
    command.current_dir(root).args([
        "build",
        "--locked",
        "--examples",
        "-p",
        PLUGIN,
        "-p",
        "velk-plugin-lsp",
        "-p",
        "velk-plugin-rust-analyzer",
        "-p",
        "velk-plugin-search",
    ]);
    if release {
        command.arg("--release");
    }
    checked(&mut command)
}
fn stage_plugin(
    output: &Path,
    directory: &Path,
    id: &str,
    stem: &str,
) -> Result<PluginSpec, String> {
    let source = output.join(library_name(stem));
    let mut hash = blake3::Hasher::new();
    hash.update_reader(
        std::fs::File::open(&source).map_err(|e| format!("{}: {e}", source.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let digest = hash.finalize().to_hex().to_string();
    let relative = PathBuf::from("artifacts")
        .join(&digest)
        .join(library_name(stem));
    let target = directory.join(&relative);
    std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
    if !target.exists() {
        let temporary =
            tempfile::NamedTempFile::new_in(target.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(&source, temporary.path()).map_err(|e| e.to_string())?;
        temporary.persist(&target).map_err(|e| e.to_string())?;
    }
    Ok(PluginSpec {
        id: id.into(),
        library: relative,
        digest,
        pack: false,
    })
}
fn publish(directory: &Path, plugins: Vec<PluginSpec>) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let manifest = Manifest { plugins };
    manifest.validate()?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    temporary
        .write_all(&serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist(directory.join(MANIFEST))
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn run() -> Result<(), String> {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let action = args
        .first()
        .and_then(|s| s.to_str())
        .unwrap_or("help")
        .to_owned();
    if !args.is_empty() {
        args.remove(0);
    }
    let release = args.first().is_some_and(|s| s == "--release");
    if release {
        args.remove(0);
    }
    if args.first().is_some_and(|s| s == "--") {
        args.remove(0);
    }
    if action == "__test-lsp" && args.len() == 3 {
        return lsp_smoke::child(
            Path::new(&args[0]),
            Path::new(&args[1]),
            Path::new(&args[2]),
        );
    }
    if action == "help" {
        println!(
            "cargo xtask build [--release]\ncargo xtask plugins [--release]\ncargo xtask run [--release] -- <file>\ncargo xtask test-plugins [--release]"
        );
        return Ok(());
    }
    if !matches!(
        action.as_str(),
        "build" | "plugins" | "run" | "test-plugins"
    ) || (action == "run" && args.len() != 1)
        || (action != "run" && !args.is_empty())
    {
        return Err("invalid arguments; use cargo xtask help".into());
    }
    let (root, output) = paths(release)?;
    if action == "test-plugins" {
        return smoke(&root, &output, release);
    }
    build(&root, release, action != "plugins")?;
    let directory = output.join("plugins");
    publish(&directory, stage_builtins(&output, &directory)?)?;
    println!("Built plugins: {}", directory.display());
    if action == "run" {
        checked(
            Command::new(output.join(format!("velk{}", std::env::consts::EXE_SUFFIX)))
                .env("VELK_PLUGIN_DIR", directory)
                .arg(&args[0]),
        )?;
    }
    Ok(())
}
#[allow(unsafe_code)]
fn smoke(root: &Path, output: &Path, release: bool) -> Result<(), String> {
    use velk_plugin_host::{Event, Manager};
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let directory = directory.path();
    let mut manager = Manager::new(directory.into());
    build(root, release, false)?;
    pack_smoke::run(root, output, release)?;
    build_standalone(root, release)?;
    let profile_output = output;
    let standalone = output.join("examples");
    let output = standalone.as_path();
    let first = stage(output, directory)?;
    publish(directory, vec![first.clone()])?;
    // SAFETY: these artifacts were built locally from the bundled SDK plugin.
    let events = unsafe { manager.load()? };
    assert!(matches!(events.as_slice(), [Event::Loaded(_)]));
    assert_eq!(manager.describe(ID)?, "CommonMark documentation renderer");
    let generation = manager.plugins().next().unwrap().generation;
    let old_copy = manager.plugins().next().unwrap().loaded_path.clone();
    assert!(old_copy.exists());
    let owned = manager.plugins().next().unwrap().description.clone();
    // Restage the real Markdown plugin at another immutable path. Loading
    // must leave the active generation alone until explicitly unloaded.
    let second = PluginSpec {
        library: PathBuf::from("restaged").join(library_name("velk_plugin_markdown")),
        ..first.clone()
    };
    std::fs::create_dir_all(directory.join("restaged")).map_err(|e| e.to_string())?;
    std::fs::copy(
        directory.join(&first.library),
        directory.join(&second.library),
    )
    .map_err(|e| e.to_string())?;
    publish(directory, vec![second.clone()])?;
    // Building/staging does not replace an active instance.
    assert!(unsafe { manager.load()? }.is_empty());
    assert_eq!(manager.describe(ID)?, "CommonMark documentation renderer");
    assert!(manager.unload(ID).is_some());
    assert!(!old_copy.exists());
    assert_eq!(owned, "CommonMark documentation renderer");
    assert!(matches!(
        unsafe { manager.load()? }.as_slice(),
        [Event::Loaded(_)]
    ));
    assert_eq!(manager.describe(ID)?, "CommonMark documentation renderer");
    assert!(manager.plugins().next().unwrap().generation > generation);
    // Repeated explicit unload/load cycles must retire their disk copies.
    for _ in 0..12 {
        let retired = manager.plugins().next().unwrap().loaded_path.clone();
        assert_eq!(manager.unload_all().len(), 1);
        assert!(!retired.exists());
        assert!(matches!(
            unsafe { manager.load()? }.as_slice(),
            [Event::Loaded(_)]
        ));
    }
    manager.unload_all();
    for bad in [
        PluginSpec {
            library: "missing-library".into(),
            ..second.clone()
        },
        PluginSpec {
            digest: "0".repeat(64),
            ..second.clone()
        },
    ] {
        publish(directory, vec![bad])?;
        assert!(matches!(
            unsafe { manager.load()? }.as_slice(),
            [Event::Rejected { .. }]
        ));
        assert_eq!(manager.plugins().count(), 0);
    }
    std::fs::write(directory.join("invalid-library"), b"not a dynamic library")
        .map_err(|e| e.to_string())?;
    publish(
        directory,
        vec![PluginSpec {
            id: ID.into(),
            library: "invalid-library".into(),
            digest: blake3::hash(b"not a dynamic library").to_hex().to_string(),
            pack: false,
        }],
    )?;
    assert!(matches!(
        unsafe { manager.load()? }.as_slice(),
        [Event::Rejected { .. }]
    ));
    publish(directory, vec![second])?;
    assert!(matches!(
        unsafe { manager.load()? }.as_slice(),
        [Event::Loaded(_)]
    ));
    std::fs::write(directory.join(MANIFEST), b"incomplete JSON").map_err(|e| e.to_string())?;
    assert!(unsafe { manager.load() }.is_err());
    assert_eq!(manager.describe(ID)?, "CommonMark documentation renderer");
    // Unload works independently of whether the manifest is missing or broken.
    let last_copy = manager.plugins().next().unwrap().loaded_path.clone();
    assert_eq!(manager.unload_all().len(), 1);
    assert!(!last_copy.exists());
    assert_eq!(manager.plugins().count(), 0);
    markdown_smoke(output)?;
    lsp_smoke::run(root, output)?;
    hooks_smoke::run(root, profile_output, release)?;
    search_smoke::run(output)?;
    println!(
        "Native plugin smoke passed: load, no automatic replacement, explicit unload/load of restaged code, repeated cleanup, retained host data, rejected artifacts, malformed manifest."
    );
    Ok(())
}

#[allow(unsafe_code)]
fn markdown_smoke(output: &Path) -> Result<(), String> {
    use velk_plugin_host::{Event, Manager, Runtime};
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let spec = stage_plugin(
        output,
        directory.path(),
        "velk.markdown",
        "velk_plugin_markdown",
    )?;
    publish(directory.path(), vec![spec])?;
    let mut manager = Manager::new(directory.path().into());
    // SAFETY: only the locally built SDK-based Markdown plugin is staged here.
    assert!(matches!(
        unsafe { manager.load()? }.as_slice(),
        [Event::Loaded(_)]
    ));
    let source = "# Heading\n\n**bold** *italic* ~~old~~ `code` [docs](https://example.com)\n\n```rust\nlet value = 42;\n```";
    let document = manager.render_markdown(source)?;
    assert!(document.runs.iter().any(|r| r.style.heading == 1));
    assert!(
        document
            .runs
            .iter()
            .any(|r| r.style.bold && r.text == "bold")
    );
    assert!(
        document
            .runs
            .iter()
            .any(|r| r.style.italic && r.text == "italic")
    );
    assert!(document.runs.iter().any(|r| r.style.strikethrough));
    assert!(
        document
            .runs
            .iter()
            .any(|r| r.style.link.as_deref() == Some("https://example.com"))
    );
    assert!(
        document
            .runs
            .iter()
            .any(|r| r.style.code && r.text.contains("let value = 42;"))
    );
    manager.unload_all();
    assert!(
        document.text().contains("Heading"),
        "result survives provider unload"
    );
    assert_eq!(
        manager.render_markdown(source)?.text(),
        source,
        "missing provider falls back to literal text"
    );
    // SAFETY: same trusted local artifacts, this time through the async endpoint.
    let runtime = unsafe { Runtime::start(directory.path().into())? };
    let client = runtime.client();
    pollster::block_on(async {
        assert_eq!(
            client
                .load()?
                .recv()
                .await
                .map_err(|e| e.to_string())??
                .loaded,
            1
        );
        assert_eq!(client.render_markdown(source.into()).await?, document);
        assert_eq!(
            client
                .unload("velk.markdown".into())?
                .recv()
                .await
                .map_err(|e| e.to_string())??
                .loaded,
            0
        );
        assert_eq!(client.render_markdown(source.into()).await?.text(), source);
        assert_eq!(
            client
                .load()?
                .recv()
                .await
                .map_err(|e| e.to_string())??
                .loaded,
            1
        );
        assert_eq!(client.render_markdown(source.into()).await?, document);
        Ok::<_, String>(())
    })?;
    println!(
        "Markdown plugin smoke passed: typed styled runs, host-owned output after unload, async rendering, literal fallback, reactivation."
    );
    Ok(())
}
