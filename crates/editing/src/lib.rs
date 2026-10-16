//! Interactive editing decisions. Offsets are absolute Unicode scalar offsets.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt::Debug};
pub const MAX_TEXT: usize = 64 * 1024;
pub const MAX_EDITS: usize = 4096;
pub const MAX_WIRE: usize = 4 * 1024 * 1024;
pub const INDENT_HOOK: &str = "velk.editing.indentation";
pub const PAIRS_HOOK: &str = "velk.editing.pairing";
pub const CONTRACT: &str = "velk.editing.provider";
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Indentation,
    Pairing,
}
impl Role {
    pub fn hook(self) -> &'static str {
        match self {
            Self::Indentation => INDENT_HOOK,
            Self::Pairing => PAIRS_HOOK,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Intent {
    Indent,
    Outdent,
    Newline,
    Backspace,
    Type(String),
}
impl Intent {
    pub fn role(&self) -> Role {
        match self {
            Self::Indent | Self::Outdent | Self::Newline => Role::Indentation,
            _ => Role::Pairing,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}
impl Selection {
    pub fn range(self) -> std::ops::Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
    pub fn caret(position: usize) -> Self {
        Self {
            anchor: position,
            head: position,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub open: usize,
    pub close: usize,
    pub opening: char,
    pub closing: char,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub revision: u64,
    /// Complete selected logical lines. Ends at EOF or a logical line boundary.
    pub text: String,
    pub offset: usize,
    pub document_length: usize,
    pub selection: Selection,
    pub intent: Intent,
    /// Resolved settings for the selected provider.
    pub settings: BTreeMap<String, serde_json::Value>,
    /// Host-tracked pairs transformed through earlier edits.
    pub pairs: Vec<Pair>,
}
impl Request {
    pub fn validate(&self) -> Result<(), String> {
        let end = self
            .offset
            .checked_add(self.text.chars().count())
            .ok_or("context overflow")?;
        let range = self.selection.range();
        if self.text.len() > MAX_TEXT
            || end > self.document_length
            || range.start < self.offset
            || range.end > end
            || self.pairs.len() > 256
            || self
                .pairs
                .iter()
                .any(|pair| pair.open >= pair.close || pair.close >= self.document_length)
            || matches!(&self.intent, Intent::Type(text) if text.len() > MAX_TEXT)
        {
            return Err("invalid editing context".into());
        }
        Ok(())
    }
    pub fn characters(&self) -> Vec<char> {
        self.text.chars().collect()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub revision: u64,
    pub edits: Vec<Edit>,
    pub selection: Option<Selection>,
    /// Newly created pairs, in final-document coordinates.
    pub pairs: Vec<Pair>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "proposal", rename_all = "snake_case")]
pub enum Decision {
    Decline,
    Apply(Proposal),
}
impl Decision {
    pub fn validate(&self, request: &Request) -> Result<(), String> {
        request.validate()?;
        let Self::Apply(proposal) = self else {
            return Ok(());
        };
        if proposal.revision != request.revision
            || proposal.edits.len() > MAX_EDITS
            || proposal.pairs.len() > 256
        {
            return Err("invalid editing proposal".into());
        }
        let context_end = request.offset + request.text.chars().count();
        let mut edits: Vec<_> = proposal.edits.iter().collect();
        edits.sort_by_key(|edit| (edit.start, edit.end));
        let mut length = request.document_length;
        let mut bytes = 0usize;
        for edit in &edits {
            bytes = bytes
                .checked_add(edit.text.len())
                .ok_or("edit size overflow")?;
            if edit.start < request.offset
                || edit.start > edit.end
                || edit.end > context_end
                || bytes > MAX_TEXT
                || edit.text.contains('\r')
            {
                return Err("invalid editing range or text".into());
            }
            length = length
                .checked_sub(edit.end - edit.start)
                .and_then(|n| n.checked_add(edit.text.chars().count()))
                .ok_or("edit length overflow")?;
        }
        if edits
            .windows(2)
            .any(|e| e[0].end > e[1].start || e[0].start == e[1].start)
            || proposal
                .selection
                .is_some_and(|s| s.anchor > length || s.head > length)
            || proposal
                .pairs
                .iter()
                .any(|p| p.open >= p.close || p.close >= length)
        {
            return Err("overlapping edits or invalid final selection/pairs".into());
        }
        Ok(())
    }
}
/// Pure synchronous decisions. Implementations must not perform I/O, acquire
/// plugin-instance locks, or retain input. The host applies the proposal.
pub trait Provider: Debug + Send + Sync {
    fn evaluate(&self, request: &Request) -> Result<Decision, String>;
}
pub type Providers = BTreeMap<(String, Role), std::sync::Arc<dyn Provider>>;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proposals_reject_stale_overlapping_out_of_context_and_invalid_selection() {
        let request = Request {
            revision: 7,
            text: "界()".into(),
            offset: 2,
            document_length: 5,
            selection: Selection::caret(3),
            intent: Intent::Newline,
            settings: Default::default(),
            pairs: vec![],
        };
        request.validate().unwrap();
        let valid = Proposal {
            revision: 7,
            edits: vec![Edit {
                start: 3,
                end: 3,
                text: "\n".into(),
            }],
            selection: Some(Selection::caret(4)),
            pairs: vec![],
        };
        Decision::Apply(valid.clone()).validate(&request).unwrap();
        for proposal in [
            Proposal {
                revision: 6,
                ..valid.clone()
            },
            Proposal {
                edits: vec![valid.edits[0].clone(), valid.edits[0].clone()],
                ..valid.clone()
            },
            Proposal {
                edits: vec![Edit {
                    start: 0,
                    end: 1,
                    text: "".into(),
                }],
                ..valid.clone()
            },
            Proposal {
                selection: Some(Selection::caret(99)),
                ..valid.clone()
            },
            Proposal {
                edits: vec![Edit {
                    start: 3,
                    end: 3,
                    text: "\r\n".into(),
                }],
                ..valid.clone()
            },
        ] {
            assert!(Decision::Apply(proposal).validate(&request).is_err());
        }
        let bytes = serde_json::to_vec(&request).unwrap();
        let decoded: Request = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.text, request.text);
    }
}
