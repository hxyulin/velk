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
        let active = self.active.read().map_err(|_| "frontend lease poisoned")?;
        if !*active {
            return Err("completion frontend retired".into());
        }
        snapshot.validate()?;
        let input = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
        if input.len() > frontend::MAX_BYTES {
            return Err("frontend request too large".into());
        }
        let mut output = vec![0; frontend::MAX_BYTES];
        let mut length = 0;
        // SAFETY: the read guard excludes retirement and the code lease pins callbacks.
        // Input and initialized output remain caller-owned throughout this call.
        let status = unsafe {
            (self.api.present)(
                Bytes {
                    data: input.as_ptr(),
                    length: input.len(),
                },
                output.as_mut_ptr(),
                output.len(),
                &mut length,
            )
        };
        if status != OK || length > output.len() {
            return Err("invalid frontend response".into());
        }
        let view: View = serde_json::from_slice(&output[..length]).map_err(|e| e.to_string())?;
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
