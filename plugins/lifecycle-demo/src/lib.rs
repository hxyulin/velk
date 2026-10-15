//! Bundled lifecycle proof; intentionally not part of the editor's feature set.
use velk_plugin_sdk::Plugin;
#[derive(Default)]
struct LifecycleDemo;
impl Plugin for LifecycleDemo {
    const ID: &'static str = "velk.lifecycle-demo";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    fn describe(&mut self) -> String {
        option_env!("VELK_PLUGIN_DEMO_MESSAGE")
            .unwrap_or("Native plugin loaded")
            .into()
    }
}
velk_plugin_sdk::export_plugin!(LifecycleDemo);
