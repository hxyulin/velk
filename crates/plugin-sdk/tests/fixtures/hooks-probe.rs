//! Test-only native probe, never staged among bundled application plugins.
use velk_plugin_sdk::{
    Plugin, Registrar, commands,
    hooks::{Event, Request},
};
#[derive(Default)]
struct Probe {
    last: Vec<u8>,
    host: Option<velk_plugin_sdk::Host>,
}
impl Plugin for Probe {
    const ID: &'static str = "test.hooks";
    const NAME: &'static str = "Named Hooks Probe";
    const README: &'static str = "# Probe\n\n**Markdown** documentation 世界";
    const CAPABILITIES: &'static [velk_plugin_sdk::CapabilityDeclaration] = &[
        velk_plugin_sdk::CapabilityDeclaration::required(
            "editor.buffer.read",
            "Read test buffers.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::optional(
            "network.http",
            "Fetch optional test docs.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            "workspace.read",
            "Read test workspace context.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            "filesystem.project.read",
            "Read project test files.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            "editor.buffer.manage",
            "Exercise host buffer lifecycle.",
        ),
    ];
    fn attach_host(&mut self, host: velk_plugin_sdk::Host) {
        self.host = Some(host);
    }
    const VERSION: &'static str = "1";
    fn commands() -> Vec<velk_plugin_sdk::commands::Declaration> {
        vec![velk_plugin_sdk::commands::Declaration {
            input: None,
            shortcut: None,
            id: "test.show-context".into(),
            title: "Show command context".into(),
            language: None,
            handler: velk_plugin_sdk::commands::Handler::Request {
                hook: "test.command".into(),
            },
        }]
    }
    fn settings() -> Vec<velk_plugin_sdk::settings::Definition> {
        vec![velk_plugin_sdk::settings::Definition {
            key: "message".into(),
            title: "Initial message".into(),
            description: "Probe startup state.".into(),
            kind: velk_plugin_sdk::settings::Kind::Text { max_length: 128 },
            default: "initial".into(),
        }]
    }
    fn configure(&mut self, values: &velk_plugin_sdk::settings::Values) -> Result<(), String> {
        self.last = values["message"]
            .as_str()
            .ok_or("invalid probe message")?
            .as_bytes()
            .to_vec();
        Ok(())
    }
    fn describe(&mut self) -> String {
        "Named registry probe".into()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(Request("test.host"), |p: &mut Self, input: &[u8]| {
            let request = serde_json::from_slice(input).map_err(|e| e.to_string())?;
            let host = p.host.as_ref().ok_or("host services not attached")?.clone();
            let response = host.request(request)?;
            serde_json::to_vec(&response).map_err(|e| e.to_string())
        })?;
        reg.editing::<EditingProbe>()?;
        reg.snippets::<SnippetProbe>()?;
        reg.matching::<MatchingProbe>()?;
        reg.completion_frontend::<FrontendProbe>()?;
        reg.provide(
            velk_plugin_sdk::hooks::CommandHandler {
                name: "test.command",
            },
            |_: &mut Self, context: velk_plugin_sdk::commands::Context| {
                Ok(velk_plugin_sdk::commands::Outcome::Message {
                    text: format!(
                        "{}:{}",
                        context.revision.unwrap_or_default(),
                        context.position.unwrap_or_default()
                    ),
                })
            },
        )?;
        reg.provide(Request("test.echo"), |_: &mut Self, input: &[u8]| {
            std::thread::spawn(|| {
                velk_plugin_sdk::diagnostics::log!(
                    velk_plugin_sdk::diagnostics::Level::Info,
                    "native logging probe 世界"
                );
                velk_plugin_sdk::diagnostics::log!(
                    velk_plugin_sdk::diagnostics::Level::Debug,
                    "filtered logging probe"
                );
            })
            .join()
            .unwrap();
            Ok(input.to_vec())
        })?;
        reg.provide(Request("test.state"), |p: &mut Self, _: &[u8]| {
            Ok(p.last.clone())
        })?;
        reg.on(Event("test.notice"), |p: &mut Self, input: &[u8]| {
            p.last = input.to_vec();
            Ok(())
        })?;
        reg.provide(Request("test.error"), |_: &mut Self, _: &[u8]| {
            Err("test error".into())
        })?;
        reg.provide(Request("test.panic"), |_: &mut Self, _: &[u8]| {
            panic!("contained test request panic")
        })?;
        Ok(())
    }
}
velk_plugin_sdk::export_plugin!(Probe);

struct EditingProbe;
impl velk_plugin_sdk::EditingBehavior for EditingProbe {
    const ROLE: velk_plugin_sdk::editing::Role = velk_plugin_sdk::editing::Role::Pairing;
    fn evaluate(
        request: &velk_plugin_sdk::editing::Request,
    ) -> Result<velk_plugin_sdk::editing::Decision, String> {
        use velk_plugin_sdk::editing::*;
        if request.intent == Intent::Type("invalid".into()) {
            return Ok(Decision::Apply(Proposal {
                revision: request.revision + 1,
                edits: vec![],
                selection: None,
                pairs: vec![],
            }));
        }
        Ok(Decision::Apply(Proposal {
            revision: request.revision,
            edits: vec![Edit {
                start: request.selection.range().start,
                end: request.selection.range().end,
                text: "()".into(),
            }],
            selection: Some(Selection::caret(request.selection.range().start + 1)),
            pairs: vec![],
        }))
    }
}

#[derive(Debug, Default)]
struct MatchingProbe;
impl velk_plugin_sdk::commands::completion::matching::Policy for MatchingProbe {
    fn scores(&self, query: &str, candidates: &[&str]) -> Result<Vec<Option<[u64; 4]>>, String> {
        if query == "panic" {
            panic!("contained matcher panic");
        }
        Ok(candidates
            .iter()
            .map(|candidate| {
                candidate
                    .ends_with(query)
                    .then_some([candidate.len() as u64, 0, 0, 0])
            })
            .collect())
    }
}

#[derive(Debug, Default)]
struct FrontendProbe;
impl velk_plugin_sdk::commands::completion::frontend::Provider for FrontendProbe {
    fn present(
        &self,
        snapshot: &velk_plugin_sdk::commands::completion::frontend::Snapshot,
    ) -> Result<velk_plugin_sdk::commands::completion::frontend::View, String> {
        use velk_plugin_sdk::commands::completion::frontend::*;
        if snapshot
            .candidates
            .first()
            .is_some_and(|c| c.title == "panic")
        {
            panic!("frontend panic");
        }
        let mut view = Fallback.present(snapshot)?;
        if snapshot
            .candidates
            .first()
            .is_some_and(|c| c.title == "invalid")
        {
            view.rows[0].candidate = 9999;
        }
        Ok(view)
    }
}

#[derive(Debug, Default)]
struct SnippetProbe;
impl commands::snippet::Provider for SnippetProbe {
    fn expand(
        &self,
        source: &str,
        _: &commands::snippet::Variables,
    ) -> Result<commands::snippet::Expansion, String> {
        if source == "panic" {
            panic!("snippet probe panic");
        }
        let text = format!("custom:{source}");
        let end = text.chars().count();
        Ok(commands::snippet::Expansion {
            text,
            stops: vec![commands::snippet::Tabstop {
                index: 0,
                range: if source == "invalid" {
                    end + 1..end + 1
                } else {
                    end..end
                },
                choices: vec![],
                transform: None,
            }],
        })
    }
    fn transform(&self, _: &commands::snippet::Transform, value: &str) -> Result<String, String> {
        Ok(if value == "invalid" {
            "\r".into()
        } else {
            format!("custom:{value}")
        })
    }
}
