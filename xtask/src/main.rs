use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
use velk_plugin_host::{MANIFEST, Manifest, PluginSpec};
mod diagnostics_smoke;
mod editing_smoke;
mod hooks_smoke;
mod lsp_smoke;
mod pack_smoke;
mod project_smoke;
mod search_smoke;
mod syntax_smoke;
mod terminal_tools;
mod trace_summary;
mod undo_smoke;
mod wasm_smoke;
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
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    terminal_tools::configure(&mut command);
    command
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
        "velk.language.rust",
        ID,
        "velk.tree-sitter",
        "velk.undo.tree",
        "velk.undo.browser",
        "velk.lsp",
        "velk.snippets",
        "velk.completion",
        "velk.completion.words",
        "velk.completion.compact",
        "velk.language-server.rust-analyzer",
        velk_search::DEFAULT_BACKEND,
        "velk.files.backend",
        "velk.project.files",
        "velk.project.search",
        "velk.files",
        "velk.terminal",
        "velk.vim",
        "velk.indentation",
        "velk.autopairs",
        "velk.autosave",
        "velk.quick-open",
        "velk.workspace-find",
        "velk.problems",
    ]
    .into_iter()
    .map(|id| PluginSpec {
        runtime: Default::default(),
        interfaces: bundled_interfaces(id),
        id: id.into(),
        pack: true,
        dependencies: match id {
            "velk.language-server.rust-analyzer" => vec!["velk.lsp".into()],
            _ => Vec::new(),
        },
        install_reason: match id {
            "velk.files.backend" | "velk.project.files" | "velk.project.search" | "velk.lsp" => {
                velk_plugin_host::InstallReason::Dependency
            }
            _ => velk_plugin_host::InstallReason::Bundled,
        },
        ..artifact.clone()
    })
    .collect())
}
fn bundled_interfaces(id: &str) -> velk_plugin_contracts::interfaces::Interfaces {
    use velk_plugin_contracts::interfaces::{Implementation, Interfaces, Requirement};
    let mut interfaces = Interfaces::default();
    for (provider, consumer, interface, title, hook) in [
        (
            "velk.files.backend",
            "velk.files",
            "velk.files.tree",
            "File tree",
            "velk.files.tree",
        ),
        (
            "velk.project.files",
            "velk.quick-open",
            "velk.project.files",
            "Project file queries",
            "velk.project.query",
        ),
        (
            "velk.project.search",
            "velk.workspace-find",
            "velk.project.search",
            "Workspace text search",
            "velk.project.query",
        ),
    ] {
        if id == provider {
            interfaces.implements.push(Implementation {
                interface: interface.into(),
                title: title.into(),
                hook: hook.into(),
                version: 1,
            });
        }
        if id == consumer {
            interfaces.requires.push(Requirement {
                interface: interface.into(),
                version: 1,
                default_provider: provider.into(),
            });
        }
    }
    interfaces
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
        "velk-plugin-snippets",
        "-p",
        "velk-plugin-completion",
        "-p",
        "velk-plugin-rust",
        "-p",
        "velk-plugin-rust-analyzer",
        "-p",
        "velk-plugin-search",
        "-p",
        "velk-plugin-vim",
        "-p",
        "velk-plugin-indentation",
        "-p",
        "velk-plugin-autopairs",
        "-p",
        "velk-plugin-tree-sitter",
        "-p",
        "velk-plugin-undo",
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
        runtime: Default::default(),
        interfaces: Default::default(),
        dependencies: Vec::new(),
        install_reason: Default::default(),
        id: id.into(),
        library: relative,
        digest,
        pack: false,
    })
}
fn build_wasm(root: &Path) -> Result<(), String> {
    checked(cargo().current_dir(root).args([
        "build",
        "--locked",
        "--release",
        "--target",
        "wasm32-unknown-unknown",
        "-p",
        "velk-plugin-completion-wasm",
    ]))
}
fn stage_wasm(output: &Path, directory: &Path) -> Result<PluginSpec, String> {
    let source = output
        .parent()
        .unwrap()
        .join("wasm32-unknown-unknown/release/velk_plugin_completion_wasm.wasm");
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let library = PathBuf::from("artifacts")
        .join(&digest)
        .join("matching.wasm");
    let target = directory.join(&library);
    std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
    if !target.exists() {
        let temporary =
            tempfile::NamedTempFile::new_in(target.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(temporary.path(), bytes).map_err(|e| e.to_string())?;
        temporary.persist(&target).map_err(|e| e.to_string())?;
    }
    Ok(PluginSpec {
        runtime: velk_plugin_host::PluginRuntime::WasmMatching {
            metadata: velk_plugin_contracts::PluginMetadata {
                name: "Wasm completion matching".into(),
                readme: include_str!("../../plugins/completion-wasm/README.md").into(),
                ..Default::default()
            },
            version: env!("CARGO_PKG_VERSION").into(),
        },
        interfaces: Default::default(),
        id: "velk.completion.wasm".into(),
        library,
        digest,
        pack: false,
        dependencies: vec![],
        install_reason: velk_plugin_host::InstallReason::Bundled,
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
    let wasm = args.first().is_some_and(|s| s == "--wasm");
    if wasm {
        args.remove(0);
    }
    if wasm
        && !matches!(
            action.as_str(),
            "build" | "plugins" | "run" | "test-plugins"
        )
    {
        return Err("--wasm is supported by build, plugins, run, and test-plugins".into());
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
    if action == "test-headless" {
        let (root, _) = paths(release)?;
        let mut command = cargo();
        command.current_dir(root).args([
            "test",
            "--locked",
            "-p",
            "velk-core",
            "-p",
            "velk-core-contracts",
            "-p",
            "velk-commands",
            "-p",
            "velk-editor-model",
            "-p",
            "velk-editor-widget",
            "-p",
            "velk-editor",
            "-p",
            "velk-plugin-sdk",
            "--features",
            "velk-plugin-sdk/test-support",
        ]);
        if release {
            command.arg("--release");
        }
        return checked(command.args(args));
    }
    if action == "prepare-wasm" && args.is_empty() {
        let (_, output) = paths(release)?;
        return prepare_wasm(&output.join("plugins"));
    }
    if action == "test-diagnostics" && args.is_empty() && !release {
        let (root, output) = paths(false)?;
        return diagnostics_smoke::run(&root, &output);
    }
    if action == "trace-summary" && args.len() == 1 {
        return trace_summary::run(Path::new(&args[0]));
    }
    if action == "profile" && args.len() == 1 && !release {
        let (root, output) = paths(false)?;
        let output = output.parent().unwrap().join("profiling");
        checked(cargo().current_dir(&root).args([
            "build",
            "--locked",
            "--profile",
            "profiling",
            "--features",
            "velk-editor/performance",
            "-p",
            "velk-editor",
            "-p",
            "velk-plugin-builtin",
        ]))?;
        let directory = output.join("plugins");
        publish(&directory, stage_builtins(&output, &directory)?)?;
        return checked(
            Command::new(output.join(format!("velk{}", std::env::consts::EXE_SUFFIX)))
                .env("VELK_PLUGIN_DIR", directory)
                .env("VELK_TRACE", "1")
                .args(&args),
        );
    }
    if action == "help" {
        println!(
            "cargo xtask setup-terminal\ncargo xtask test\ncargo xtask test-headless [filter]\ncargo xtask build [--release]\ncargo xtask plugins [--release] [--wasm]\ncargo xtask run [--release] [--wasm] -- [file]\ncargo xtask test-plugins [--release] [--wasm]\ncargo xtask prepare-wasm [--release]\ncargo xtask profile -- <file>\ncargo xtask trace-summary <capture.trace.json>\ncargo xtask test-diagnostics"
        );
        return Ok(());
    }
    if !matches!(
        action.as_str(),
        "build" | "plugins" | "run" | "test-plugins" | "setup-terminal" | "test"
    ) || (action == "run" && args.len() > 1)
        || (action != "run" && !args.is_empty())
    {
        return Err("invalid arguments; use cargo xtask help".into());
    }
    let (root, output) = paths(release)?;
    if action == "setup-terminal" {
        return terminal_tools::setup(&root);
    }
    if action == "test" {
        return checked(cargo().current_dir(&root).args(["test", "--workspace"]));
    }
    if action == "test-plugins" {
        if wasm {
            build_wasm(&root)?;
            wasm_smoke::run(&output)?;
        }
        return smoke(&root, &output, release);
    }
    build(&root, release, action != "plugins")?;
    let directory = output.join("plugins");
    let mut plugins = stage_builtins(&output, &directory)?;
    if wasm {
        build_wasm(&root)?;
        plugins.push(stage_wasm(&output, &directory)?);
    }
    publish(&directory, plugins)?;
    if wasm {
        prepare_wasm(&directory)?;
    }
    println!("Built plugins: {}", directory.display());
    if action == "run" {
        checked(
            Command::new(output.join(format!("velk{}", std::env::consts::EXE_SUFFIX)))
                .env("VELK_PLUGIN_DIR", directory)
                .args(&args),
        )?;
    }
    Ok(())
}

fn prepare_wasm(directory: &Path) -> Result<(), String> {
    let mut manager = velk_plugin_host::Manager::new(directory.to_path_buf());
    let count = manager.prepare_wasm()?;
    let stats = manager.wasm_cache_stats();
    if count > 0 && !stats.enabled {
        println!("Prepared {count} Wasm modules without persistent caching");
        return Ok(());
    }
    println!(
        "Prepared {count} Wasm modules: {} disk cache hits, {} newly cached compilations",
        stats.hits, stats.misses
    );
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
    project_smoke::run(output)?;
    build_standalone(root, release)?;
    editing_smoke::run(output, false)?;
    editing_smoke::run(output, true)?;
    syntax_smoke::run(output, false)?;
    syntax_smoke::run(output, true)?;
    undo_smoke::run(output, false)?;
    undo_smoke::run(output, true)?;
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
        runtime: Default::default(),
        interfaces: Default::default(),
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
            runtime: Default::default(),
            interfaces: Default::default(),
            library: "missing-library".into(),
            ..second.clone()
        },
        PluginSpec {
            runtime: Default::default(),
            interfaces: Default::default(),
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
            runtime: Default::default(),
            interfaces: Default::default(),
            dependencies: Vec::new(),
            install_reason: Default::default(),
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
    use velk_markup::Renderer;
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
    assert_eq!(
        manager
            .render_markdown_with("community.missing", source)?
            .text(),
        source
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
    let renderer = client.markdown_renderer("velk.markdown");
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
        assert_eq!(renderer.render(source.into()).await?, document);
        assert_eq!(
            client
                .unload("velk.markdown".into())?
                .recv()
                .await
                .map_err(|e| e.to_string())??
                .loaded,
            0
        );
        assert_eq!(renderer.render(source.into()).await?.text(), source);
        assert_eq!(
            client
                .load()?
                .recv()
                .await
                .map_err(|e| e.to_string())??
                .loaded,
            1
        );
        assert_eq!(renderer.render(source.into()).await?, document);
        Ok::<_, String>(())
    })?;
    println!(
        "Markdown plugin smoke passed: typed styled runs, host-owned output after unload, async rendering, literal fallback, reactivation."
    );
    Ok(())
}
