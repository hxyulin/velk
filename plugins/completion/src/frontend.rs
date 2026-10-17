//! Bundled completion presentations, independent of sources and matching.
use velk_plugin_sdk::{Plugin, Registrar, commands::completion::frontend::*};
#[derive(Debug, Default)]
pub struct Popup;
impl Provider for Popup {
    fn present(&self, snapshot: &Snapshot) -> Result<View, String> {
        let alongside = snapshot.documentation && snapshot.viewport[0] >= 680.;
        let width = if alongside {
            300.
        } else {
            (snapshot.viewport[0] - 24.).clamp(120., 420.)
        };
        let mut view = Fallback.present(snapshot)?;
        view.width = width;
        view.documentation = if !snapshot.documentation {
            Documentation::Hidden
        } else if alongside {
            Documentation::Alongside
        } else {
            Documentation::Below
        };
        view.documentation_width = if alongside {
            (snapshot.viewport[0] - 24.).min(740.) - width
        } else {
            width
        };
        view.documentation_height = (snapshot.viewport[1] * 0.35).clamp(60., 220.);
        Ok(view)
    }
}
#[derive(Debug, Default)]
pub struct Compact;
impl Provider for Compact {
    fn present(&self, snapshot: &Snapshot) -> Result<View, String> {
        let mut view = Fallback.present(snapshot)?;
        view.rows = snapshot
            .candidates
            .iter()
            .enumerate()
            .skip(snapshot.selected.saturating_sub(4))
            .take(10)
            .map(|(candidate, c)| Row {
                candidate,
                title: c.title.clone(),
                detail: None,
            })
            .collect();
        view.width = (snapshot.viewport[0] - 24.).clamp(120., 340.);
        Ok(view)
    }
}
#[derive(Default)]
pub struct CompactFrontend;
impl Plugin for CompactFrontend {
    const ID: &'static str = "velk.completion.compact";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Compact completion frontend";
    fn register(registrar: &mut Registrar<Self>) -> Result<(), String> {
        registrar.completion_frontend::<Compact>()
    }
    fn describe(&mut self) -> String {
        "Compact, title-only completion menu without documentation.".into()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontends_keep_rank_and_selection_but_choose_distinct_layouts() {
        for viewport in [[900., 500.], [320., 500.]] {
            let snapshot = Snapshot {
                candidates: (0..20)
                    .map(|i| Candidate {
                        title: format!("item {i}"),
                        detail: Some("detail".into()),
                    })
                    .collect(),
                selected: 15,
                viewport,
                documentation: true,
                ready: true,
            };
            let popup = Popup.present(&snapshot).unwrap();
            popup.validate(&snapshot).unwrap();
            let compact = Compact.present(&snapshot).unwrap();
            compact.validate(&snapshot).unwrap();
            assert_eq!(compact.documentation, Documentation::Hidden);
            assert!(compact.rows.iter().all(|r| r.detail.is_none()));
            assert!(popup.rows.iter().all(|r| r.detail.is_some()));
            assert_eq!(
                popup.documentation,
                if viewport[0] >= 680. {
                    Documentation::Alongside
                } else {
                    Documentation::Below
                }
            );
        }
    }
}
