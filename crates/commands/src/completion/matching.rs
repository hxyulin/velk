//! Reentrant local ranking contract. Providers must perform bounded CPU work only.
use std::sync::Arc;
pub const CONTRACT: &str = "velk.matching.v1";
pub const HOOK: &str = "velk.matching.v1";
/// Lower scores rank first. Equal scores retain provider order.
pub type Score = [u64; 4];
pub trait Policy: std::fmt::Debug + Send + Sync {
    fn scores(&self, query: &str, candidates: &[&str]) -> Result<Vec<Option<Score>>, String>;
}
pub type Policies = std::collections::BTreeMap<String, Arc<dyn Policy>>;
/// Safe fallback when no ranking service is selected or its lease retires.
#[derive(Debug)]
pub struct Prefix;
impl Policy for Prefix {
    fn scores(&self, query: &str, candidates: &[&str]) -> Result<Vec<Option<Score>>, String> {
        let query = query.to_lowercase();
        Ok(candidates
            .iter()
            .map(|candidate| {
                candidate
                    .to_lowercase()
                    .starts_with(&query)
                    .then_some([0; 4])
            })
            .collect())
    }
}
