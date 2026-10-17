use super::*;
use velk_plugin_contracts::commands::completion::matching::{self, Policy, Score};
struct Lease {
    api: MatchingApi,
    active: Arc<std::sync::RwLock<bool>>,
    _code: Arc<Code>,
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MatchingLease")
    }
}
impl Policy for Lease {
    fn scores(&self, query: &str, candidates: &[&str]) -> Result<Vec<Option<Score>>, String> {
        let active = self.active.read().map_err(|_| "matching lease poisoned")?;
        if !*active {
            return Err("matching provider retired".into());
        }
        if query.len() > 4096
            || candidates.len() > 1000
            || candidates.iter().any(|value| value.len() > 4096)
        {
            return Err("matching request too large".into());
        }
        let bytes = |value: &str| Bytes {
            data: value.as_ptr(),
            length: value.len(),
        };
        let candidates: Vec<_> = candidates
            .iter()
            .map(|candidate| bytes(candidate))
            .collect();
        let mut output = vec![MatchScore::default(); candidates.len()];
        // SAFETY: the read guard excludes retirement, code is pinned, and all
        // initialized byte views and output slots remain live for this call.
        let status = unsafe {
            (self.api.scores)(
                bytes(query),
                candidates.as_ptr(),
                candidates.len(),
                output.as_mut_ptr(),
            )
        };
        if status != OK || output.iter().any(|score| score.matched > 1) {
            return Err("invalid matching response".into());
        }
        Ok(output
            .into_iter()
            .map(|score| (score.matched == 1).then_some(score.score))
            .collect())
    }
}
impl Manager {
    pub fn matching_provider(&self, id: &str) -> Result<Arc<dyn Policy>, String> {
        let generation = self
            .plugins
            .get(id)
            .filter(|generation| !generation.destroyed)
            .ok_or("matching plugin unavailable")?;
        Ok(Arc::new(Lease {
            api: generation.matching.ok_or("matching service unavailable")?,
            active: generation.services_active.clone(),
            _code: generation
                .library
                .clone()
                .ok_or("matching code unavailable")?,
        }))
    }
    pub(crate) fn matching_providers(&self) -> matching::Policies {
        self.plugins
            .keys()
            .filter_map(|id| {
                self.matching_provider(id)
                    .ok()
                    .map(|provider| (id.clone(), provider))
            })
            .collect()
    }
}
