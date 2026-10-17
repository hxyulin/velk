//! Default completion policy and an independent document-word contributor.
pub mod frontend;
pub mod matching;
mod merge;
use velk_completion_contracts::{Collection, Contribution, TOPIC};
use velk_plugin_sdk::{Host, Plugin, Registrar, commands::*, hooks, host};

pub struct Completion {
    automatic: bool,
    typing: bool,
    merge: merge::Policy,
}
impl Default for Completion {
    fn default() -> Self {
        Self {
            automatic: true,
            typing: true,
            merge: Default::default(),
        }
    }
}
impl Plugin for Completion {
    const ID: &'static str = "velk.completion";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Completion";
    const README: &'static str = include_str!("../README.md");
    fn commands() -> Vec<Declaration> {
        vec![Declaration {
            id: velk_completion_contracts::COMMAND.into(),
            title: "Complete at caret".into(),
            language: None,
            input: None,
            shortcut: Some(Shortcut {
                key: "Space".into(),
                control: true,
                ..Default::default()
            }),
            handler: Handler::Collect {
                automatic: Some(velk_completion_contracts::AUTOMATIC.into()),
                topic: TOPIC.into(),
                hook: velk_completion_contracts::REDUCE.into(),
            },
        }]
    }
    fn settings() -> Vec<velk_plugin_sdk::settings::Definition> {
        use velk_plugin_sdk::settings::{Definition, Kind};
        vec![
            Definition { key: "source_order".into(), title: "Completion source order".into(), description: "Comma-separated plugin IDs, highest priority first. Unlisted sources follow in declaration order. Priority breaks equal text-match scores.".into(), kind: Kind::Text { max_length: 4096 }, default: "velk.lsp, velk.completion.words".into() },
            Definition { key: "deduplicate".into(), title: "Merge equivalent suggestions".into(), description: "Keep the highest-priority copy of concrete edits shared by different sources. Different edits, overload details, and unresolved actions stay separate.".into(), kind: Kind::Boolean, default: true.into() },
            Definition { key: "automatic".into(), title: "Automatic completion".into(), description: "Request suggestions after typing and provider trigger characters.".into(), kind: Kind::Boolean, default: true.into() },
            Definition { key: "typing".into(), title: "Complete while typing words".into(), description: "Start suggestions after two identifier characters; trigger characters still work when disabled.".into(), kind: Kind::Boolean, default: true.into() },
        ]
    }
    fn configure(&mut self, values: &velk_plugin_sdk::settings::Values) -> Result<(), String> {
        self.automatic = values["automatic"]
            .as_bool()
            .ok_or("invalid automatic setting")?;
        self.typing = values["typing"].as_bool().ok_or("invalid typing setting")?;
        self.merge.source_order = merge::Policy::parse_order(
            values["source_order"]
                .as_str()
                .ok_or("invalid source order")?,
        )?;
        self.merge.deduplicate = values["deduplicate"]
            .as_bool()
            .ok_or("invalid deduplication setting")?;
        Ok(())
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.matching::<matching::DefaultPolicy>()?;
        reg.completion_frontend::<frontend::Popup>()?;
        reg.provide(
            hooks::Request(velk_completion_contracts::AUTOMATIC),
            |this: &mut Self, bytes: &[u8]| {
                let input: contributions::Activation =
                    serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                let reason = this.activate(&input);
                let input = reason
                    .map(|reason| serde_json::to_string(&reason))
                    .transpose()
                    .map_err(|e| e.to_string())?;
                serde_json::to_vec(&input).map_err(|e| e.to_string())
            },
        )?;
        reg.provide(
            hooks::Request(velk_completion_contracts::REDUCE),
            |this: &mut Self, bytes: &[u8]| {
                let input: Collection = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                serde_json::to_vec(&this.merge.reduce(input)?).map_err(|e| e.to_string())
            },
        )
    }
    fn describe(&mut self) -> String {
        "Combine optional completion sources".into()
    }
}

impl Completion {
    fn activate(
        &self,
        input: &contributions::Activation,
    ) -> Option<velk_completion_contracts::Invocation> {
        if !self.automatic || input.text.chars().count() != 1 {
            return None;
        }
        let ch = input.text.chars().next()?;
        let triggers: Vec<velk_completion_contracts::Triggers> = input
            .contributions
            .iter()
            .filter_map(|source| serde_json::from_value(source.metadata.clone()).ok())
            .collect();
        if ch == '_' || ch.is_alphanumeric() {
            (self.typing
                && input.prefix.chars().count() >= 2
                && triggers.iter().any(|source| source.typing))
            .then_some(velk_completion_contracts::Invocation::Typing)
        } else if !ch.is_whitespace()
            && !ch.is_control()
            && triggers
                .iter()
                .any(|source| source.dynamic_characters || source.characters.contains(&input.text))
        {
            // Dynamic providers check their advertised characters before doing work.
            Some(velk_completion_contracts::Invocation::Character {
                character: input.text.clone(),
            })
        } else {
            None
        }
    }
}

pub fn reduce(input: Collection) -> Result<Outcome, String> {
    merge::Policy::default().reduce(input)
}

#[derive(Default)]
pub struct Words {
    host: Option<Host>,
}
impl Plugin for Words {
    const ID: &'static str = "velk.completion.words";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Document Words";
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [velk_plugin_sdk::CapabilityDeclaration] = &[
        velk_plugin_sdk::CapabilityDeclaration::required(
            host::BUFFER_READ,
            "Suggest words from the current document.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::optional(
            host::BUFFER_WRITE,
            "Propose completion edits for the host to validate.",
        ),
    ];
    fn attach_host(&mut self, host: Host) {
        self.host = Some(host);
    }
    fn contributions() -> Vec<Contribution> {
        vec![Contribution {
            metadata: velk_completion_contracts::Triggers {
                typing: true,
                ..Default::default()
            }
            .metadata(),
            topic: TOPIC.into(),
            command: Declaration {
                id: "velk.completion.words".into(),
                title: "Document words".into(),
                language: None,
                input: None,
                shortcut: None,
                handler: Handler::Request {
                    hook: "velk.completion.words".into(),
                },
            },
        }]
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::Request("velk.completion.words"),
            |this: &mut Self, bytes: &[u8]| {
                let context: Context = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                serde_json::to_vec(&this.complete(context)?).map_err(|e| e.to_string())
            },
        )
    }
    fn describe(&mut self) -> String {
        "Complete words without a language server".into()
    }
}
impl Words {
    fn complete(&self, context: Context) -> Result<Outcome, String> {
        let host = self.host.as_ref().ok_or("host unavailable")?;
        let host::Response::Buffers { buffers } = host.request(host::Request::Buffers)? else {
            return Err("invalid buffer list".into());
        };
        let buffer = buffers
            .into_iter()
            .find(|b| b.active && b.path == context.path && Some(b.revision) == context.revision)
            .ok_or("completion source changed")?;
        let host::Response::Buffer { buffer, text } =
            host.request(host::Request::ReadBuffer { id: buffer.id })?
        else {
            return Err("invalid buffer snapshot".into());
        };
        if Some(buffer.revision) != context.revision {
            return Err("completion source changed".into());
        }
        words(&context, buffer.id, &text)
    }
}

fn words(context: &Context, buffer: u64, text: &str) -> Result<Outcome, String> {
    if matches!(
        velk_completion_contracts::Invocation::decode(context.input.as_deref())?,
        Some(velk_completion_contracts::Invocation::Character { .. })
    ) {
        return Ok(Outcome::Completions {
            items: vec![],
            continuation: None,
        });
    }
    let position = context.position.ok_or("missing completion position")?;
    let revision = context.revision.ok_or("missing completion revision")?;
    let chars: Vec<_> = text.chars().collect();
    if position > chars.len() {
        return Err("completion position is out of range".into());
    }
    let is_word = |c: char| c == '_' || c.is_alphanumeric();
    let mut start = position;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let prefix: String = chars[start..position].iter().collect();
    let before: String = chars[..start].iter().collect();
    let line = before.chars().filter(|&c| c == '\n').count() as u32;
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count() as u32;
    let begin = Position {
        line,
        column,
        encoding: Encoding::Utf32,
    };
    let end = Position {
        column: column + (position - start) as u32,
        ..begin
    };
    let caret_byte = text
        .char_indices()
        .nth(position)
        .map_or(text.len(), |(offset, _)| offset);
    let mut matcher = matching::Matcher::new(&prefix);
    let mut seen = std::collections::BTreeSet::new();
    let mut ranked = std::collections::BinaryHeap::new();
    let mut offset = 0;
    for part in text.split_inclusive(|c| !is_word(c)) {
        let word = part.trim_end_matches(|c| !is_word(c));
        let begin = offset;
        offset += part.len();
        if word.is_empty()
            || word.len() > 1024
            || (begin <= caret_byte && caret_byte <= begin + word.len())
            || !seen.insert(word)
        {
            continue;
        }
        if let Some(score) = matcher.score(word) {
            ranked.push((score, word));
            if ranked.len() > 200 {
                ranked.pop();
            }
        }
    }
    let mut items = Vec::new();
    for (_, word) in ranked.into_sorted_vec() {
        items.push(
            Choice {
                title: word.into(),
                detail: None,
                action: if let Some(path) = &context.path {
                    let start = Position {
                        line,
                        column: before.rsplit('\n').next().unwrap_or_default().len() as u32,
                        encoding: Encoding::Utf8,
                    };
                    let end = Position {
                        column: start.column + prefix.len() as u32,
                        ..start
                    };
                    ChoiceAction::Edit {
                        edit: WorkspaceEdit {
                            documents: vec![DocumentEdit {
                                path: path.clone(),
                                revision: Some(revision),
                                edits: vec![TextEdit {
                                    start,
                                    end,
                                    text: word.into(),
                                }],
                            }],
                        },
                    }
                } else {
                    ChoiceAction::BufferEdit {
                        id: buffer,
                        revision,
                        edits: vec![TextEdit {
                            start: begin,
                            end,
                            text: word.into(),
                        }],
                    }
                },
            }
            .into(),
        );
    }
    Ok(Outcome::Completions {
        items,
        continuation: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(position: usize) -> Context {
        Context {
            input: None,
            path: None,
            language: None,
            revision: Some(7),
            position: Some(position),
        }
    }
    #[test]
    fn words_support_untitled_unicode_and_replace_only_the_prefix() {
        let input = "界面 世界 世界\n世";
        let Outcome::Completions { items, .. } =
            words(&context(input.chars().count()), 42, input).unwrap()
        else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "世界");
        let ChoiceAction::BufferEdit {
            id,
            revision,
            edits,
        } = &items[0].action
        else {
            panic!()
        };
        assert_eq!((*id, *revision), (42, 7));
        assert_eq!(edits[0].start.line, 1);
        assert_eq!(edits[0].start.column, 0);
        assert_eq!(edits[0].end.column, 1);
        assert_eq!(edits[0].text, "世界");
        assert!(words(&context(999), 42, input).is_err());
    }
    #[test]
    fn reducer_is_optional_preserves_sources_and_isolates_errors() {
        use velk_plugin_sdk::commands::contributions::ContributionResult;
        let source = |plugin: &str, generation, outcome| ContributionResult {
            plugin: plugin.into(),
            generation,
            command: "test.complete".into(),
            outcome,
        };
        let candidates = words(&context(16), 42, "print process pr").unwrap();
        let merged = merge::Policy {
            deduplicate: false,
            ..Default::default()
        }
        .reduce(Collection {
            context: context(16),
            results: vec![
                source("test.failed", 1, Err("not ready".into())),
                source("test.words", 2, Ok(candidates.clone())),
                source("test.other", 3, Ok(candidates)),
            ],
        })
        .unwrap();
        let Outcome::Completions { items, .. } = merged else {
            panic!()
        };
        assert!(!items.is_empty());
        assert!(
            matches!(&items[0].action, ChoiceAction::Contributed { plugin, generation: 2, .. } if plugin == "test.words")
        );
        assert!(
            matches!(&items.last().unwrap().action, ChoiceAction::Contributed { plugin, generation: 3, .. } if plugin == "test.other")
        );
        assert!(
            matches!(reduce(Collection { context: context(0), results: vec![] }).unwrap(), Outcome::Completions { items, ..} if items.is_empty())
        );
    }
    #[test]
    fn contributor_reads_the_host_snapshot_and_rejects_changed_revisions() {
        use velk_plugin_sdk::test_support::MockHost;
        let buffer = host::Buffer {
            id: 42,
            title: "Untitled".into(),
            path: None,
            revision: 7,
            dirty: true,
            active: true,
            anchor: 16,
            head: 16,
        };
        let snapshot = buffer.clone();
        let mock = MockHost::new(move |request| match request {
            host::Request::Buffers => Ok(host::Response::Buffers {
                buffers: vec![snapshot.clone()],
            }),
            host::Request::ReadBuffer { id: 42 } => Ok(host::Response::Buffer {
                buffer: snapshot.clone(),
                text: "print process pr".into(),
            }),
            _ => Err("unexpected request".into()),
        });
        let mut words = Words::default();
        words.attach_host(mock.host());
        let Outcome::Completions { items, .. } = words.complete(context(16)).unwrap() else {
            panic!()
        };
        assert_eq!(
            items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            ["print", "process"]
        );
        let mut stale = context(16);
        stale.revision = Some(6);
        assert!(words.complete(stale).is_err());
        assert_eq!(mock.take_requests().len(), 3);
    }
}

#[cfg(test)]
mod activation_tests {
    use super::*;
    use velk_completion_contracts::{Invocation, Triggers};
    fn activation(text: &str, prefix: &str, triggers: Triggers) -> contributions::Activation {
        let mut source = Words::contributions().remove(0);
        source.metadata = triggers.metadata();
        contributions::Activation {
            context: Context {
                input: None,
                path: None,
                language: None,
                revision: Some(1),
                position: Some(2),
            },
            text: text.into(),
            prefix: prefix.into(),
            contributions: vec![source],
        }
    }
    #[test]
    fn policy_respects_source_declarations_and_user_settings() {
        let mut completion = Completion::default();
        let input = activation(
            "r",
            "pr",
            Triggers {
                typing: true,
                ..Default::default()
            },
        );
        assert!(matches!(
            completion.activate(&input),
            Some(Invocation::Typing)
        ));
        assert!(
            completion
                .activate(&activation(
                    "p",
                    "p",
                    Triggers {
                        typing: true,
                        ..Default::default()
                    }
                ))
                .is_none()
        );
        assert!(
            completion
                .activate(&activation("r", "pr", Triggers::default()))
                .is_none()
        );
        assert!(
            completion
                .activate(&activation(".", "", Triggers::default()))
                .is_none()
        );
        let dot = activation(
            ".",
            "",
            Triggers {
                characters: vec![".".into()],
                ..Default::default()
            },
        );
        assert!(
            matches!(completion.activate(&dot), Some(Invocation::Character { character }) if character == ".")
        );
        completion.typing = false;
        assert!(completion.activate(&input).is_none());
        assert!(completion.activate(&dot).is_some());
        completion.automatic = false;
        assert!(completion.activate(&dot).is_none());
    }
    #[test]
    fn policy_rejects_paste_and_whitespace_and_words_decline_character_triggers() {
        let completion = Completion::default();
        let dynamic = || Triggers {
            typing: true,
            dynamic_characters: true,
            ..Default::default()
        };
        assert!(
            completion
                .activate(&activation("paste", "paste", dynamic()))
                .is_none()
        );
        assert!(
            completion
                .activate(&activation("\n", "", dynamic()))
                .is_none()
        );
        let mut input = activation(":", "", dynamic());
        let reason = completion.activate(&input).unwrap();
        input.context.input = Some(
            contributions::Input {
                activation: Some(serde_json::to_string(&reason).unwrap()),
                ..Default::default()
            }
            .encode()
            .unwrap(),
        );
        input.context.position = Some(1);
        let Outcome::Completions { items, .. } = words(&input.context, 1, ": words").unwrap()
        else {
            panic!()
        };
        assert!(items.is_empty());
    }
    #[test]
    fn reducer_sorts_by_provider_metadata_and_namespaces_identity() {
        let source = |command: &str, id: &str, sort: &str| contributions::ContributionResult {
            plugin: "test.source".into(),
            generation: 1,
            command: command.into(),
            outcome: Ok(Outcome::Completions {
                items: vec![velk_completion_contracts::Item {
                    documentation: None,
                    documentation_resolve: None,
                    id: id.into(),
                    filter_text: "other".into(),
                    sort_text: sort.into(),
                    choice: Choice {
                        title: "same label".into(),
                        detail: None,
                        action: ChoiceAction::Semantic {
                            payload: Default::default(),
                        },
                    },
                }],
                continuation: None,
            }),
        };
        let Outcome::Completions { items, .. } = reduce(Collection {
            context: activation("x", "xx", Triggers::default()).context,
            results: vec![
                source("test.first", "same", "20"),
                source("test.second", "same", "10"),
            ],
        })
        .unwrap() else {
            panic!()
        };
        assert_eq!(items[0].id, "test.second:same");
        assert_eq!(items[1].id, "test.first:same");
        assert_eq!(items[0].filter_text, "other");
    }
}

#[cfg(test)]
mod matching_tests {
    use super::*;
    fn complete(text: &str) -> Vec<velk_completion_contracts::Item> {
        let context = Context {
            input: None,
            path: None,
            language: None,
            revision: Some(1),
            position: Some(text.chars().count()),
        };
        let Outcome::Completions { items, .. } = words(&context, 42, text).unwrap() else {
            panic!()
        };
        items
    }
    #[test]
    fn word_source_keeps_typo_candidates_and_proposes_the_original_prefix_range() {
        let items = complete("print println unrelated\npritn");
        assert_eq!(items[0].title, "print");
        assert!(!items.iter().any(|item| item.title == "unrelated"));
        let ChoiceAction::BufferEdit {
            edits, revision: 1, ..
        } = &items[0].action
        else {
            panic!()
        };
        assert_eq!(edits[0].start.line, 1);
        assert_eq!(edits[0].start.column, 0);
        assert_eq!(edits[0].end.column, 5);
        assert_eq!(edits[0].text, "print");
    }
    #[test]
    fn exact_words_elsewhere_are_included_but_the_current_token_is_excluded() {
        assert!(complete("print").is_empty());
        let items = complete("print println\nprint");
        assert_eq!(items[0].title, "print");
        assert_eq!(items.iter().filter(|item| item.title == "print").count(), 1);
    }
    #[test]
    fn result_cap_keeps_the_best_matches_even_when_they_appear_late_in_the_document() {
        let mut text = (0..250)
            .map(|index| format!("print_candidate_{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        text.push_str(" print\nprint");
        let items = complete(&text);
        assert_eq!(items.len(), 200);
        assert_eq!(items[0].title, "print");
    }
}
