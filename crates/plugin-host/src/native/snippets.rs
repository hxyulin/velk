use super::*;
use velk_plugin_contracts::commands::snippet::{
    self, Expansion, Provider, Request, Response, Transform, Variables,
};
struct Lease {
    api: SnippetApi,
    active: Arc<std::sync::RwLock<bool>>,
    _code: Arc<Code>,
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SnippetLease")
    }
}
impl Provider for Lease {
    fn expand(&self, source: &str, variables: &Variables) -> Result<Expansion, String> {
        snippet::validate_input(source, variables)?;
        let Response::Expansion { expansion } = self.call(Request::Expand {
            source: source.into(),
            variables: variables.clone(),
        })?
        else {
            return Err("invalid snippet expansion response".into());
        };
        expansion.validate()?;
        Ok(expansion)
    }
    fn transform(&self, transform: &Transform, value: &str) -> Result<String, String> {
        transform.validate()?;
        if value.len() > 64 * 1024 {
            return Err("snippet transform input too large".into());
        }
        let Response::Text { text } = self.call(Request::Transform {
            transform: transform.clone(),
            value: value.into(),
        })?
        else {
            return Err("invalid snippet transform response".into());
        };
        if text.len() > 64 * 1024 || text.contains('\r') {
            return Err("invalid snippet transform output".into());
        }
        Ok(text)
    }
}
impl Lease {
    fn call(&self, request: Request) -> Result<Response, String> {
        let active = self.active.read().map_err(|_| "snippet lease poisoned")?;
        if !*active {
            return Err("snippet provider retired".into());
        }
        let input = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if input.len() > snippet::MAX_BYTES {
            return Err("snippet request too large".into());
        }
        let mut output = vec![0; snippet::MAX_BYTES];
        let mut length = 0;
        // SAFETY: the read guard excludes retirement and the code lease pins callbacks.
        // Input and initialized output remain caller-owned throughout this call.
        let status = unsafe {
            (self.api.evaluate)(
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
            return Err("invalid snippet response".into());
        }
        let response: Result<Response, String> =
            serde_json::from_slice(&output[..length]).map_err(|e| e.to_string())?;
        response
    }
}
impl Manager {
    pub fn snippet_provider(&self, id: &str) -> Result<Arc<dyn Provider>, String> {
        let generation = self
            .plugins
            .get(id)
            .filter(|g| !g.destroyed)
            .ok_or("snippet plugin unavailable")?;
        Ok(Arc::new(Lease {
            api: generation
                .snippet_provider
                .ok_or("snippet provider unavailable")?,
            active: generation.services_active.clone(),
            _code: generation
                .library
                .clone()
                .ok_or("snippet code unavailable")?,
        }))
    }
    pub(crate) fn snippet_providers(&self) -> snippet::Providers {
        self.plugins
            .keys()
            .filter_map(|id| self.snippet_provider(id).ok().map(|p| (id.clone(), p)))
            .collect()
    }
}
