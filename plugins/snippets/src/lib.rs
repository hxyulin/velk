//! Default snippet syntax and bounded regex transforms.
mod expansion;
mod transform;
pub use expansion::{expand, expand_with_variables};
use transform::TransformExt;
use velk_commands::snippet::{Expansion, Provider, Transform, Variables};
#[derive(Debug, Default)]
pub struct Engine;
impl Provider for Engine {
    fn expand(&self, source: &str, variables: &Variables) -> Result<Expansion, String> {
        expand_with_variables(source, variables)
    }
    fn transform(&self, transform: &Transform, value: &str) -> Result<String, String> {
        TransformExt::apply(transform, value)
    }
}
#[derive(Default)]
pub struct Snippets;
impl velk_plugin_sdk::Plugin for Snippets {
    const ID: &'static str = "velk.snippets";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Snippets";
    const README: &'static str = include_str!("../README.md");
    fn register(reg: &mut velk_plugin_sdk::Registrar<Self>) -> Result<(), String> {
        reg.snippets::<Engine>()
    }
    fn describe(&mut self) -> String {
        "Expand snippet templates and evaluate placeholder transforms".into()
    }
}
