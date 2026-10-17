//! Optional command contributors. Discovery never activates or installs a plugin.
use crate::{Context, Declaration, Handler, Outcome};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    pub topic: String,
    pub command: Declaration,
}
impl Contribution {
    pub fn validate(&self) -> Result<(), String> {
        self.command.validate()?;
        if !super::valid_id(&self.topic)
            || self.command.input.is_some()
            || self.command.shortcut.is_some()
            || !matches!(
                self.command.handler,
                Handler::Request { .. } | Handler::Semantic { .. }
            )
        {
            return Err("invalid command contribution".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionResult {
    pub plugin: String,
    pub generation: u64,
    pub command: String,
    pub outcome: std::result::Result<Outcome, String>,
}
/// A reducer receives a new snapshot after each contributor finishes. Errors are
/// isolated to that contributor; an empty result set is a valid initial snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    pub context: Context,
    pub results: Vec<ContributionResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contributors_cannot_prompt_or_recursively_collect() {
        let mut contribution = Contribution {
            topic: "test.topic".into(),
            command: Declaration {
                id: "test.source".into(),
                title: "Source".into(),
                language: None,
                input: None,
                shortcut: None,
                handler: Handler::Semantic {
                    payload: serde_json::Value::Null,
                },
            },
        };
        contribution.validate().unwrap();
        contribution.command.input = Some("Prompt".into());
        assert!(contribution.validate().is_err());
        contribution.command.input = None;
        contribution.command.handler = Handler::Collect {
            topic: "test.topic".into(),
            hook: "test.reducer".into(),
        };
        assert!(contribution.validate().is_err());
    }
}

impl Collection {
    /// Share the native request budget across sources so a large batch cannot
    /// crowd out other sources or turn the whole collection into a transport error.
    pub fn encode_bounded(mut self, limit: usize) -> std::result::Result<Vec<u8>, String> {
        let encode = |value: &Self| serde_json::to_vec(value).map_err(|e| e.to_string());
        let mut bytes = encode(&self)?;
        if bytes.len() <= limit {
            return Ok(bytes);
        }
        let results = std::mem::take(&mut self.results);
        let overhead = encode(&self)?.len();
        let budget = limit.saturating_sub(overhead + results.len() * 2) / results.len().max(1);
        for mut result in results {
            if let Ok(Outcome::Completions { items }) = &mut result.outcome {
                let candidates = std::mem::take(items);
                let mut low = 0;
                let mut high = candidates.len();
                while low < high {
                    let count = low + (high - low).div_ceil(2);
                    *items = candidates[..count].to_vec();
                    let size = serde_json::to_vec(&*items)
                        .map_err(|e| e.to_string())?
                        .len();
                    if size + result.plugin.len() + result.command.len() + 256 <= budget {
                        low = count;
                    } else {
                        high = count - 1;
                    }
                }
                *items = candidates[..low].to_vec();
            }
            if serde_json::to_vec(&result)
                .map_err(|e| e.to_string())?
                .len()
                > budget
            {
                result.outcome = Err("contributor exceeded the collection request budget".into());
            }
            self.results.push(result);
        }
        bytes = encode(&self)?;
        if bytes.len() > limit {
            return Err("collection metadata exceeds the request budget".into());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn large_batches_fit_the_wire_budget_and_retain_every_source() {
        let result = |plugin: &str| ContributionResult {
            plugin: plugin.into(),
            generation: 1,
            command: "test.complete".into(),
            outcome: Ok(Outcome::Completions {
                items: (0..100)
                    .map(|index| crate::Choice {
                        title: format!("candidate{index}"),
                        detail: Some("x".repeat(500)),
                        action: crate::ChoiceAction::Semantic {
                            payload: serde_json::Value::Null,
                        },
                    })
                    .collect(),
            }),
        };
        let bytes = Collection {
            context: Context {
                input: None,
                path: None,
                language: None,
                revision: Some(1),
                position: Some(1),
            },
            results: vec![result("test.first"), result("test.second")],
        }
        .encode_bounded(4096)
        .unwrap();
        assert!(bytes.len() <= 4096);
        let collection: Collection = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(collection.results.len(), 2);
        for result in collection.results {
            let Outcome::Completions { items } = result.outcome.unwrap() else {
                panic!()
            };
            assert!(!items.is_empty());
            assert!(items.len() < 100);
        }
    }
}
