//! Safe author-facing lifecycle. Raw ABI operations are confined to `ffi`.
pub use velk_markup as markup;
pub use velk_plugin_api as api;
pub use velk_semantic as semantic;
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
    const MARKDOWN: bool = false;
    const SEMANTIC: bool = false;
    const LANGUAGE_SERVER: bool = false;
    fn language_server(&mut self, _language: &str) -> Result<Option<LanguageServer>, String> {
        Ok(None)
    }
    const RESIDENT_CODE: bool = false;
    /// Each returned backend owns one independent session. The SDK cancels and
    /// joins its reactor before native session destruction returns.
    fn semantic_backend(&mut self, _definition: LanguageServer) -> Box<dyn semantic::Backend> {
        panic!("plugin does not provide semantics")
    }
    fn describe(&mut self) -> String;
    /// Executed on a host worker; implementations retain no input or callbacks.
    fn render_markdown(&mut self, source: &str) -> markup::Document {
        markup::Document::plain(source)
    }
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
