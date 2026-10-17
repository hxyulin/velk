pub use velk_completion_matching::Matcher;
#[cfg(test)]
mod ranking_tests {
    use velk_plugin_sdk::commands::{Choice, completion::*};
    fn make_session(target: Target, prefix: String, items: Vec<Item>) -> Session {
        Session::with_policy(
            target,
            prefix,
            items,
            std::sync::Arc::new(super::DefaultPolicy),
        )
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
    fn exact_matches_beat_provider_order_and_arriving_exact_matches_replace_automatic_selection() {
        let target = Target {
            revision: 1,
            position: 5,
        };
        let session = make_session(target, "print".into(), vec![item("println"), item("print")]);
        assert_eq!(session.accept().unwrap().title, "print");
        let mut session = make_session(target, "print".into(), vec![item("println")]);
        session.resolve(target, vec![item("println"), item("print")]);
        assert_eq!(session.accept().unwrap().title, "print");
    }
    #[test]
    fn explicit_navigation_survives_late_better_matches_but_typing_recomputes_the_best_match() {
        let target = Target {
            revision: 1,
            position: 4,
        };
        let mut session = make_session(
            target,
            "prin".into(),
            vec![item("print"), item("printable")],
        );
        session.next(false);
        session.resolve(target, vec![item("prin"), item("print"), item("printable")]);
        assert_eq!(session.accept().unwrap().title, "printable");
        let current = Target {
            revision: 2,
            position: 5,
        };
        session.update(current, "print".into());
        assert_eq!(session.choice().unwrap().title, "print");
        assert!(session.accept().is_none());
        session.resolve(current, vec![item("printable"), item("print")]);
        assert_eq!(session.accept().unwrap().title, "print");
    }
    #[test]
    fn popup_accepts_subsequences_and_typos_using_filter_text() {
        let target = Target {
            revision: 1,
            position: 5,
        };
        let mut candidate = item("A display label");
        candidate.filter_text = "print".into();
        let session = make_session(target, "pritn".into(), vec![candidate]);
        assert_eq!(session.accept().unwrap().title, "A display label");
        let session = make_session(target, "prnt".into(), vec![item("print")]);
        assert_eq!(session.accept().unwrap().title, "print");
    }
}

#[derive(Debug, Default)]
pub struct DefaultPolicy;
impl velk_plugin_sdk::commands::completion::matching::Policy for DefaultPolicy {
    fn scores(&self, query: &str, candidates: &[&str]) -> Result<Vec<Option<[u64; 4]>>, String> {
        Ok(velk_completion_matching::scores(query, candidates))
    }
}
