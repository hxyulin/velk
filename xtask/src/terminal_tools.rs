use std::{
    path::{Path, PathBuf},
    process::Command,
};
const SOURCE: &str = "ghostty-b40acce/libghostty-vt-1.3.2-main+b40acce";
pub fn cache() -> Option<PathBuf> {
    std::env::var_os("VELK_TOOLCHAIN_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|p| PathBuf::from(p).join(".cache/velk/toolchains"))
        })
}
pub fn configure(command: &mut Command) {
    if let Some(cache) = cache() {
        let zig = cache
            .join("zig-python-0.16.0/ziglang")
            .join(if cfg!(windows) { "zig.exe" } else { "zig" });
        if std::env::var_os("ZIG").is_none() && zig.exists() {
            command.env("ZIG", zig);
        }
        let source = cache.join(SOURCE);
        if std::env::var_os("GHOSTTY_SOURCE_DIR").is_none() && source.join("build.zig").exists() {
            command.env("GHOSTTY_SOURCE_DIR", source);
        }
    }
}
pub fn setup(root: &Path) -> Result<(), String> {
    let cache = cache().ok_or("set VELK_TOOLCHAIN_DIR for the terminal build cache")?;
    super::checked(
        Command::new("python3")
            .arg(root.join("scripts/terminal-tools.py"))
            .arg("--cache")
            .arg(cache),
    )
}
