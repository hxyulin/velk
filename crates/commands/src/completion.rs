//! Headless suggestion selection. Edits are usable only against their exact source.
pub mod frontend;
pub mod matching;
use crate::Choice;
use serde::{Deserialize, Serialize};

/// Presentation metadata for a caret suggestion, separate from picker choices.
/// Providers own identity; reducers namespace it when combining sources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub filter_text: String,
    pub sort_text: String,
    pub choice: Choice,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commit_characters: Vec<char>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<Documentation>,
    /// Opaque semantic command that returns `Outcome::Document` for this item.
    /// Resolving documentation must not apply the item's edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_resolve: Option<serde_json::Value>,
}
impl From<Choice> for Item {
    fn from(choice: Choice) -> Self {
        Self {
            id: choice.title.clone(),
            filter_text: choice.title.clone(),
            sort_text: choice.title.clone(),
            choice,
            commit_characters: Vec::new(),
            documentation: None,
            documentation_resolve: None,
        }
    }
}
impl std::ops::Deref for Item {
    type Target = Choice;
    fn deref(&self) -> &Choice {
        &self.choice
    }
}
impl std::ops::DerefMut for Item {
    fn deref_mut(&mut self) -> &mut Choice {
        &mut self.choice
    }
}
impl Item {
    pub fn validate(&self) -> Result<(), String> {
        if self.commit_characters.len() > 32
            || self
                .commit_characters
                .iter()
                .any(|c| c.is_control() || c.is_whitespace() || c.is_alphanumeric() || *c == '_')
        {
            return Err("invalid completion commit characters".into());
        }
        for value in [&self.id, &self.filter_text, &self.sort_text] {
            if value.len() > 4096 || value.chars().any(char::is_control) {
                return Err("invalid completion metadata".into());
            }
        }
        if let Some(documentation) = &self.documentation {
            documentation.validate()?;
        }
        if let Some(payload) = &self.documentation_resolve
            && serde_json::to_vec(payload)
                .map_err(|e| e.to_string())?
                .len()
                > 16 * 1024
        {
            return Err("completion documentation payload exceeds 16 KiB".into());
        }
        if self.id.is_empty() {
            return Err("missing completion identity".into());
        }
        Ok(())
    }
}

/// Raw provider documentation. Markdown is parsed by the selected markup service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Documentation {
    pub text: String,
    pub markdown: bool,
}
impl Documentation {
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > 64 * 1024 || self.text.contains('\0') {
            return Err("invalid completion documentation".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentationTarget {
    pub target: Target,
    pub epoch: u64,
    pub id: String,
    pub source: Option<(String, u64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub revision: u64,
    pub position: usize,
}

pub struct Session {
    pub target: Target,
    pub items: Vec<Item>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub prefix: String,
    pub start: usize,
    pub ready: bool,
    selection_explicit: bool,
    policy: std::sync::Arc<dyn matching::Policy>,
}
impl Session {
    pub fn new(target: Target, prefix: String, items: Vec<Item>) -> Self {
        Self::with_policy(target, prefix, items, std::sync::Arc::new(matching::Prefix))
    }
    pub fn with_policy(
        target: Target,
        prefix: String,
        items: Vec<Item>,
        policy: std::sync::Arc<dyn matching::Policy>,
    ) -> Self {
        let start = target.position.saturating_sub(prefix.chars().count());
        let mut session = Self {
            target,
            items,
            visible: Vec::new(),
            selected: 0,
            prefix,
            start,
            ready: true,
            selection_explicit: false,
            policy,
        };
        session.filter();
        session
    }
    pub fn update(&mut self, target: Target, prefix: String) -> bool {
        if target.position.saturating_sub(prefix.chars().count()) != self.start
            || target.revision == self.target.revision && target.position != self.target.position
        {
            return false;
        }
        if target != self.target {
            self.target = target;
            self.prefix = prefix;
            self.ready = false;
            self.selection_explicit = false;
            self.filter();
        }
        true
    }
    pub fn resolve(&mut self, target: Target, items: Vec<Item>) -> bool {
        if self.target != target {
            return false;
        }
        let selected = self.choice().map(identity);
        self.items = items;
        self.ready = true;
        self.filter();
        if let Some(selected) = selected {
            self.selected = self
                .visible
                .iter()
                .position(|i| identity(&self.items[*i]) == selected)
                .filter(|index| self.selection_explicit || *index == 0)
                .unwrap_or_else(|| {
                    self.selection_explicit = false;
                    0
                });
        }
        true
    }
    fn filter(&mut self) {
        let candidates: Vec<_> = self
            .items
            .iter()
            .map(|item| item.filter_text.as_str())
            .collect();
        let scores = self
            .policy
            .scores(&self.prefix, &candidates)
            .ok()
            .filter(|scores| scores.len() == candidates.len())
            .unwrap_or_else(|| {
                matching::Policy::scores(&matching::Prefix, &self.prefix, &candidates)
                    .expect("prefix policy")
            });
        let mut ranked: Vec<_> = scores
            .iter()
            .enumerate()
            .filter_map(|(index, score)| score.map(|score| (score, index)))
            .collect();
        ranked.sort_by_key(|(score, _)| *score);
        self.visible = ranked.into_iter().map(|(_, index)| index).collect();
        self.selected = 0;
    }

    pub fn next(&mut self, backwards: bool) {
        if self.visible.is_empty() {
            return;
        }
        self.selection_explicit = true;
        self.selected = if backwards {
            (self.selected + self.visible.len() - 1) % self.visible.len()
        } else {
            (self.selected + 1) % self.visible.len()
        };
    }
    pub fn choice(&self) -> Option<&Item> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.items.get(*index))
    }
    pub fn accept(&self) -> Option<Item> {
        self.ready.then(|| self.choice().cloned()).flatten()
    }
}

fn identity(choice: &Item) -> (String, Option<(String, u64)>) {
    let source = match &choice.action {
        crate::ChoiceAction::Contributed {
            plugin, generation, ..
        } => Some((plugin.clone(), *generation)),
        _ => None,
    };
    (choice.id.clone(), source)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn choice(title: &str) -> Item {
        Choice {
            title: title.into(),
            detail: None,
            action: crate::ChoiceAction::Semantic {
                payload: serde_json::Value::Null,
            },
        }
        .into()
    }
    #[test]
    fn typing_filters_but_requires_fresh_edits_and_rejects_old_responses() {
        let original = Target {
            revision: 1,
            position: 3,
        };
        let mut session = Session::new(
            original,
            "pr".into(),
            vec![choice("print"), choice("process")],
        );
        session.next(false);
        assert_eq!(session.accept().unwrap().title, "process");
        let current = Target {
            revision: 2,
            position: 4,
        };
        assert!(session.update(current, "pri".into()));
        assert_eq!(session.visible.len(), 1);
        assert!(session.accept().is_none());
        assert!(!session.resolve(original, vec![choice("previous")]));
        assert!(session.resolve(current, vec![choice("print")]));
        assert_eq!(session.accept().unwrap().title, "print");
        assert!(!session.update(
            Target {
                position: 2,
                ..current
            },
            "p".into()
        ));
    }
    #[test]
    fn navigation_wraps_and_empty_results_are_safe() {
        let mut session = Session::new(
            Target {
                revision: 0,
                position: 0,
            },
            String::new(),
            vec![choice("a"), choice("b")],
        );
        session.next(true);
        assert_eq!(session.accept().unwrap().title, "b");
        session.next(false);
        assert_eq!(session.accept().unwrap().title, "a");
        session.resolve(session.target, vec![]);
        session.next(true);
        assert!(session.accept().is_none());
    }
}

#[cfg(test)]
mod contribution_tests {
    use super::*;
    fn item(plugin: &str) -> Item {
        Choice {
            title: "print".into(),
            detail: None,
            action: crate::ChoiceAction::Contributed {
                plugin: plugin.into(),
                generation: 1,
                action: Box::new(crate::ChoiceAction::Semantic {
                    payload: serde_json::Value::Null,
                }),
            },
        }
        .into()
    }
    #[test]
    fn partial_results_preserve_selection_between_equal_labels_from_different_sources() {
        let target = Target {
            revision: 1,
            position: 1,
        };
        let mut session = Session::new(target, "p".into(), vec![item("test.lsp")]);
        session.next(false);
        session.resolve(target, vec![item("test.words"), item("test.lsp")]);
        assert_eq!(session.selected, 1);
        assert_eq!(session.accept().unwrap(), item("test.lsp"));
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    fn item(id: &str, label: &str, filter: &str) -> Item {
        Item {
            commit_characters: Vec::new(),
            documentation: None,
            documentation_resolve: None,
            id: id.into(),
            filter_text: filter.into(),
            sort_text: id.into(),
            choice: Choice {
                title: label.into(),
                detail: None,
                action: crate::ChoiceAction::Semantic {
                    payload: serde_json::Value::Null,
                },
            },
        }
    }
    #[test]
    fn filtering_uses_provider_text_and_refresh_tracks_identity_despite_label_changes() {
        let target = Target {
            revision: 1,
            position: 2,
        };
        let mut session = Session::new(
            target,
            "pr".into(),
            vec![
                item("one", "Display label", "print"),
                item("two", "Display label", "process"),
            ],
        );
        assert_eq!(session.visible.len(), 2);
        session.next(false);
        assert_eq!(session.accept().unwrap().id, "two");
        session.resolve(
            target,
            vec![
                item("two", "Updated label", "process"),
                item("one", "Display one", "print"),
            ],
        );
        assert_eq!(session.selected, 0);
        assert_eq!(session.accept().unwrap().title, "Updated label");
        session.update(
            Target {
                revision: 2,
                position: 3,
            },
            "pri".into(),
        );
        assert_eq!(session.visible.len(), 1);
        assert_eq!(session.choice().unwrap().id, "one");
        assert!(session.accept().is_none());
    }
    #[test]
    fn malformed_metadata_is_rejected_before_presentation() {
        let mut candidate = item("one", "label", "filter");
        candidate.id.clear();
        assert!(candidate.validate().is_err());
        candidate.id = "one".into();
        candidate.sort_text = "x\n".into();
        assert!(candidate.validate().is_err());
        candidate.sort_text = "x".into();
        candidate.filter_text = "x".repeat(4097);
        assert!(candidate.validate().is_err());
        candidate.filter_text = "x".into();
        candidate.documentation = Some(Documentation {
            text: "x".repeat(64 * 1024 + 1),
            markdown: true,
        });
        assert!(candidate.validate().is_err());
        candidate.documentation = None;
        candidate.documentation_resolve = Some(serde_json::json!({"data":"x".repeat(16 * 1024)}));
        assert!(candidate.validate().is_err());
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    #[derive(Debug)]
    struct Suffix;
    impl matching::Policy for Suffix {
        fn scores(
            &self,
            query: &str,
            candidates: &[&str],
        ) -> Result<Vec<Option<matching::Score>>, String> {
            Ok(candidates
                .iter()
                .map(|candidate| {
                    candidate
                        .ends_with(query)
                        .then_some([candidate.len() as u64, 0, 0, 0])
                })
                .collect())
        }
    }
    #[derive(Debug)]
    struct Broken;
    impl matching::Policy for Broken {
        fn scores(&self, _: &str, _: &[&str]) -> Result<Vec<Option<matching::Score>>, String> {
            Ok(vec![])
        }
    }
    fn item(title: &str) -> Item {
        Choice {
            title: title.into(),
            detail: None,
            action: crate::ChoiceAction::Semantic {
                payload: Default::default(),
            },
        }
        .into()
    }
    #[test]
    fn custom_policy_controls_filtering_and_ranking_and_invalid_responses_fall_back() {
        let target = Target {
            revision: 1,
            position: 3,
        };
        let session = Session::with_policy(
            target,
            "ing".into(),
            vec![item("long ending"), item("ring"), item("ignore")],
            std::sync::Arc::new(Suffix),
        );
        assert_eq!(session.visible, vec![1, 0]);
        assert_eq!(session.accept().unwrap().title, "ring");
        let session = Session::with_policy(
            target,
            "ing".into(),
            vec![item("ring"), item("ingredient")],
            std::sync::Arc::new(Broken),
        );
        assert_eq!(session.visible, vec![1]);
    }
}

#[cfg(test)]
mod arrival_order_tests {
    use super::*;
    #[test]
    fn late_preferred_equal_matches_replace_automatic_but_not_explicit_selection() {
        let item = |id: &str| Item {
            id: id.into(),
            ..crate::Choice {
                title: "print".into(),
                detail: None,
                action: crate::ChoiceAction::Semantic {
                    payload: Default::default(),
                },
            }
            .into()
        };
        let target = Target {
            revision: 1,
            position: 2,
        };
        let mut session = Session::new(target, "pr".into(), vec![item("fallback")]);
        session.resolve(target, vec![item("preferred"), item("fallback")]);
        assert_eq!(session.choice().unwrap().id, "preferred");
        session.next(false);
        session.resolve(target, vec![item("preferred"), item("fallback")]);
        assert_eq!(session.choice().unwrap().id, "fallback");
    }
}
