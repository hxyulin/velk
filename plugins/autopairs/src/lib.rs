//! Default lexical pairing policy; the host owns tracking and transactions.
use velk_plugin_sdk::{
    CapabilityDeclaration, EditingBehavior, Plugin, Registrar, editing::*, settings,
};
#[derive(Default)]
pub struct Autopairs;
impl Plugin for Autopairs {
    const ID: &'static str = "velk.autopairs";
    const NAME: &'static str = "Autopairs";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[
        CapabilityDeclaration::required(
            "editor.buffer.read",
            "Inspect text, selection, and host-tracked pairs.",
        ),
        CapabilityDeclaration::required(
            "editor.buffer.write",
            "Propose matching delimiters and caret placement.",
        ),
    ];
    fn describe(&mut self) -> String {
        "Matching brackets and selection wrapping.".into()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.editing::<Self>()
    }
    fn settings() -> Vec<settings::Definition> {
        vec![settings::Definition {
            key: "pair_quotes".into(),
            title: "Pair quotes".into(),
            description: "Lexically pair single and double quotes without syntax parsing.".into(),
            kind: settings::Kind::Boolean,
            default: false.into(),
        }]
    }
}
impl EditingBehavior for Autopairs {
    const ROLE: Role = Role::Pairing;
    fn evaluate(request: &Request) -> Result<Decision, String> {
        request.validate()?;
        let range = request.selection.range();
        if matches!(request.intent, Intent::Backspace) {
            if range.is_empty()
                && let Some(pair) = request
                    .pairs
                    .iter()
                    .find(|pair| pair.open + 1 == range.start && pair.close == range.start)
            {
                return Ok(Decision::Apply(Proposal {
                    revision: request.revision,
                    edits: vec![Edit {
                        start: pair.open,
                        end: pair.close + 1,
                        text: String::new(),
                    }],
                    selection: Some(Selection::caret(pair.open)),
                    pairs: vec![],
                }));
            }
            return Ok(Decision::Decline);
        }
        let Intent::Type(typed) = &request.intent else {
            return Ok(Decision::Decline);
        };
        let mut characters = typed.chars();
        let Some(opening) = characters.next() else {
            return Ok(Decision::Decline);
        };
        if characters.next().is_some() {
            return Ok(Decision::Decline);
        }
        if range.is_empty()
            && request
                .pairs
                .iter()
                .any(|pair| pair.close == range.start && pair.closing == opening)
        {
            return Ok(Decision::Apply(Proposal {
                revision: request.revision,
                edits: vec![],
                selection: Some(Selection::caret(range.start + 1)),
                pairs: vec![],
            }));
        }
        let quotes = request
            .settings
            .get("pair_quotes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let closing = match opening {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '\'' | '"' if quotes => opening,
            _ => return Ok(Decision::Decline),
        };
        let text = request.characters();
        let start = range.start - request.offset;
        let end = range.end - request.offset;
        if range.is_empty() {
            if text
                .get(end)
                .is_some_and(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == opening)
            {
                return Ok(Decision::Decline);
            }
            if matches!(opening, '\'' | '"')
                && (start > 0 && (text[start - 1].is_alphanumeric() || text[start - 1] == '_')
                    || text[..start]
                        .iter()
                        .rev()
                        .take_while(|ch| **ch == '\\')
                        .count()
                        % 2
                        == 1)
            {
                return Ok(Decision::Decline);
            }
        }
        let edits = if range.is_empty() {
            vec![Edit {
                start: range.start,
                end: range.end,
                text: format!("{opening}{closing}"),
            }]
        } else {
            vec![
                Edit {
                    start: range.start,
                    end: range.start,
                    text: opening.to_string(),
                },
                Edit {
                    start: range.end,
                    end: range.end,
                    text: closing.to_string(),
                },
            ]
        };
        let decision = Decision::Apply(Proposal {
            revision: request.revision,
            edits,
            selection: Some(Selection {
                anchor: request.selection.anchor + 1,
                head: request.selection.head + 1,
            }),
            pairs: vec![Pair {
                open: range.start,
                close: range.end + 1,
                opening,
                closing,
            }],
        });
        decision.validate(request)?;
        Ok(decision)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request(text: &str, selection: Selection, typed: &str, quotes: bool) -> Request {
        Request {
            revision: 0,
            text: text.into(),
            offset: 0,
            document_length: text.chars().count(),
            selection,
            intent: Intent::Type(typed.into()),
            settings: std::collections::BTreeMap::from([("pair_quotes".into(), quotes.into())]),
            pairs: vec![],
        }
    }
    #[test]
    fn pairing_wraps_unicode_selections_preserves_direction_and_undo() {
        for selection in [
            Selection::caret(1),
            Selection { anchor: 0, head: 1 },
            Selection { anchor: 1, head: 0 },
        ] {
            let mut buffer = velk_core::Buffer::new("界");
            buffer.set_caret(selection.anchor, false);
            buffer.set_caret(selection.head, true);
            let Decision::Apply(proposal) =
                Autopairs::evaluate(&request("界", selection, "(", false)).unwrap()
            else {
                panic!("expected pair");
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
            assert_eq!(
                buffer.text().to_string(),
                if selection.range().is_empty() {
                    "界()"
                } else {
                    "(界)"
                }
            );
            assert_eq!(buffer.selection().anchor, selection.anchor + 1);
            assert_eq!(buffer.selection().head, selection.head + 1);
            buffer.undo();
            assert_eq!(buffer.text().to_string(), "界");
            assert!(!buffer.can_undo());
        }
    }
    #[test]
    fn skip_and_paired_backspace_require_a_tracked_empty_pair() {
        let mut request = request("()", Selection::caret(1), ")", false);
        assert_eq!(Autopairs::evaluate(&request).unwrap(), Decision::Decline);
        request.pairs.push(Pair {
            open: 0,
            close: 1,
            opening: '(',
            closing: ')',
        });
        let Decision::Apply(proposal) = Autopairs::evaluate(&request).unwrap() else {
            panic!("expected skip");
        };
        assert!(proposal.edits.is_empty());
        assert_eq!(proposal.selection.unwrap().head, 2);
        request.intent = Intent::Backspace;
        let Decision::Apply(proposal) = Autopairs::evaluate(&request).unwrap() else {
            panic!("expected deletion");
        };
        assert_eq!(
            proposal.edits,
            [Edit {
                start: 0,
                end: 2,
                text: String::new()
            }]
        );
        request.selection = Selection { anchor: 0, head: 2 };
        assert_eq!(Autopairs::evaluate(&request).unwrap(), Decision::Decline);
    }
    #[test]
    fn quotes_are_opt_in_and_escaped_or_identifier_quotes_remain_literal() {
        for (text, typed, quotes) in [
            ("", "'", false),
            ("word", "'", true),
            ("\\", "\"", true),
            ("", "ab", false),
        ] {
            assert_eq!(
                Autopairs::evaluate(&request(
                    text,
                    Selection::caret(text.chars().count()),
                    typed,
                    quotes
                ))
                .unwrap(),
                Decision::Decline
            );
        }
        assert!(matches!(
            Autopairs::evaluate(&request("", Selection::caret(0), "\"", true)).unwrap(),
            Decision::Apply(_)
        ));
        assert_eq!(
            Autopairs::evaluate(&request("word", Selection::caret(0), "(", false)).unwrap(),
            Decision::Decline
        );
    }
}
