//! Invalid metadata must be rejected before either factory is instantiated.
use velk_plugin_sdk::Plugin;
struct Duplicate;
impl Default for Duplicate {
    fn default() -> Self {
        panic!("duplicate pack factories must never be called")
    }
}
impl Plugin for Duplicate {
    const ID: &'static str = "test.duplicate";
    const VERSION: &'static str = "1";
    fn describe(&mut self) -> String {
        unreachable!()
    }
}
velk_plugin_sdk::export_plugin_pack!(Duplicate, Duplicate);
