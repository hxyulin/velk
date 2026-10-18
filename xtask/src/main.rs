use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
mod editing_smoke;
mod project_smoke;
mod search_smoke;
mod syntax_smoke;
mod terminal_tools;
mod trace_summary;
mod undo_smoke;
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
fn build(root: &Path, release: bool) -> Result<(), String> {
    let mut command = cargo();
    command
        .current_dir(root)
        .args(["build", "--locked", "-p", "velk-editor"]);
    if release {
        command.arg("--release");
    }
    checked(&mut command)
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
        ]))?;
        return checked(
            Command::new(output.join(format!("velk{}", std::env::consts::EXE_SUFFIX)))
                .env("VELK_TRACE", "1")
                .args(&args),
        );
    }
    if action == "help" {
        println!(
            "cargo xtask setup-terminal\ncargo xtask test\ncargo xtask test-headless [filter]\ncargo xtask build [--release]\ncargo xtask run [--release] -- [file]\ncargo xtask test-features\ncargo xtask profile -- <file>\ncargo xtask trace-summary <capture.trace.json>\n"
        );
        return Ok(());
    }
    if !matches!(
        action.as_str(),
        "build" | "run" | "test-features" | "setup-terminal" | "test"
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
    if action == "test-features" {
        editing_smoke::run()?;
        search_smoke::run()?;
        syntax_smoke::run()?;
        project_smoke::run()?;
        undo_smoke::run()?;
        println!("Built-in feature integration checks passed.");
        return Ok(());
    }
    build(&root, release)?;
    if action == "run" {
        checked(
            Command::new(output.join(format!("velk{}", std::env::consts::EXE_SUFFIX))).args(&args),
        )?;
    }
    Ok(())
}
