//! Registration tables for features linked into this executable.
use super::*;

pub(super) struct Code {
    pub apis: Vec<PluginApi>,
}
impl Code {
    pub fn builtin() -> Arc<Self> {
        Arc::new(Self {
            apis: velk_plugin_builtin::entries(),
        })
    }
}
