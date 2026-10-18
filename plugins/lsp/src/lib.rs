//! Generic stateful LSP transport. Language policy comes from another plugin.
pub mod lsp;
mod text;
use velk_plugin_sdk::{LanguageServer, Plugin, semantic::*};
#[derive(Default)]
pub struct Lsp {
    client: std::sync::Arc<lsp::Client>,
}
impl Plugin for Lsp {
    const ID: &'static str = "velk.lsp";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Language Server Protocol";
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [velk_plugin_sdk::CapabilityDeclaration] = &[
        velk_plugin_sdk::CapabilityDeclaration::required(
            "process.spawn",
            "Start and communicate with configured language-server processes.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            "editor.buffer.read",
            "Send document contents and changes to language servers.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            "editor.diagnostics.publish",
            "Publish workspace diagnostics, including files without open buffers.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::optional(
            "editor.buffer.write",
            "Propose formatting, refactoring and completion edits for the host to validate.",
        ),
    ];

    // Tokio process signal handlers can outlive their reactor on Unix.
    fn attach_host(&mut self, host: velk_plugin_sdk::Host) {
        self.client.attach_host(host);
    }
    fn commands() -> Vec<velk_plugin_sdk::commands::Declaration> {
        use velk_plugin_sdk::commands::{Declaration, Handler};
        let mut declarations:Vec<_> = [
            ("definition", "Go to definition", "textDocument/definition", "/definitionProvider"),
            ("type-definition", "Go to type definition", "textDocument/typeDefinition", "/typeDefinitionProvider"),
            ("implementation", "Go to implementation", "textDocument/implementation", "/implementationProvider"),
            ("references", "Find references", "textDocument/references", "/referencesProvider"),
        ].into_iter().map(|(id, title, method, capability)| Declaration { input:None, shortcut:None,
            id:format!("velk.lsp.{id}"), title:title.into(), language:None,
            handler:Handler::Semantic { payload:serde_json::json!({"method":method,"result":"locations","capability":capability}) },
        }).collect();
        for (id, title, method, result, capability, input, shortcut) in [
            (
                "signature-help",
                "Show signature help",
                "textDocument/signatureHelp",
                "signature_help",
                "/signatureHelpProvider",
                None,
                Some(velk_plugin_sdk::commands::Shortcut {
                    key: "Space".into(),
                    control: true,
                    shift: true,
                    ..Default::default()
                }),
            ),
            (
                "format",
                "Format document",
                "textDocument/formatting",
                "text_edits",
                "/documentFormattingProvider",
                None,
                Some(velk_plugin_sdk::commands::Shortcut {
                    key: "f".into(),
                    alt: true,
                    shift: true,
                    ..Default::default()
                }),
            ),
            (
                "rename",
                "Rename symbol",
                "textDocument/rename",
                "workspace_edit",
                "/renameProvider",
                Some("New name"),
                Some(velk_plugin_sdk::commands::Shortcut {
                    key: "F2".into(),
                    ..Default::default()
                }),
            ),
            (
                "code-actions",
                "Code actions and quick fixes",
                "textDocument/codeAction",
                "code_actions",
                "/codeActionProvider",
                None,
                Some(velk_plugin_sdk::commands::Shortcut {
                    key: ".".into(),
                    primary: true,
                    ..Default::default()
                }),
            ),
        ] {
            declarations.push(Declaration {id:format!("velk.lsp.{id}"),title:title.into(),language:None,input:input.map(Into::into),shortcut,handler:Handler::Semantic {payload:serde_json::json!({"method":method,"result":result,"capability":capability})}});
        }
        declarations[0].shortcut = Some(velk_plugin_sdk::commands::Shortcut {
            key: "F12".into(),
            ..Default::default()
        });
        declarations
    }
    fn contributions() -> Vec<velk_plugin_sdk::commands::contributions::Contribution> {
        use velk_plugin_sdk::commands::{Declaration, Handler, contributions::Contribution};
        vec![Contribution {
            metadata: velk_completion_contracts::Triggers {
                typing: true,
                dynamic_characters: true,
                ..Default::default()
            }
            .metadata(),
            topic: velk_completion_contracts::TOPIC.into(),
            command: Declaration {
                id: "velk.lsp.complete".into(),
                title: "Language server completions".into(),
                language: None,
                input: None,
                shortcut: None,
                handler: Handler::Semantic {
                    payload: serde_json::json!({
                        "method":"textDocument/completion", "result":"completion", "capability":"/completionProvider"
                    }),
                },
            },
        }]
    }
    fn describe(&mut self) -> String {
        "Generic LSP transport".into()
    }
    fn register(reg: &mut velk_plugin_sdk::Registrar<Self>) -> Result<(), String> {
        reg.provide(
            velk_plugin_sdk::hooks::Request(
                velk_plugin_sdk::contracts::hooks::SEMANTIC_RETIRE_IDLE_HOOK,
            ),
            |this: &mut Self, _: &[u8]| {
                this.client.retire_idle();
                Ok(Vec::new())
            },
        )?;
        reg.provide(
            velk_plugin_sdk::hooks::SemanticSessions,
            |this: &mut Self, definition: LanguageServer| -> Box<dyn Backend> {
                Box::new(Configured(definition, this.client.clone()))
            },
        )
    }
}

struct Configured(LanguageServer, std::sync::Arc<lsp::Client>);
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
            self.1.run(backend, session).await
        })
    }
}
