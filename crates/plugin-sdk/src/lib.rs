//! Safe author-facing lifecycle. Raw ABI operations are confined to `ffi`.
pub use velk_markup as markup;
pub use velk_plugin_api as api;
pub trait Plugin: Default + 'static {
    const ID: &'static str;
    const VERSION: &'static str;
    const MARKDOWN: bool = false;
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
