//! Safe author-facing lifecycle. Raw ABI operations are confined to `ffi`.
//! The optional `markup` feature adds the standard styled-documentation adapter.
pub use ffi::registry::{Provider, Registrar};
pub use velk_diagnostics as diagnostics;
#[cfg(feature = "markup")]
pub use velk_markup as markup;
pub use velk_plugin_api as api;
pub use velk_semantic as semantic;
pub mod hooks {
    #[cfg(feature = "markup")]
    pub use crate::ffi::registry::RenderMarkdown;
    pub use crate::ffi::registry::{
        CommandHandler, Event, LanguageDefinition, Request, SemanticSessions,
    };
}
mod metadata;
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
    const RESIDENT_CODE: bool = false;
    fn commands() -> Vec<commands::Declaration> {
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
    fn register(_reg: &mut Registrar<Self>) -> Result<(), String> {
        Ok(())
    }
    /// Short plain-text summary used in plugin lists. Rich documentation goes in README.
    fn describe(&mut self) -> String;
}
#[doc(hidden)]
pub mod ffi;
/// Export exactly one plugin from a cdylib. The generated wrapper delegates all
/// pointer handling and panic containment to the SDK.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        unsafe extern "C" fn velk_plugin_entry_v1(
            abi: u32,
            output: *mut $crate::api::PluginApi,
            size: usize,
        ) -> i32 {
            // SAFETY: called by a host satisfying the documented v1 ABI.
            unsafe { $crate::ffi::entry::<$plugin>(abi, output, size) }
        }
    };
}

/// Export several implementations from one native image. Implementations must
/// not also export standalone entry points in their library dependencies.
#[macro_export]
macro_rules! export_plugin_pack {
    ($($plugin:ty),+ $(,)?) => {
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        unsafe extern "C" fn velk_plugin_pack_entry_v1(
            abi: u32,
            output: *mut $crate::api::PluginPackApi,
            size: usize,
        ) -> i32 {
            #[allow(unsafe_code)]
            unsafe extern "C" fn member(index: usize, abi: u32, output: *mut $crate::api::PluginApi, size: usize) -> i32 {
                let entries: &[$crate::api::Entry] = &[$($crate::ffi::member::<$plugin>),+];
                match entries.get(index) {
                    Some(entry) => unsafe { entry(abi, output, size) },
                    None => $crate::api::NOT_FOUND,
                }
            }
            let count = [$(stringify!($plugin)),+].len();
            unsafe { $crate::ffi::pack_entry(abi, output, size, count, member) }
        }
    };
}
