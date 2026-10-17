use super::*;
use velk_editing::{Decision, Provider, Request, Role};
struct Lease {
    api: EditingApi,
    active: Arc<std::sync::RwLock<bool>>,
    _code: Arc<Code>,
    settings: velk_plugin_contracts::settings::Values,
    id: String,
    role: Role,
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditingLease")
            .field("plugin", &self.id)
            .field("role", &self.role)
            .finish()
    }
}
impl Provider for Lease {
    fn evaluate(&self, request: &Request) -> Result<Decision, String> {
        let active = self.active.read().map_err(|_| "editing lease poisoned")?;
        if !*active {
            return Ok(Decision::Decline);
        }
        if request.intent.role() != self.role {
            return Err("wrong editing role".into());
        }
        let mut request = request.clone();
        request.settings = self.settings.clone();
        request.validate()?;
        let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if bytes.len() > velk_editing::MAX_WIRE {
            return Err("editing request too large".into());
        }
        let mut reply = hooks::Reply {
            result: None,
            calls: 0,
        };
        // SAFETY: the read guard excludes retirement; code is pinned, and the
        // context-free callback is reentrant. All byte views last this call.
        let status = unsafe {
            (self.api.evaluate)(
                Bytes {
                    data: bytes.as_ptr(),
                    length: bytes.len(),
                },
                hooks::collect,
                (&mut reply as *mut hooks::Reply).cast(),
            )
        };
        if status != OK || reply.calls != 1 {
            return Err(format!("editing provider failed ({status})"));
        }
        let bytes = reply.result.ok_or("missing editing reply")??;
        let decision: Decision = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        decision.validate(&request)?;
        Ok(decision)
    }
}
impl Manager {
    pub fn editing_provider(&self, id: &str, role: Role) -> Result<Arc<dyn Provider>, String> {
        let generation = self
            .plugins
            .get(id)
            .filter(|p| !p.destroyed)
            .ok_or("editing plugin unavailable")?;
        Ok(Arc::new(Lease {
            api: *generation
                .editing
                .get(&role)
                .ok_or("editing role unavailable")?,
            active: generation.services_active.clone(),
            _code: generation
                .library
                .clone()
                .ok_or("editing code unavailable")?,
            settings: generation.info.settings_values.clone(),
            id: id.into(),
            role,
        }))
    }
    pub(crate) fn editing_providers(&self) -> velk_editing::Providers {
        self.plugins
            .iter()
            .flat_map(|(id, generation)| {
                generation.editing.keys().filter_map(|role| {
                    self.editing_provider(id, *role)
                        .ok()
                        .map(|provider| ((id.clone(), *role), provider))
                })
            })
            .collect()
    }
}
