//! Shared plugin definitions without a native ABI, loader, or authoring runtime.
pub mod buffers;
pub mod hooks;
pub mod host;
mod id;
mod language_server;
mod metadata;
pub mod settings;

pub use id::{PluginId, parse_plugin_id};
pub use language_server::LanguageServer;
pub use metadata::{Capability, CapabilityDeclaration, PluginMetadata};

pub use velk_commands as commands;

pub use velk_syntax as syntax;
