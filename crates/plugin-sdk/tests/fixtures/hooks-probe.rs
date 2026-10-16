//! Test-only native probe, never staged among bundled application plugins.
use velk_plugin_sdk::{
    Plugin, Registrar,
    hooks::{Event, Request},
};
#[derive(Default)]
struct Probe {
    last: Vec<u8>,
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
    ];
    const VERSION: &'static str = "1";
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
        reg.provide(Request("test.echo"), |_: &mut Self, input: &[u8]| {
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
