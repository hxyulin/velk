//! Rust-specific policy only. The generic LSP plugin owns all protocol/runtime work.
use velk_plugin_sdk::{LanguageServer, Plugin};
#[derive(Default)]
pub struct RustAnalyzer;
impl Plugin for RustAnalyzer {
    const ID: &'static str = "velk.language-server.rust-analyzer";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    fn register(reg: &mut velk_plugin_sdk::Registrar<Self>) -> Result<(), String> {
        reg.provide(
            velk_plugin_sdk::hooks::LanguageDefinition,
            Self::language_server,
        )
    }
    fn describe(&mut self) -> String {
        "Rust support via rust-analyzer · Requires velk.lsp".into()
    }
}
impl RustAnalyzer {
    fn language_server(&mut self, language: &str) -> Result<Option<LanguageServer>, String> {
        if language != "rust" {
            return Ok(None);
        }
        let config = if let Some(path) = std::env::var_os("VELK_LSP_CONFIG") {
            if std::fs::metadata(&path)
                .map_err(|e| format!("Rust language-server configuration: {e}"))?
                .len()
                > 1024 * 1024
            {
                return Err("Rust language-server configuration exceeds 1 MiB".into());
            }
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("Rust language-server configuration: {e}"))?
        } else {
            Configuration::defaults(
                std::env::var("VELK_RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".into()),
            )
        };
        Ok(Some(config.definition()))
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    executable: String,
    #[serde(default)]
    arguments: Vec<String>,
    root: Option<String>,
    #[serde(default)]
    initialization_options: serde_json::Value,
    #[serde(default)]
    configuration: serde_json::Value,
}
impl Configuration {
    fn defaults(executable: String) -> Self {
        Self {
            executable,
            arguments: vec![],
            root: None,
            initialization_options: serde_json::json!({"checkOnSave":false}),
            configuration: serde_json::json!({"rust-analyzer":{"checkOnSave":false}}),
        }
    }
    fn definition(self) -> LanguageServer {
        LanguageServer {
            transport: "velk.lsp".into(),
            language: "rust".into(),
            executable: self.executable,
            arguments: self.arguments,
            root: self.root,
            initialization_options: self.initialization_options.to_string(),
            configuration: self.configuration.to_string(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_policy_is_a_namespaced_definition_with_an_explicit_transport_dependency() {
        let definition = Configuration::defaults("/tools/rust-analyzer".into()).definition();
        assert_eq!(definition.transport, "velk.lsp");
        assert_eq!(definition.language, "rust");
        assert_eq!(definition.executable, "/tools/rust-analyzer");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&definition.configuration).unwrap()["rust-analyzer"]
                ["checkOnSave"],
            false
        );
        assert!(RustAnalyzer.language_server("python").unwrap().is_none());
        assert!(velk_plugin_sdk::api::parse_plugin_id(RustAnalyzer::ID).is_some());
    }
}
