//! Rust-specific policy only. The generic LSP plugin owns all protocol/runtime work.
use velk_plugin_sdk::{LanguageServer, Plugin};
#[derive(Default)]
pub struct RustAnalyzer {
    executable: Option<String>,
}
impl Plugin for RustAnalyzer {
    const ID: &'static str = "velk.language-server.rust-analyzer";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Rust Analyzer";
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [velk_plugin_sdk::CapabilityDeclaration] = &[
        velk_plugin_sdk::CapabilityDeclaration::optional(
            "environment.read",
            "Read language-server executable and configuration overrides.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::optional(
            "filesystem.read",
            "Read an explicitly configured language-server configuration file.",
        ),
    ];
    fn commands() -> Vec<velk_plugin_sdk::commands::Declaration> {
        use velk_plugin_sdk::commands::{Declaration, Handler};
        [
            ("parent-module", "Rust: Go to parent module", "experimental/parentModule", "locations", Some("/experimental/parentModule")),
            ("expand-macro", "Rust: Expand macro", "rust-analyzer/expandMacro", "macro_expansion", None),
        ].into_iter().map(|(id, title, method, result, capability)| Declaration { input:None, shortcut:None,
            id:format!("{}.{}", Self::ID, id), title:title.into(), language:Some("rust".into()),
            handler:Handler::Semantic { payload:serde_json::json!({"method":method,"result":result,"capability":capability}) },
        }).collect()
    }
    fn settings() -> Vec<velk_plugin_sdk::settings::Definition> {
        use velk_plugin_sdk::settings::{Definition, Kind};
        vec![Definition {
            key: "executable".into(), title: "Executable".into(),
            description: "Path or command for rust-analyzer. Empty uses the existing environment override or PATH.".into(),
            kind: Kind::Text { max_length: 4096 }, default: "".into(),
        }]
    }
    fn configure(&mut self, values: &velk_plugin_sdk::settings::Values) -> Result<(), String> {
        let executable = values["executable"]
            .as_str()
            .ok_or("invalid rust-analyzer executable")?;
        self.executable = (!executable.is_empty()).then(|| executable.into());
        Ok(())
    }
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
        let mut config = if let Some(path) = std::env::var_os("VELK_LSP_CONFIG") {
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
        if let Some(executable) = &self.executable {
            config.executable.clone_from(executable);
        }
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
            initialization_options: serde_json::json!({"checkOnSave":true}),
            configuration: serde_json::json!({"rust-analyzer":{"checkOnSave":true}}),
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
            serde_json::from_str::<serde_json::Value>(&definition.initialization_options).unwrap()
                ["checkOnSave"],
            true
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&definition.configuration).unwrap()["rust-analyzer"]
                ["checkOnSave"],
            true
        );
        assert!(
            RustAnalyzer::default()
                .language_server("python")
                .unwrap()
                .is_none()
        );
        assert!(velk_plugin_sdk::api::parse_plugin_id(RustAnalyzer::ID).is_some());
    }
    #[test]
    fn user_executable_override_is_applied_to_the_language_definition() {
        let mut plugin = RustAnalyzer::default();
        plugin
            .configure(&velk_plugin_sdk::settings::Values::from([(
                "executable".into(),
                "/configured/rust-analyzer".into(),
            )]))
            .unwrap();
        assert_eq!(
            plugin.language_server("rust").unwrap().unwrap().executable,
            "/configured/rust-analyzer"
        );
        plugin
            .configure(&velk_plugin_sdk::settings::Values::from([(
                "executable".into(),
                "".into(),
            )]))
            .unwrap();
        assert!(plugin.executable.is_none());
    }
}
