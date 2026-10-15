//! Generic stateful LSP transport. Language policy comes from another plugin.
pub mod lsp;
mod text;
use velk_plugin_sdk::{LanguageServer, Plugin, semantic::*};
#[derive(Default)]
pub struct Lsp;
impl Plugin for Lsp {
    const ID: &'static str = "velk.lsp";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    // Tokio process signal handlers can outlive their reactor on Unix.
    const RESIDENT_CODE: bool = true;
    fn describe(&mut self) -> String {
        "Generic LSP transport".into()
    }
    fn register(reg: &mut velk_plugin_sdk::Registrar<Self>) -> Result<(), String> {
        reg.provide(
            velk_plugin_sdk::hooks::SemanticSessions,
            |_: &mut Self, definition: LanguageServer| -> Box<dyn Backend> {
                Box::new(Configured(definition))
            },
        )
    }
}

struct Configured(LanguageServer);
impl Backend for Configured {
    fn run(self: Box<Self>, session: Session) -> BackendFuture {
        Box::pin(async move {
            let d = self.0;
            let backend = lsp::LspBackend {
                executable: d.executable.into(),
                arguments: d.arguments.into_iter().map(Into::into).collect(),
                root: d.root.map(Into::into),
                initialization_options: serde_json::from_str(&d.initialization_options)
                    .map_err(|e| format!("LSP initialization options: {e}"))?,
                configuration: serde_json::from_str(&d.configuration)
                    .map_err(|e| format!("LSP configuration: {e}"))?,
            };
            Box::new(backend).run(session).await
        })
    }
}
