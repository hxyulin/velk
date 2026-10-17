//! Headless suggestion selection. Edits are usable only against their exact source.
use crate::Choice;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub revision: u64,
    pub position: usize,
}

pub struct Session {
    pub target: Target,
    pub items: Vec<Choice>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub prefix: String,
    pub start: usize,
    pub ready: bool,
}
impl Session {
    pub fn new(target: Target, prefix: String, items: Vec<Choice>) -> Self {
        let start = target.position.saturating_sub(prefix.chars().count());
        let mut session = Self {
            target,
            items,
            visible: Vec::new(),
            selected: 0,
            prefix,
            start,
            ready: true,
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
            self.filter();
        }
        true
    }
    pub fn resolve(&mut self, target: Target, items: Vec<Choice>) -> bool {
        if self.target != target {
            return false;
        }
        let selected = self.choice().map(|c| c.title.clone());
        self.items = items;
        self.ready = true;
        self.filter();
        if let Some(title) = selected {
            self.selected = self
                .visible
                .iter()
                .position(|i| self.items[*i].title == title)
                .unwrap_or(0);
        }
        true
    }
    fn filter(&mut self) {
        let prefix = self.prefix.to_lowercase();
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.title.to_lowercase().starts_with(&prefix))
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }
    pub fn next(&mut self, backwards: bool) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = if backwards {
            (self.selected + self.visible.len() - 1) % self.visible.len()
        } else {
            (self.selected + 1) % self.visible.len()
        };
    }
    pub fn choice(&self) -> Option<&Choice> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.items.get(*index))
    }
    pub fn accept(&self) -> Option<Choice> {
        self.ready.then(|| self.choice().cloned()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn choice(title: &str) -> Choice {
        Choice {
            title: title.into(),
            detail: None,
            action: crate::ChoiceAction::Semantic {
                payload: serde_json::Value::Null,
            },
        }
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
