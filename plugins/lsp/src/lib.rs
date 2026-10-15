//! Stateful native LSP provider. The protocol and child process live here.
pub mod lsp;
mod text;
use velk_plugin_sdk::{Plugin, semantic::*};
#[derive(Default)]
pub struct Lsp;
impl Plugin for Lsp {
    const ID: &'static str = "velk.lsp";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const SEMANTIC: bool = true;
    // Tokio process signal handlers can outlive their reactor on Unix.
    const RESIDENT_CODE: bool = true;
    fn describe(&mut self) -> String {
        "LSP semantic provider (rust-analyzer)".into()
    }
    fn semantic_backend(&mut self) -> Box<dyn Backend> {
        Box::new(Configured)
    }
}
velk_plugin_sdk::export_plugin!(Lsp);

/// Configuration is private to the plugin; no LSP settings leak into the ABI.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    executable: String,
    #[serde(default)]
    arguments: Vec<String>,
    root: Option<std::path::PathBuf>,
    #[serde(default)]
    initialization_options: serde_json::Value,
    #[serde(default)]
    configuration: serde_json::Value,
}
struct Configured;
impl Backend for Configured {
    fn run(self: Box<Self>, session: Session) -> BackendFuture {
        Box::pin(async move {
            let backend = if let Some(path) = std::env::var_os("VELK_LSP_CONFIG") {
                let metadata =
                    std::fs::metadata(&path).map_err(|e| format!("LSP configuration: {e}"))?;
                if metadata.len() > 1024 * 1024 {
                    return Err("LSP configuration exceeds 1 MiB".into());
                }
                let config: Configuration =
                    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                        .map_err(|e| format!("LSP configuration: {e}"))?;
                lsp::LspBackend {
                    executable: config.executable.into(),
                    arguments: config.arguments.into_iter().map(Into::into).collect(),
                    root: config.root,
                    initialization_options: config.initialization_options,
                    configuration: config.configuration,
                }
            } else {
                lsp::LspBackend::rust_analyzer()
            };
            Box::new(backend).run(session).await
        })
    }
}
