//! Default completion policy and an independent document-word contributor.
use velk_completion_contracts::{Collection, Contribution, TOPIC};
use velk_plugin_sdk::{Host, Plugin, Registrar, commands::*, hooks, host};

#[derive(Default)]
pub struct Completion;
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
                topic: TOPIC.into(),
                hook: velk_completion_contracts::REDUCE.into(),
            },
        }]
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::Request(velk_completion_contracts::REDUCE),
            |_: &mut Self, bytes: &[u8]| {
                let input: Collection = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                serde_json::to_vec(&reduce(input)?).map_err(|e| e.to_string())
            },
        )
    }
    fn describe(&mut self) -> String {
        "Combine optional completion sources".into()
    }
}

pub fn reduce(input: Collection) -> Result<Outcome, String> {
    let mut items = Vec::new();
    // Host supplies results in declaration order, independent of response timing.
    for result in input.results {
        if let Ok(Outcome::Completions { items: candidates }) = result.outcome {
            for mut candidate in candidates {
                candidate.action = ChoiceAction::Contributed {
                    plugin: result.plugin.clone(),
                    generation: result.generation,
                    action: Box::new(candidate.action),
                };
                let source = &result.plugin;
                candidate.detail = Some(match candidate.detail {
                    Some(detail) => format!("{source}: {detail}"),
                    None => source.clone(),
                });
                items.push(candidate);
                if items.len() == 1000 {
                    break;
                }
            }
        }
        if items.len() == 1000 {
            break;
        }
    }
    let outcome = Outcome::Completions { items };
    outcome.validate()?;
    Ok(outcome)
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
    let mut seen = std::collections::BTreeSet::new();
    let mut items = Vec::new();
    for word in text.split(|c| !is_word(c)) {
        if word.len() > 1024 || word == prefix || !word.starts_with(&prefix) || !seen.insert(word) {
            continue;
        }
        if word.is_empty() {
            continue;
        }
        items.push(Choice {
            title: word.into(),
            detail: None,
            action: ChoiceAction::BufferEdit {
                id: buffer,
                revision,
                edits: vec![TextEdit {
                    start: begin,
                    end,
                    text: word.into(),
                }],
            },
        });
        if items.len() == 200 {
            break;
        }
    }
    Ok(Outcome::Completions { items })
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
        let Outcome::Completions { items } =
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
        let merged = reduce(Collection {
            context: context(16),
            results: vec![
                source("test.failed", 1, Err("not ready".into())),
                source("test.words", 2, Ok(candidates.clone())),
                source("test.other", 3, Ok(candidates)),
            ],
        })
        .unwrap();
        let Outcome::Completions { items } = merged else {
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
            matches!(reduce(Collection { context: context(0), results: vec![] }).unwrap(), Outcome::Completions { items } if items.is_empty())
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
        let Outcome::Completions { items } = words.complete(context(16)).unwrap() else {
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
