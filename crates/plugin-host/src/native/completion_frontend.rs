use super::*;
use velk_plugin_contracts::commands::completion::frontend::{self, Provider, Snapshot, View};
struct Lease {
    api: CompletionFrontendApi,
    active: Arc<std::sync::RwLock<bool>>,
    _code: Arc<Code>,
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompletionFrontendLease")
    }
}
impl Provider for Lease {
    fn present(&self, snapshot: &Snapshot) -> Result<View, String> {
        let _scope =
            velk_diagnostics::Scope::new("plugin.completion_frontend", "velk_plugin_host", false);
        let active = self.active.read().map_err(|_| "frontend lease poisoned")?;
        if !*active {
            return Err("completion frontend retired".into());
        }
        snapshot.validate()?;
        let input = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
        if input.len() > frontend::MAX_BYTES {
            return Err("frontend request too large".into());
        }
        let output = super::output::bounded_output(frontend::MAX_BYTES, |output, length| {
            // SAFETY: the active read guard excludes retirement; code and caller storage stay live.
            unsafe {
                (self.api.present)(
                    Bytes {
                        data: input.as_ptr(),
                        length: input.len(),
                    },
                    output.as_mut_ptr(),
                    output.len(),
                    length,
                )
            }
        })?;
        let view: View = serde_json::from_slice(&output).map_err(|e| e.to_string())?;
        view.validate(snapshot)?;
        Ok(view)
    }
}
impl Manager {
    pub fn completion_frontend(&self, id: &str) -> Result<Arc<dyn Provider>, String> {
        let generation = self
            .plugins
            .get(id)
            .filter(|g| !g.destroyed)
            .ok_or("frontend plugin unavailable")?;
        Ok(Arc::new(Lease {
            api: generation
                .completion_frontend
                .ok_or("completion frontend unavailable")?,
            active: generation.services_active.clone(),
            _code: generation
                .library
                .clone()
                .ok_or("frontend code unavailable")?,
        }))
    }
    pub(crate) fn completion_frontends(&self) -> frontend::Providers {
        self.plugins
            .keys()
            .filter_map(|id| self.completion_frontend(id).ok().map(|p| (id.clone(), p)))
            .collect()
    }
}
