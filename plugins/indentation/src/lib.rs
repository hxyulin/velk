//! Default indentation policy, independent of the renderer and document engine.
use velk_plugin_sdk::{
    CapabilityDeclaration, EditingBehavior, Plugin, Registrar, editing::*, settings,
};
#[derive(Default)]
pub struct Indentation;
impl Plugin for Indentation {
    const ID: &'static str = "velk.indentation";
    const NAME: &'static str = "Indentation";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[
        CapabilityDeclaration::required(
            "editor.buffer.read",
            "Inspect selected lines and leading whitespace.",
        ),
        CapabilityDeclaration::required(
            "editor.buffer.write",
            "Propose indentation edits and caret placement.",
        ),
    ];
    fn describe(&mut self) -> String {
        "Tab stops and selection indentation.".into()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.editing::<Self>()
    }
    fn settings() -> Vec<settings::Definition> {
        vec![
            settings::Definition {
                key: "tab_width".into(),
                title: "Tab width".into(),
                description: "Columns per indentation unit.".into(),
                kind: settings::Kind::Integer { min: 1, max: 16 },
                default: 4.into(),
            },
            settings::Definition {
                key: "use_tabs".into(),
                title: "Use tabs".into(),
                description: "Insert literal tabs instead of spaces.".into(),
                kind: settings::Kind::Boolean,
                default: false.into(),
            },
        ]
    }
}
impl EditingBehavior for Indentation {
    const ROLE: Role = Role::Indentation;
    fn evaluate(request: &Request) -> Result<Decision, String> {
        request.validate()?;
        let width = request
            .settings
            .get("tab_width")
            .and_then(|v| v.as_u64())
            .unwrap_or(4)
            .clamp(1, 16) as usize;
        let tabs = request
            .settings
            .get("use_tabs")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let text = request.characters();
        let range = request.selection.range();
        let start = range.start - request.offset;
        let end = range.end - request.offset;
        let mut proposal = Proposal {
            revision: request.revision,
            edits: vec![],
            selection: None,
            pairs: vec![],
        };
        match request.intent {
            Intent::Indent if range.is_empty() => {
                let column = text[..start].iter().fold(0, |column, ch| {
                    if *ch == '\t' {
                        column + width - column % width
                    } else {
                        column + 1
                    }
                });
                proposal.edits.push(Edit {
                    start: range.start,
                    end: range.end,
                    text: if tabs {
                        "\t".into()
                    } else {
                        " ".repeat(width - column % width)
                    },
                });
            }
            Intent::Indent | Intent::Outdent => {
                for line in std::iter::once(0).chain(
                    text.iter()
                        .enumerate()
                        .filter(|(_, ch)| **ch == '\n')
                        .map(|(i, _)| i + 1),
                ) {
                    if line > end || (!range.is_empty() && line == end) {
                        break;
                    }
                    if matches!(request.intent, Intent::Indent) {
                        proposal.edits.push(Edit {
                            start: request.offset + line,
                            end: request.offset + line,
                            text: if tabs { "\t".into() } else { " ".repeat(width) },
                        });
                    } else {
                        let remove = if text.get(line) == Some(&'\t') {
                            1
                        } else {
                            text[line..]
                                .iter()
                                .take_while(|ch| **ch == ' ')
                                .count()
                                .min(width)
                        };
                        if remove > 0 {
                            proposal.edits.push(Edit {
                                start: request.offset + line,
                                end: request.offset + line + remove,
                                text: String::new(),
                            });
                        }
                    }
                    if range.is_empty() {
                        break;
                    }
                }
            }
            _ => return Ok(Decision::Decline),
        }
        let decision = Decision::Apply(proposal);
        decision.validate(request)?;
        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn apply(text: &str, selection: Selection, intent: Intent, tabs: bool) -> velk_core::Buffer {
        let mut buffer = velk_core::Buffer::new(text);
        buffer.set_caret(selection.anchor, false);
        buffer.set_caret(selection.head, true);
        let request = Request {
            revision: 0,
            text: text.into(),
            offset: 0,
            document_length: text.chars().count(),
            selection,
            intent,
            settings: std::collections::BTreeMap::from([("use_tabs".into(), tabs.into())]),
            pairs: vec![],
        };
        let Decision::Apply(proposal) = Indentation::evaluate(&request).unwrap() else {
            panic!("expected edit");
        };
        buffer
            .apply_transaction(velk_core::Transaction {
                revision: 0,
                edits: proposal
                    .edits
                    .into_iter()
                    .map(|e| velk_core::TextEdit {
                        range: e.start..e.end,
                        text: e.text,
                    })
                    .collect(),
                selection: proposal.selection.map(|s| velk_core::Selection {
                    anchor: s.anchor,
                    head: s.head,
                }),
            })
            .unwrap();
        buffer
    }
    #[test]
    fn tab_aligns_after_text_and_existing_tabs_and_is_one_undo_step() {
        for (text, expected, position) in
            [("ab", "ab  ", 2), ("\tx", "\tx   ", 2), ("界", "界   ", 1)]
        {
            let mut buffer = apply(text, Selection::caret(position), Intent::Indent, false);
            assert_eq!(buffer.text().to_string(), expected);
            buffer.undo();
            assert_eq!(buffer.text().to_string(), text);
            assert!(!buffer.can_undo());
        }
        assert_eq!(
            apply("ab", Selection::caret(2), Intent::Indent, true)
                .text()
                .to_string(),
            "ab\t"
        );
    }
    #[test]
    fn selected_lines_exclude_terminal_line_and_outdent_restores_text_and_direction() {
        let selection = Selection { anchor: 4, head: 0 };
        let mut buffer = apply("a\nb\nc", selection, Intent::Indent, false);
        assert_eq!(buffer.text().to_string(), "    a\n    b\nc");
        assert!(buffer.selection().anchor > buffer.selection().head);
        let selected = Selection {
            anchor: buffer.selection().anchor,
            head: buffer.selection().head,
        };
        let result = apply(&buffer.text().to_string(), selected, Intent::Outdent, false);
        assert_eq!(result.text().to_string(), "a\nb\nc");
        buffer.undo();
        assert_eq!(buffer.text().to_string(), "a\nb\nc");
        assert_eq!(
            apply("  a", Selection::caret(2), Intent::Outdent, false)
                .text()
                .to_string(),
            "a"
        );
        assert_eq!(
            apply("\ta", Selection::caret(1), Intent::Outdent, false)
                .text()
                .to_string(),
            "a"
        );
    }
}
