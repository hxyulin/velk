//! Merge policy stays in the feature plugin; the host authenticates source identity.
use crate::*;
use std::collections::BTreeSet;
#[derive(Clone)]
pub struct Policy {
    pub source_order: Vec<String>,
    pub deduplicate: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            source_order: vec!["velk.lsp".into(), "velk.completion.words".into()],
            deduplicate: true,
        }
    }
}
impl Policy {
    pub fn parse_order(value: &str) -> Result<Vec<String>, String> {
        let order: Vec<_> = value
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(String::from)
            .collect();
        let unique: BTreeSet<_> = order.iter().collect();
        if order.len() > 32
            || unique.len() != order.len()
            || order
                .iter()
                .any(|id| velk_plugin_sdk::contracts::parse_plugin_id(id).is_none())
        {
            return Err(
                "source_order must contain up to 32 distinct comma-separated plugin IDs".into(),
            );
        }
        Ok(order)
    }
    pub fn reduce(&self, mut input: Collection) -> Result<Outcome, String> {
        input.results.sort_by_key(|result| {
            self.source_order
                .iter()
                .position(|id| id == &result.plugin)
                .unwrap_or(self.source_order.len())
        });
        let mut source_order = std::collections::BTreeMap::new();
        let mut candidates = Vec::new();
        for result in &input.results {
            let next = source_order.len();
            let order = *source_order.entry(&result.plugin).or_insert(next);
            if let Ok(Outcome::Completions { items, .. }) = &result.outcome {
                candidates.extend(items.iter().map(|candidate| (order, result, candidate)));
            }
        }
        candidates.sort_by(|(left_order, _, left), (right_order, _, right)| {
            left_order
                .cmp(right_order)
                .then_with(|| left.sort_text.cmp(&right.sort_text))
        });
        let mut items = Vec::new();
        let mut seen = std::collections::BTreeMap::new();
        for (_, result, candidate) in candidates {
            candidate.validate()?;
            if self.deduplicate
                && let Some(key) = equivalence(candidate)?
            {
                if seen
                    .get(&key)
                    .is_some_and(|source| source != &result.plugin)
                {
                    continue;
                }
                seen.entry(key).or_insert_with(|| result.plugin.clone());
            }
            let mut candidate = candidate.clone();
            candidate.choice.action = ChoiceAction::Contributed {
                plugin: result.plugin.clone(),
                generation: result.generation,
                action: Box::new(candidate.choice.action),
            };
            candidate.choice.detail = Some(match candidate.choice.detail.take() {
                Some(detail) => format!("{}: {detail}", result.plugin),
                None => result.plugin.clone(),
            });
            candidate.id = format!("{}:{}", result.command, candidate.id);
            items.push(candidate);
            if items.len() == 1000 {
                break;
            }
        }
        let outcome = Outcome::Completions {
            items,
            continuation: None,
        };
        outcome.validate()?;
        Ok(outcome)
    }
}
fn equivalence(item: &velk_completion_contracts::Item) -> Result<Option<String>, String> {
    let action = match &item.action {
        ChoiceAction::Edit { edit } => {
            let mut edit = edit.clone();
            edit.documents.sort_by(|a, b| a.path.cmp(&b.path));
            for document in &mut edit.documents {
                document.edits.sort_by_key(|edit| {
                    (
                        edit.start.line,
                        edit.start.column,
                        edit.end.line,
                        edit.end.column,
                    )
                });
            }
            ChoiceAction::Edit { edit }
        }
        ChoiceAction::BufferEdit { .. } | ChoiceAction::Snippet { .. } => item.action.clone(),
        // A resolve token can hide imports or other edits. Labels alone do not
        // establish equivalence, and resolving every candidate would add latency.
        _ => return Ok(None),
    };
    serde_json::to_string(&(
        &item.title,
        &item.filter_text,
        &item.choice.detail,
        &item.commit_characters,
        action,
    ))
    .map(Some)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use velk_plugin_sdk::commands::contributions::ContributionResult;
    fn context() -> Context {
        Context {
            input: None,
            path: Some("/test.rs".into()),
            language: Some("rust".into()),
            revision: Some(1),
            position: Some(2),
        }
    }
    fn candidate(text: &str) -> velk_completion_contracts::Item {
        Choice {
            title: "print".into(),
            detail: None,
            action: ChoiceAction::Edit {
                edit: WorkspaceEdit {
                    documents: vec![DocumentEdit {
                        path: "/test.rs".into(),
                        revision: Some(1),
                        edits: vec![TextEdit {
                            start: Position {
                                line: 0,
                                column: 0,
                                encoding: Encoding::Utf8,
                            },
                            end: Position {
                                line: 0,
                                column: 2,
                                encoding: Encoding::Utf8,
                            },
                            text: text.into(),
                        }],
                    }],
                },
            },
        }
        .into()
    }
    fn source(plugin: &str, items: Vec<velk_completion_contracts::Item>) -> ContributionResult {
        ContributionResult {
            plugin: plugin.into(),
            generation: 1,
            command: format!("{plugin}.complete"),
            outcome: Ok(Outcome::Completions {
                items,
                continuation: None,
            }),
        }
    }
    fn merge(
        policy: &Policy,
        results: Vec<ContributionResult>,
    ) -> Vec<velk_completion_contracts::Item> {
        let Outcome::Completions { items, .. } = policy
            .reduce(Collection {
                context: context(),
                results,
            })
            .unwrap()
        else {
            panic!()
        };
        items
    }
    #[test]
    fn priority_precedes_foreign_sort_keys_and_can_be_reordered_or_disabled() {
        let mut preferred = candidate("print");
        preferred.sort_text = "z".into();
        let mut fallback = candidate("println");
        fallback.sort_text = "a".into();
        let results = vec![
            source("velk.completion.words", vec![fallback]),
            source("velk.lsp", vec![preferred]),
        ];
        let items = merge(&Policy::default(), results.clone());
        assert!(
            matches!(&items[0].action, ChoiceAction::Contributed { plugin, .. } if plugin == "velk.lsp")
        );
        let policy = Policy {
            source_order: vec!["velk.completion.words".into()],
            deduplicate: true,
        };
        assert!(
            matches!(&merge(&policy, results.clone())[0].action, ChoiceAction::Contributed { plugin, .. } if plugin == "velk.completion.words")
        );
        let policy = Policy {
            source_order: vec![],
            deduplicate: false,
        };
        assert_eq!(merge(&policy, results).len(), 2);
        for invalid in ["bad id", "velk.lsp,velk.lsp", "unscoped"] {
            assert!(Policy::parse_order(invalid).is_err());
        }
        assert_eq!(
            Policy::parse_order(" velk.lsp, community.words ").unwrap(),
            vec!["velk.lsp", "community.words"]
        );
    }
    #[test]
    fn duplicates_keep_the_preferred_source_but_distinct_edits_imports_and_overloads_survive() {
        let base = candidate("print");
        let mut with_commit = base.clone();
        with_commit.commit_characters = vec!['('];
        assert_eq!(
            merge(
                &Policy::default(),
                vec![
                    source("velk.lsp", vec![base.clone()]),
                    source("velk.completion.words", vec![with_commit])
                ]
            )
            .len(),
            2
        );
        let mut imported = base.clone();
        if let ChoiceAction::Edit { edit } = &mut imported.action {
            edit.documents[0].edits.push(TextEdit {
                start: Position {
                    line: 1,
                    column: 0,
                    encoding: Encoding::Utf8,
                },
                end: Position {
                    line: 1,
                    column: 0,
                    encoding: Encoding::Utf8,
                },
                text: "use dep;\n".into(),
            });
        }
        let mut overload = base.clone();
        overload.choice.detail = Some("fn(value: &str)".into());
        let mut unresolved = base.clone();
        unresolved.action = ChoiceAction::Semantic {
            payload: serde_json::json!({"resolve":"unknown imports"}),
        };
        let items = merge(
            &Policy::default(),
            vec![
                source(
                    "velk.completion.words",
                    vec![
                        base.clone(),
                        candidate("println"),
                        imported,
                        overload,
                        unresolved.clone(),
                    ],
                ),
                source("velk.lsp", vec![base.clone(), base.clone(), unresolved]),
            ],
        );
        assert_eq!(items.len(), 7);
        assert!(
            matches!(&items[0].action, ChoiceAction::Contributed { plugin, .. } if plugin == "velk.lsp")
        );
        let policy = Policy {
            deduplicate: false,
            ..Default::default()
        };
        assert_eq!(
            merge(
                &policy,
                vec![
                    source("velk.lsp", vec![base.clone()]),
                    source("velk.completion.words", vec![base])
                ]
            )
            .len(),
            2
        );
    }
    #[test]
    fn higher_priority_candidates_get_the_budget_before_lower_priority_sources() {
        let low = (0..1000)
            .map(|index| {
                let mut item = candidate("low");
                item.id = index.to_string();
                item
            })
            .collect();
        let items = merge(
            &Policy::default(),
            vec![
                source("velk.completion.words", low),
                source("velk.lsp", vec![candidate("preferred")]),
            ],
        );
        assert_eq!(items.len(), 1000);
        assert!(
            matches!(&items[0].action, ChoiceAction::Contributed { plugin, .. } if plugin == "velk.lsp")
        );
    }
    #[test]
    fn file_words_and_concrete_lsp_edits_share_unicode_coordinates_without_losing_undo_targets() {
        let mut context = context();
        context.position = Some(5);
        let Outcome::Completions { items: words, .. } = words(&context, 42, "世界 \n世").unwrap()
        else {
            panic!()
        };
        assert_eq!(words.len(), 1);
        let ChoiceAction::Edit { edit } = &words[0].action else {
            panic!()
        };
        assert_eq!(edit.documents[0].revision, Some(1));
        assert_eq!(edit.documents[0].edits[0].end.column, 3);
        let items = merge(
            &Policy::default(),
            vec![
                source("velk.completion.words", words.clone()),
                source("velk.lsp", words),
            ],
        );
        assert_eq!(items.len(), 1);
    }
}
