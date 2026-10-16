//! Shared plugin definitions without a native ABI, loader, or authoring runtime.
pub mod hooks;
mod id;
mod language_server;
mod metadata;
pub mod settings;

pub use id::{PluginId, parse_plugin_id};
pub use language_server::LanguageServer;
pub use metadata::{Capability, CapabilityDeclaration, PluginMetadata};
