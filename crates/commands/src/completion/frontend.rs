//! Declarative, reentrant completion presentation. No edits or UI handles are exposed.
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub const HOOK: &str = "velk.completion.frontend.v1";
pub const CONTRACT: &str = HOOK;
pub const MAX_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub title: String,
    pub detail: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Candidates are already filtered and ranked. Row indices refer to this list.
    pub candidates: Vec<Candidate>,
    pub selected: usize,
    pub viewport: [f32; 2],
    pub documentation: bool,
    pub ready: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub candidate: usize,
    pub title: String,
    pub detail: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Documentation {
    Hidden,
    Below,
    Alongside,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub rows: Vec<Row>,
    pub width: f32,
    pub documentation: Documentation,
    pub documentation_width: f32,
    pub documentation_height: f32,
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.candidates.len() > 1000
            || (!self.candidates.is_empty() && self.selected >= self.candidates.len())
            || self.viewport.iter().any(|v| !v.is_finite() || *v < 0.)
            || self
                .candidates
                .iter()
                .any(|c| c.title.len() > 4096 || c.detail.as_ref().is_some_and(|s| s.len() > 4096))
        {
            return Err("invalid completion frontend snapshot".into());
        }
        Ok(())
    }
}
impl View {
    pub fn validate(&self, snapshot: &Snapshot) -> Result<(), String> {
        if self
            .rows
            .windows(2)
            .any(|rows| rows[0].candidate >= rows[1].candidate)
            || self.rows.len() > 32
            || self.rows.is_empty() && !snapshot.candidates.is_empty()
            || !self.rows.is_empty() && !self.rows.iter().any(|r| r.candidate == snapshot.selected)
            || self.rows.iter().any(|r| {
                r.candidate >= snapshot.candidates.len()
                    || r.title.len() > 4096
                    || r.detail.as_ref().is_some_and(|s| s.len() > 4096)
            })
            || [
                self.width,
                self.documentation_width,
                self.documentation_height,
            ]
            .iter()
            .any(|v| !v.is_finite() || !(0. ..=2048.).contains(v))
            || self.width < 80.
            || self.documentation != Documentation::Hidden
                && (!snapshot.documentation
                    || self.documentation_width < 80.
                    || self.documentation_height < 40.)
        {
            return Err("invalid completion frontend view".into());
        }
        Ok(())
    }
}
pub trait Provider: std::fmt::Debug + Send + Sync {
    /// Bounded CPU work only. The host renders the returned controls and owns input.
    fn present(&self, snapshot: &Snapshot) -> Result<View, String>;
}
pub type Providers = std::collections::BTreeMap<String, Arc<dyn Provider>>;
/// Minimal host fallback when the selected service fails or retires.
#[derive(Debug)]
pub struct Fallback;
impl Provider for Fallback {
    fn present(&self, snapshot: &Snapshot) -> Result<View, String> {
        Ok(View {
            rows: snapshot
                .candidates
                .iter()
                .enumerate()
                .skip(snapshot.selected.saturating_sub(3))
                .take(6)
                .map(|(candidate, c)| Row {
                    candidate,
                    title: c.title.clone(),
                    detail: c.detail.clone(),
                })
                .collect(),
            width: 300.,
            documentation: Documentation::Hidden,
            documentation_width: 0.,
            documentation_height: 0.,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_indices_hidden_selection_and_unbounded_layouts() {
        let snapshot = Snapshot {
            candidates: vec![
                Candidate {
                    title: "a".into(),
                    detail: None
                };
                10
            ],
            selected: 8,
            viewport: [800., 600.],
            documentation: false,
            ready: true,
        };
        let view = Fallback.present(&snapshot).unwrap();
        view.validate(&snapshot).unwrap();
        let mut invalid = view.clone();
        invalid.rows[0].candidate = 100;
        assert!(invalid.validate(&snapshot).is_err());
        let mut invalid = view.clone();
        invalid.rows.retain(|r| r.candidate != 8);
        assert!(invalid.validate(&snapshot).is_err());
        let mut invalid = view.clone();
        invalid.rows.swap(0, 1);
        assert!(invalid.validate(&snapshot).is_err());
        let mut invalid = view.clone();
        invalid.width = f32::NAN;
        assert!(invalid.validate(&snapshot).is_err());
        let mut invalid = view;
        invalid.documentation = Documentation::Alongside;
        assert!(invalid.validate(&snapshot).is_err());
    }
}
