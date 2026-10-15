//! Safe author-facing lifecycle. Raw ABI operations are confined to `ffi`.
pub use ffi::registry::{Provider, Registrar};
pub use velk_diagnostics as diagnostics;
pub use velk_markup as markup;
pub use velk_plugin_api as api;
pub use velk_semantic as semantic;
pub mod hooks {
    pub use crate::ffi::registry::{
        Event, LanguageDefinition, RenderMarkdown, Request, SemanticSessions,
    };
}
/// Owned definition copied independently into each library's allocator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageServer {
    pub transport: String,
    pub language: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub root: Option<String>,
    pub initialization_options: String,
    pub configuration: String,
}
pub trait Plugin: Default + 'static {
    const ID: &'static str;
    const VERSION: &'static str;
    const RESIDENT_CODE: bool = false;
    fn register(_reg: &mut Registrar<Self>) -> Result<(), String> {
        Ok(())
    }
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
