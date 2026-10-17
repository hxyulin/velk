//! Bundled native undo provider. Its engine is also available for linked bootstrap.
#[derive(Default)]
pub struct Undo;
impl velk_plugin_sdk::Plugin for Undo {
    const ID: &'static str = "velk.undo.tree";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Undo tree";
    const README: &'static str = include_str!("../README.md");
    fn register(registrar: &mut velk_plugin_sdk::Registrar<Self>) -> Result<(), String> {
        registrar.undo::<velk_undo_tree::Tree>()
    }
    fn describe(&mut self) -> String {
        "Bounded undo trees and exact history snapshots".into()
    }
}
