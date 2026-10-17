//! Expanded snippets use Unicode scalar offsets relative to the primary insertion.
use crate::WorkspaceEdit;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    pub pattern: String,
    pub format: String,
    pub options: String,
}
impl Transform {
    /// Structural limits only; syntax and evaluation belong to the selected provider.
    pub fn validate(&self) -> Result<(), String> {
        if self.pattern.len() > 4096 || self.format.len() > 4096 || self.options.len() > 128 {
            return Err("snippet transform exceeds limits".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tabstop {
    /// Positive indices navigate in ascending order; zero is the final stop.
    pub index: u32,
    pub range: Range<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Derived occurrences are refreshed when leaving their source tabstop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnippetEdit {
    pub edit: WorkspaceEdit,
    /// Primary insertion indices in the original, unsorted edit batch.
    pub document: usize,
    pub replacement: usize,
    pub stops: Vec<Tabstop>,
}
impl SnippetEdit {
    pub fn validate(&self) -> Result<(), String> {
        self.edit.validate()?;
        let text = &self
            .edit
            .documents
            .get(self.document)
            .and_then(|document| document.edits.get(self.replacement))
            .ok_or("missing snippet replacement")?
            .text;
        if text.len() > 64 * 1024 {
            return Err("expanded snippet exceeds 64 KiB".into());
        }
        if text.contains('\r') {
            return Err("expanded snippet must use LF line endings".into());
        }
        validate_stops(&self.stops, text.chars().count())?;
        let characters: Vec<_> = text.chars().collect();
        for stop in &self.stops {
            if self.stops.iter().any(|other| {
                other.index == stop.index
                    && other.transform.is_none()
                    && stop.transform.is_none()
                    && characters[other.range.clone()] != characters[stop.range.clone()]
            }) {
                return Err("snippet mirrors have different defaults".into());
            }
        }
        Ok(())
    }
}
pub fn validate_stops(stops: &[Tabstop], length: usize) -> Result<(), String> {
    if stops.len() > 256
        || stops
            .iter()
            .any(|stop| stop.range.start > stop.range.end || stop.range.end > length)
        || stops.iter().filter(|stop| stop.index == 0).count() != 1
    {
        return Err("invalid snippet tabstops".into());
    }
    for stop in stops {
        if stop.choices.len() > 32
            || stop
                .choices
                .iter()
                .any(|value| value.len() > 4096 || value.contains('\r') || value.contains('\0'))
            || stop.index == 0 && (!stop.choices.is_empty() || stop.transform.is_some())
            || stop.transform.is_some()
                && (!stop.choices.is_empty()
                    || !stops
                        .iter()
                        .any(|source| source.index == stop.index && source.transform.is_none()))
        {
            return Err("invalid snippet choices or transform source".into());
        }
        if stop.transform.is_none()
            && stops.iter().any(|other| {
                other.index == stop.index
                    && other.transform.is_none()
                    && other.choices != stop.choices
            })
        {
            return Err("snippet mirrors have different choices".into());
        }
        if let Some(transform) = &stop.transform {
            transform.validate()?;
        }
    }
    for (index, stop) in stops.iter().enumerate() {
        if stops[..index].iter().any(|other| {
            other.index == stop.index
                && (other.range == stop.range
                    || other.range.start < stop.range.end && stop.range.start < other.range.end)
        }) {
            return Err("duplicate snippet tabstop".into());
        }
    }
    Ok(())
}

pub const CONTRACT: &str = "velk.snippet.v1";
pub const HOOK: &str = CONTRACT;
pub const DEFAULT_PROVIDER: &str = "velk.snippets";
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub type Variables = BTreeMap<String, String>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expansion {
    pub text: String,
    pub stops: Vec<Tabstop>,
}
impl Expansion {
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > 64 * 1024 || self.text.contains('\r') {
            return Err("invalid expanded snippet text".into());
        }
        validate_stops(&self.stops, self.text.chars().count())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub edit: WorkspaceEdit,
    pub document: usize,
    pub replacement: usize,
    pub variables: Variables,
}
impl Template {
    pub fn validate(&self) -> Result<(), String> {
        self.edit.validate()?;
        let source = &self
            .edit
            .documents
            .get(self.document)
            .and_then(|document| document.edits.get(self.replacement))
            .ok_or("missing snippet template replacement")?
            .text;
        validate_input(source, &self.variables)
    }
    pub fn expand(mut self, provider: &dyn Provider) -> Result<SnippetEdit, String> {
        self.validate()?;
        let primary = &mut self.edit.documents[self.document].edits[self.replacement];
        let expansion = provider.expand(&primary.text, &self.variables)?;
        expansion.validate()?;
        primary.text = expansion.text;
        let snippet = SnippetEdit {
            edit: self.edit,
            document: self.document,
            replacement: self.replacement,
            stops: expansion.stops,
        };
        snippet.validate()?;
        Ok(snippet)
    }
}
pub fn validate_input(source: &str, variables: &Variables) -> Result<(), String> {
    if source.len() > 64 * 1024
        || variables.len() > 128
        || variables
            .iter()
            .any(|(name, value)| name.len() > 128 || value.len() > 64 * 1024)
    {
        return Err("snippet or variable context exceeds limits".into());
    }
    Ok(())
}
/// Reentrant, bounded local computation. Providers must not perform I/O or call host services.
pub trait Provider: std::fmt::Debug + Send + Sync {
    fn expand(&self, source: &str, variables: &Variables) -> Result<Expansion, String>;
    fn transform(&self, transform: &Transform, value: &str) -> Result<String, String>;
}
pub type Providers = BTreeMap<String, std::sync::Arc<dyn Provider>>;
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Expand {
        source: String,
        variables: Variables,
    },
    Transform {
        transform: Transform,
        value: String,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Expansion { expansion: Expansion },
    Text { text: String },
}
pub fn evaluate(provider: &dyn Provider, request: Request) -> Result<Response, String> {
    match request {
        Request::Expand { source, variables } => {
            validate_input(&source, &variables)?;
            let expansion = provider.expand(&source, &variables)?;
            expansion.validate()?;
            Ok(Response::Expansion { expansion })
        }
        Request::Transform { transform, value } => {
            transform.validate()?;
            if value.len() > 64 * 1024 {
                return Err("snippet transform input too large".into());
            }
            let text = provider.transform(&transform, &value)?;
            if text.len() > 64 * 1024 || text.contains('\r') {
                return Err("invalid snippet transform output".into());
            }
            Ok(Response::Text { text })
        }
    }
}
