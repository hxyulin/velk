//! Safe author-facing lifecycle. Raw ABI operations are confined to `ffi`.
//! The optional `markup` feature adds the standard styled-documentation adapter.
pub use ffi::editing::Behavior as EditingBehavior;
pub use ffi::host::Host;
pub use ffi::registry::{Provider, Registrar};
pub use velk_diagnostics as diagnostics;
pub use velk_editing as editing;
#[cfg(feature = "markup")]
pub use velk_markup as markup;
pub use velk_plugin_api as api;
pub use velk_plugin_contracts::{buffers, file_types, host, interfaces};
pub use velk_semantic as semantic;
pub mod hooks {
    #[cfg(feature = "markup")]
    pub use crate::ffi::registry::RenderMarkdown;
    pub use crate::ffi::registry::{
        CommandHandler, Event, LanguageDefinition, Request, SemanticSessions,
    };
}
mod metadata;
#[cfg(feature = "test-support")]
pub mod test_support;
pub use velk_plugin_contracts as contracts;
pub use velk_plugin_contracts::{
    Capability, CapabilityDeclaration, LanguageServer, PluginMetadata, commands, settings,
};

pub trait Plugin: Default + 'static {
    const ID: &'static str;
    const VERSION: &'static str;
    /// Human-readable display name. Defaults to the ID for older implementations.
    const NAME: &'static str = Self::ID;
    /// README-style Markdown; use include_str! to embed a README.md.
    const README: &'static str = "";
    /// Declared access requirements, not enforced grants for native code.
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[];
    fn commands() -> Vec<commands::Declaration> {
        Vec::new()
    }
    fn contributions() -> Vec<commands::contributions::Contribution> {
        Vec::new()
    }
    fn file_types() -> Vec<file_types::FileType> {
        Vec::new()
    }
    fn settings() -> Vec<settings::Definition> {
        Vec::new()
    }
    /// Called on the plugin worker before feature hooks or lifecycle restore.
    /// Validate before mutating state; the snapshot contains declared keys only.
    fn configure(&mut self, _settings: &settings::Values) -> Result<(), String> {
        Ok(())
    }
    /// Called after metadata discovery and before configure/restore. Store this
    /// per-instance handle to query workspace data or request document operations.
    fn attach_host(&mut self, _host: Host) {}
    fn register(_reg: &mut Registrar<Self>) -> Result<(), String> {
        Ok(())
    }
    /// Short plain-text summary used in plugin lists. Rich documentation goes in README.
    fn describe(&mut self) -> String;
}
#[doc(hidden)]
pub mod ffi;
pub use velk_syntax as syntax;

pub mod backend;

/// Register a feature compiled into the same executable as its host.
pub fn builtin<P: Plugin>() -> api::PluginApi {
    ffi::builtin::<P>()
}
