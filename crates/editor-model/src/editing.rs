//! Interactive policy adapter over the authoritative Core document.
use std::{collections::BTreeMap, sync::Arc};
use velk_core::{Buffer, TextEdit, Transaction};
use velk_editing::{Decision, Intent, Pair, Provider, Request, Role, Selection};
#[derive(Default)]
pub struct Session {
    providers: BTreeMap<Role, Arc<dyn Provider>>,
    pairs: Vec<Pair>,
    revision: u64,
    input: velk_editing::input::State,
}
impl Session {
    pub fn set_providers(&mut self, providers: BTreeMap<Role, Arc<dyn Provider>>) {
        let unchanged = match (
            self.providers.get(&Role::Keymap),
            providers.get(&Role::Keymap),
        ) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            self.input = Default::default();
        }
        self.providers = providers;
        self.pairs.clear();
    }
    pub fn input_state(&self) -> &velk_editing::input::State {
        &self.input
    }
    pub fn clear_pairs(&mut self) {
        self.pairs.clear();
    }
    pub fn synchronize(&mut self, buffer: &Buffer) {
        if buffer.revision() == self.revision {
            return;
        }
        if let Some(changes) = buffer.edits_since(self.revision) {
            for change in changes {
                self.pairs.retain_mut(|pair| {
                    if change.replaced.contains(&pair.open) || change.replaced.contains(&pair.close)
                    {
                        return false;
                    }
                    let map = |position| {
                        if position < change.replaced.start {
                            position
                        } else {
                            position - change.replaced.len() + change.inserted
                        }
                    };
                    pair.open = map(pair.open);
                    pair.close = map(pair.close);
                    true
                });
            }
        } else {
            self.pairs.clear();
        }
        self.pairs.retain(|pair| {
            pair.open < pair.close
                && pair.close < buffer.text().len_chars()
                && buffer.text().char(pair.open) == pair.opening
                && buffer.text().char(pair.close) == pair.closing
        });
        self.revision = buffer.revision();
    }
    pub fn evaluate(&mut self, buffer: &mut Buffer, mut intent: Intent) -> Result<bool, String> {
        self.synchronize(buffer);
        let Some(provider) = self.providers.get(&intent.role()) else {
            return Ok(false);
        };
        let selection = buffer.selection();
        let range = selection.range();
        let text = buffer.text();
        let first = text.char_to_line(range.start);
        let last = text.char_to_line(range.end);
        let offset = text.line_to_char(first);
        let end = if last + 1 < text.len_lines() {
            text.line_to_char(last + 1)
        } else {
            text.len_chars()
        };
        let keymap = matches!(intent, Intent::Key(_));
        if let Intent::Key(key) = &mut intent {
            key.state = self.input.clone();
        }
        let context = if keymap {
            String::new()
        } else {
            let context = text.slice(offset..end);
            if context.len_bytes() > velk_editing::MAX_TEXT {
                return Err("interactive editing context exceeds 64 KiB".into());
            }
            context.to_string()
        };
        let request = Request {
            revision: buffer.revision(),
            text: context,
            offset: if keymap { 0 } else { offset },
            document_length: text.len_chars(),
            selection: Selection {
                anchor: selection.anchor,
                head: selection.head,
            },
            intent,
            settings: Default::default(),
            pairs: if keymap { vec![] } else { self.pairs.clone() },
        };
        let decision = provider.evaluate(&request)?;
        decision.validate(&request)?;
        if let Decision::Input(response) = decision {
            if self.input.clamp_caret {
                buffer.clamp_caret_to_line_character();
            }
            for action in response.actions {
                if matches!(
                    action,
                    velk_editing::input::Action::Undo { .. }
                        | velk_editing::input::Action::Redo { .. }
                ) {
                    self.pairs.clear();
                }
                crate::input::apply(buffer, action)?;
            }
            if response.state.clamp_caret {
                buffer.clamp_caret_to_line_character();
            }
            self.input = response.state;
            self.synchronize(buffer);
            return Ok(response.handled);
        }
        let Decision::Apply(proposal) = decision else {
            if keymap {
                self.input = Default::default();
            }
            return Ok(false);
        };
        if !proposal.pairs.is_empty() {
            let mut preview = text.clone();
            let mut edits: Vec<_> = proposal.edits.iter().collect();
            edits.sort_by_key(|e| std::cmp::Reverse(e.start));
            for edit in edits {
                preview.remove(edit.start..edit.end);
                preview.insert(edit.start, &edit.text);
            }
            if proposal.pairs.iter().any(|pair| {
                preview.char(pair.open) != pair.opening || preview.char(pair.close) != pair.closing
            }) {
                return Err("pair does not match proposed text".into());
            }
        }
        buffer
            .apply_transaction(Transaction {
                revision: proposal.revision,
                edits: proposal
                    .edits
                    .into_iter()
                    .map(|e| TextEdit {
                        range: e.start..e.end,
                        text: e.text,
                    })
                    .collect(),
                selection: proposal.selection.map(|s| velk_core::Selection {
                    anchor: s.anchor,
                    head: s.head,
                }),
            })
            .map_err(|e| e.to_string())?;
        self.synchronize(buffer);
        self.pairs.extend(proposal.pairs);
        if self.pairs.len() > 256 {
            self.pairs.drain(..self.pairs.len() - 256);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velk_editing::{Edit, Proposal};
    #[derive(Debug)]
    struct PairProvider {
        invalid: bool,
    }
    impl Provider for PairProvider {
        fn evaluate(&self, request: &Request) -> Result<Decision, String> {
            Ok(Decision::Apply(Proposal {
                revision: request.revision + u64::from(self.invalid),
                edits: vec![Edit {
                    start: request.selection.range().start,
                    end: request.selection.range().end,
                    text: "()".into(),
                }],
                selection: Some(Selection::caret(request.selection.range().start + 1)),
                pairs: vec![Pair {
                    open: request.selection.range().start,
                    close: request.selection.range().start + 1,
                    opening: '(',
                    closing: ')',
                }],
            }))
        }
    }
    #[test]
    fn tracking_keeps_the_newest_pairs_when_the_limit_is_reached() {
        let mut buffer = Buffer::new("");
        let mut session = Session::default();
        session.set_providers(BTreeMap::from([(
            Role::Pairing,
            Arc::new(PairProvider { invalid: false }) as Arc<dyn Provider>,
        )]));
        for _ in 0..257 {
            session
                .evaluate(&mut buffer, Intent::Type("(".into()))
                .unwrap();
        }
        assert_eq!(session.pairs.len(), 256);
        assert_eq!(session.pairs.last().unwrap().close, buffer.selection().head);
    }
    #[test]
    fn proposals_are_atomic_and_pair_anchors_follow_text_changes_and_deletions() {
        let mut buffer = Buffer::new("界");
        buffer.set_caret(1, false);
        let mut session = Session::default();
        session.set_providers(BTreeMap::from([(
            Role::Pairing,
            Arc::new(PairProvider { invalid: true }) as Arc<dyn Provider>,
        )]));
        assert!(
            session
                .evaluate(&mut buffer, Intent::Type("(".into()))
                .is_err()
        );
        assert_eq!(buffer.text().to_string(), "界");
        assert!(!buffer.can_undo());
        session.set_providers(BTreeMap::from([(
            Role::Pairing,
            Arc::new(PairProvider { invalid: false }) as Arc<dyn Provider>,
        )]));
        assert!(
            session
                .evaluate(&mut buffer, Intent::Type("(".into()))
                .unwrap()
        );
        assert_eq!(buffer.text().to_string(), "界()");
        assert_eq!(buffer.selection().head, 2);
        buffer.insert("inside");
        session.synchronize(&buffer);
        assert_eq!(session.pairs[0].close, 8);
        buffer.set_caret(8, false);
        buffer.delete();
        session.synchronize(&buffer);
        assert!(session.pairs.is_empty());
    }
}
