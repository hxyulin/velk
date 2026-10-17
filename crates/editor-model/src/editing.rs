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
    visual: Option<crate::input::VisualSelection>,
    multiple: Option<crate::selections::SelectionSet>,
    snippet: Option<crate::snippets::Session>,
}
impl Session {
    pub fn start_snippet(
        &mut self,
        buffer: &mut Buffer,
        stops: Vec<velk_commands::snippet::Tabstop>,
    ) -> Result<(), String> {
        self.cancel_snippet();
        self.snippet = Some(crate::snippets::Session::new(buffer, stops)?);
        self.project_snippet(buffer)
    }
    pub fn cancel_snippet(&mut self) {
        if self.snippet.take().is_some() {
            self.multiple = None;
        }
    }
    fn project_snippet(&mut self, buffer: &mut Buffer) -> Result<(), String> {
        let snippet = self.snippet.as_ref().ok_or("missing snippet")?;
        let finished = snippet.finished();
        let regions = snippet
            .selections()
            .into_iter()
            .map(|selection| crate::selections::SelectionRegion {
                caret: selection.head,
                selection,
            })
            .collect();
        let set = crate::selections::SelectionSet::new(buffer, regions, 0)?;
        set.project(buffer);
        self.multiple = (!finished).then_some(set);
        if finished {
            self.snippet = None;
        }
        Ok(())
    }
    pub fn navigate_snippet(&mut self, buffer: &mut Buffer, backwards: bool) -> bool {
        self.synchronize(buffer);
        let Some(snippet) = &mut self.snippet else {
            return false;
        };
        buffer.break_undo_group();
        snippet.advance(backwards);
        if self.project_snippet(buffer).is_err() {
            self.cancel_snippet();
        }
        true
    }
    pub fn snippet_active(&self) -> bool {
        self.snippet.is_some()
    }

    /// A new view shares providers, with independent modes, pairs, and selections.
    pub fn new_view(&self) -> Self {
        Self {
            providers: self.providers.clone(),
            ..Self::default()
        }
    }
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
            self.cancel_snippet();
            self.input = Default::default();
            self.visual = None;
            self.multiple = None;
        }
        self.providers = providers;
        self.pairs.clear();
    }
    pub fn input_state(&self) -> &velk_editing::input::State {
        &self.input
    }
    pub fn has_visual_selection(&self) -> bool {
        self.visual.is_some()
    }
    pub fn caret_position(&self, buffer: &Buffer) -> usize {
        self.visual
            .as_ref()
            .map_or(buffer.selection().head, |selection| selection.head)
    }
    pub fn selection_regions(
        &self,
    ) -> Option<std::sync::Arc<Vec<crate::selections::SelectionRegion>>> {
        self.visual
            .as_ref()
            .and_then(|selection| selection.block.as_ref())
            .or(self.multiple.as_ref())
            .map(|set| set.regions.clone())
    }
    pub fn has_multiple_carets(&self) -> bool {
        self.multiple.is_some()
    }
    pub fn has_selection(&self, buffer: &Buffer) -> bool {
        self.selection_regions().map_or_else(
            || !buffer.selection().is_empty(),
            |regions| regions.iter().any(|region| !region.selection.is_empty()),
        )
    }
    pub fn selected_text(&self, buffer: &Buffer) -> String {
        self.visual
            .as_ref()
            .and_then(|selection| selection.block.as_ref())
            .or(self.multiple.as_ref())
            .map_or_else(|| buffer.selected_text(), |set| set.selected_text(buffer))
    }
    pub fn edit_command(
        &mut self,
        buffer: &mut Buffer,
        command: &crate::EditCommand<'_>,
    ) -> Result<bool, String> {
        self.synchronize(buffer);
        if !matches!(
            command,
            crate::EditCommand::Insert(_)
                | crate::EditCommand::Paste(_)
                | crate::EditCommand::Backspace
                | crate::EditCommand::Delete
        ) {
            self.cancel_snippet();
        }
        let block = self
            .visual
            .as_ref()
            .and_then(|selection| selection.block.as_ref())
            .is_some();
        if !block && self.multiple.is_none() {
            if self.visual.is_some() && command.clone().apply(buffer) {
                self.visual = None;
                self.input = Default::default();
                self.synchronize(buffer);
                return Ok(true);
            }
            return Ok(false);
        }
        match command {
            crate::EditCommand::Insert(text) | crate::EditCommand::Paste(text) => {
                if block {
                    let mut set = self.visual.as_ref().unwrap().block.clone().unwrap();
                    set.primary = 0;
                    if matches!(command, crate::EditCommand::Paste(_)) {
                        set.paste(buffer, text)?;
                    } else {
                        set.replace(buffer, text)?;
                    }
                    self.visual = None;
                    self.input = Default::default();
                } else {
                    let set = self.multiple.as_mut().unwrap();
                    if matches!(command, crate::EditCommand::Paste(_)) {
                        set.paste(buffer, text)?;
                    } else {
                        set.replace(buffer, text)?;
                    }
                }
            }
            crate::EditCommand::Backspace | crate::EditCommand::Delete => {
                if block {
                    let mut set = self.visual.as_ref().unwrap().block.clone().unwrap();
                    set.primary = 0;
                    set.replace(buffer, "")?;
                    self.visual = None;
                    self.input = Default::default();
                } else {
                    self.multiple
                        .as_mut()
                        .unwrap()
                        .erase(buffer, matches!(command, crate::EditCommand::Backspace))?;
                }
            }
            _ => {
                self.multiple = None;
                self.visual = None;
                self.input = Default::default();
                return Ok(false);
            }
        }
        self.synchronize(buffer);
        Ok(true)
    }
    pub fn clear_pairs(&mut self) {
        self.pairs.clear();
    }
    pub fn synchronize(&mut self, buffer: &Buffer) {
        if self
            .snippet
            .as_mut()
            .is_some_and(|snippet| !snippet.synchronize(buffer))
        {
            self.cancel_snippet();
        }
        if self
            .visual
            .as_mut()
            .is_some_and(|selection| !selection.synchronize(buffer))
        {
            self.visual = None;
            self.input = Default::default();
        }
        if self
            .multiple
            .as_mut()
            .is_some_and(|set| !set.synchronize(buffer))
        {
            self.multiple = None;
        }
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
                    pair.open = change.map_position(pair.open, velk_core::Affinity::After);
                    pair.close = change.map_position(pair.close, velk_core::Affinity::After);
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
        if !matches!(intent, Intent::Key(_)) && self.multiple.is_some() {
            return Ok(false);
        }
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
            if self.input.clamp_caret && self.visual.is_none() && self.multiple.is_none() {
                buffer.clamp_caret_to_line_character();
            }
            for action in response.actions {
                if matches!(
                    action,
                    velk_editing::input::Action::Undo { .. }
                        | velk_editing::input::Action::Redo { .. }
                ) {
                    self.pairs.clear();
                    self.cancel_snippet();
                }
                crate::input::apply_visual(buffer, action, &mut self.visual, &mut self.multiple)?;
            }
            if response.state.clamp_caret && self.visual.is_none() && self.multiple.is_none() {
                buffer.clamp_caret_to_line_character();
            }
            self.input = response.state;
            self.synchronize(buffer);
            return Ok(response.handled);
        }
        let Decision::Apply(proposal) = decision else {
            if keymap {
                self.input = Default::default();
                self.visual = None;
                self.multiple = None;
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
        let mut buffer =
            velk_core::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
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
        let mut buffer =
            velk_core::Buffer::with_history("界", std::sync::Arc::new(velk_undo_tree::Tree));
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
