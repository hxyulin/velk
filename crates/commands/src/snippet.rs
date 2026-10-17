//! Expanded snippets use Unicode scalar offsets relative to the primary insertion.
mod transform;
use crate::WorkspaceEdit;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};
pub use transform::Transform;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    pub stops: Vec<Tabstop>,
}
#[derive(Clone)]
enum Node {
    Text(String),
    Stop(u32, Option<Vec<Node>>),
    Variable(String, Option<Vec<Node>>),
    Transform(u32, Transform),
    VariableTransform(String, Transform),
}
#[derive(Default)]
struct Parser {
    chars: Vec<char>,
    cursor: usize,
    definitions: BTreeMap<u32, Vec<Node>>,
    choices: BTreeMap<u32, Vec<String>>,
    maximum: u32,
}
pub type Variables = BTreeMap<String, String>;
/// Expand without ambient context. Unresolved variables become editable placeholders.
pub fn expand(source: &str) -> Result<Expansion, String> {
    expand_with_variables(source, &Variables::new())
}
/// Values are supplied explicitly; expansion never reads files, environment or clipboard.
pub fn expand_with_variables(source: &str, variables: &Variables) -> Result<Expansion, String> {
    if source.len() > 64 * 1024
        || variables.len() > 128
        || variables
            .iter()
            .any(|(name, value)| name.len() > 128 || value.len() > 64 * 1024)
    {
        return Err("snippet or variable context exceeds limits".into());
    }
    let mut parser = Parser {
        chars: source
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .collect(),
        ..Default::default()
    };
    let nodes = parser.parse(false, 0)?;
    let mut renderer = Renderer {
        parser: &parser,
        variables,
        unknown: BTreeMap::new(),
        next: parser.maximum,
        expansion: Expansion {
            text: String::new(),
            stops: Vec::new(),
        },
    };
    renderer.render(&nodes, &mut Vec::new(), 0)?;
    if !renderer.expansion.stops.iter().any(|stop| stop.index == 0) {
        let end = renderer.expansion.text.chars().count();
        renderer.expansion.stops.push(Tabstop {
            index: 0,
            range: end..end,
            choices: vec![],
            transform: None,
        });
    }
    validate_stops(
        &renderer.expansion.stops,
        renderer.expansion.text.chars().count(),
    )?;
    Ok(renderer.expansion)
}
impl Parser {
    fn parse(&mut self, nested: bool, depth: usize) -> Result<Vec<Node>, String> {
        if depth > 16 {
            return Err("snippet nesting exceeds 16 levels".into());
        }
        let mut nodes = Vec::new();
        let mut text = String::new();
        while let Some(&character) = self.chars.get(self.cursor) {
            self.cursor += 1;
            match character {
                '}' if nested => {
                    if !text.is_empty() {
                        nodes.push(Node::Text(text));
                    }
                    return Ok(nodes);
                }
                '\\' => {
                    let escaped = *self
                        .chars
                        .get(self.cursor)
                        .ok_or("unfinished snippet escape")?;
                    if matches!(escaped, '$' | '}' | '\\') {
                        self.cursor += 1;
                        text.push(escaped);
                    } else {
                        text.push('\\');
                    }
                }
                '$' => {
                    if !text.is_empty() {
                        nodes.push(Node::Text(std::mem::take(&mut text)));
                    }
                    let braced = self.chars.get(self.cursor) == Some(&'{');
                    if braced {
                        self.cursor += 1;
                    }
                    let start = self.cursor;
                    let numbered = self
                        .chars
                        .get(self.cursor)
                        .is_some_and(char::is_ascii_digit);
                    if numbered {
                        while self
                            .chars
                            .get(self.cursor)
                            .is_some_and(char::is_ascii_digit)
                        {
                            self.cursor += 1;
                        }
                    } else if self
                        .chars
                        .get(self.cursor)
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_')
                    {
                        while self
                            .chars
                            .get(self.cursor)
                            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                        {
                            self.cursor += 1;
                        }
                    }
                    if self.cursor == start {
                        if braced {
                            return Err("invalid snippet variable or stop".into());
                        }
                        text.push('$');
                        continue;
                    }
                    let name: String = self.chars[start..self.cursor].iter().collect();
                    let index = if numbered {
                        let index = name.parse::<u32>().map_err(|e| e.to_string())?;
                        self.maximum = self.maximum.max(index);
                        Some(index)
                    } else {
                        None
                    };
                    if braced && self.chars.get(self.cursor) == Some(&'/') {
                        let transform = self.transform()?;
                        nodes.push(if let Some(index) = index {
                            Node::Transform(index, transform)
                        } else {
                            Node::VariableTransform(name, transform)
                        });
                        continue;
                    }
                    let default = if !braced {
                        None
                    } else {
                        match self.chars.get(self.cursor) {
                            Some('}') => {
                                self.cursor += 1;
                                None
                            }
                            Some(':') => {
                                self.cursor += 1;
                                Some(self.parse(true, depth + 1)?)
                            }
                            Some('|') if index.is_some() => {
                                self.cursor += 1;
                                let values = self.choice()?;
                                self.choices
                                    .entry(index.unwrap())
                                    .or_insert_with(|| values.clone());
                                Some(vec![Node::Text(values[0].clone())])
                            }
                            _ => return Err("malformed snippet placeholder".into()),
                        }
                    };
                    if let Some(index) = index {
                        if let Some(default) = &default {
                            self.definitions
                                .entry(index)
                                .or_insert_with(|| default.clone());
                        }
                        nodes.push(Node::Stop(index, default));
                    } else {
                        nodes.push(Node::Variable(name, default));
                    }
                }
                character => text.push(character),
            }
        }
        if nested {
            return Err("unfinished snippet placeholder".into());
        }
        if !text.is_empty() {
            nodes.push(Node::Text(text));
        }
        Ok(nodes)
    }
    fn choice(&mut self) -> Result<Vec<String>, String> {
        let mut values = vec![String::new()];
        loop {
            let next = *self
                .chars
                .get(self.cursor)
                .ok_or("unfinished snippet choice")?;
            self.cursor += 1;
            match next {
                '|' if self.chars.get(self.cursor) == Some(&'}') => {
                    self.cursor += 1;
                    break;
                }
                ',' => {
                    if values.len() == 32 {
                        return Err("too many snippet choices".into());
                    }
                    values.push(String::new());
                }
                '\\' => {
                    let escaped = *self
                        .chars
                        .get(self.cursor)
                        .ok_or("unfinished choice escape")?;
                    self.cursor += 1;
                    if !matches!(escaped, '|' | ',' | '\\') {
                        values.last_mut().unwrap().push('\\');
                    }
                    values.last_mut().unwrap().push(escaped);
                }
                c => values.last_mut().unwrap().push(c),
            }
            if values.last().unwrap().len() > 4096 {
                return Err("snippet choice too large".into());
            }
        }
        Ok(values)
    }
    fn segment(&mut self, format: bool) -> Result<String, String> {
        let mut result = String::new();
        let mut depth = 0;
        loop {
            let c = *self
                .chars
                .get(self.cursor)
                .ok_or("unfinished snippet transform")?;
            self.cursor += 1;
            if c == '\\' {
                let next = *self
                    .chars
                    .get(self.cursor)
                    .ok_or("unfinished transform escape")?;
                self.cursor += 1;
                if next != '/' {
                    result.push('\\');
                }
                result.push(next);
                continue;
            }
            if format && c == '$' && self.chars.get(self.cursor) == Some(&'{') {
                depth += 1;
                result.push_str("${");
                self.cursor += 1;
                continue;
            }
            if format && c == '}' && depth > 0 {
                depth -= 1;
            }
            if c == '/' && depth == 0 {
                return Ok(result);
            }
            result.push(c);
            if result.len() > 4096 || depth > 16 {
                return Err("snippet transform exceeds limits".into());
            }
        }
    }
    fn transform(&mut self) -> Result<Transform, String> {
        self.cursor += 1;
        let pattern = self.segment(false)?;
        let format = self.segment(true)?;
        let start = self.cursor;
        while self.chars.get(self.cursor).is_some_and(|c| *c != '}') {
            self.cursor += 1;
        }
        if self.chars.get(self.cursor) != Some(&'}') {
            return Err("unfinished transform options".into());
        }
        let options = self.chars[start..self.cursor].iter().collect();
        self.cursor += 1;
        let transform = Transform {
            pattern,
            format,
            options,
        };
        transform.validate()?;
        Ok(transform)
    }
}
struct Renderer<'a> {
    parser: &'a Parser,
    variables: &'a Variables,
    unknown: BTreeMap<String, u32>,
    next: u32,
    expansion: Expansion,
}
impl Renderer<'_> {
    fn render(&mut self, nodes: &[Node], stack: &mut Vec<u32>, depth: usize) -> Result<(), String> {
        if depth > 16 {
            return Err("snippet expansion nesting exceeds 16 levels".into());
        }
        for node in nodes {
            match node {
                Node::Text(text) => self.expansion.text.push_str(text),
                Node::Stop(index, default) => {
                    if stack.contains(index) {
                        return Err("recursive snippet placeholder".into());
                    }
                    let start = self.expansion.text.chars().count();
                    if let Some(default) = default
                        .as_ref()
                        .or_else(|| self.parser.definitions.get(index))
                    {
                        stack.push(*index);
                        self.render(default, stack, depth + 1)?;
                        stack.pop();
                    }
                    let end = self.expansion.text.chars().count();
                    self.expansion.stops.push(Tabstop {
                        index: *index,
                        range: start..end,
                        choices: self.parser.choices.get(index).cloned().unwrap_or_default(),
                        transform: None,
                    });
                }
                Node::Variable(name, default) => {
                    if let Some(value) = self.variables.get(name).filter(|value| !value.is_empty())
                    {
                        self.expansion
                            .text
                            .push_str(&value.replace("\r\n", "\n").replace('\r', "\n"));
                    } else if let Some(default) = default {
                        self.render(default, stack, depth + 1)?;
                    } else if !self.variables.contains_key(name) {
                        let index = if let Some(index) = self.unknown.get(name) {
                            *index
                        } else {
                            self.next = self
                                .next
                                .checked_add(1)
                                .ok_or("snippet variable index overflow")?;
                            self.unknown.insert(name.clone(), self.next);
                            self.next
                        };
                        let start = self.expansion.text.chars().count();
                        self.expansion.text.push_str(name);
                        self.expansion.stops.push(Tabstop {
                            index,
                            range: start..start + name.chars().count(),
                            choices: vec![],
                            transform: None,
                        });
                    }
                }
                Node::VariableTransform(name, transform) => {
                    let value = self.variables.get(name).map_or("", String::as_str);
                    self.expansion.text.push_str(
                        &transform
                            .apply(value)?
                            .replace("\r\n", "\n")
                            .replace('\r', "\n"),
                    );
                }
                Node::Transform(index, transform) => {
                    if stack.contains(index) {
                        return Err("recursive snippet transform".into());
                    }
                    let mut value = Renderer {
                        parser: self.parser,
                        variables: self.variables,
                        unknown: BTreeMap::new(),
                        next: self.next,
                        expansion: Expansion {
                            text: String::new(),
                            stops: Vec::new(),
                        },
                    };
                    if let Some(default) = self.parser.definitions.get(index) {
                        stack.push(*index);
                        value.render(default, stack, depth + 1)?;
                        stack.pop();
                    }
                    let start = self.expansion.text.chars().count();
                    self.expansion
                        .text
                        .push_str(&transform.apply(&value.expansion.text)?);
                    let end = self.expansion.text.chars().count();
                    self.expansion.stops.push(Tabstop {
                        index: *index,
                        range: start..end,
                        choices: vec![],
                        transform: Some(transform.clone()),
                    });
                }
            }
            if self.expansion.text.len() > 64 * 1024 || self.expansion.stops.len() > 256 {
                return Err("expanded snippet exceeds limits".into());
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_mirrors_choices_unicode_and_escapes() {
        let expansion = expand(r"$1 ${1:世界} ${2|one,two|} \$value $0").unwrap();
        assert_eq!(expansion.text, "世界 世界 one $value ");
        assert_eq!(expansion.stops[0].range, 0..2);
        assert_eq!(expansion.stops[1].range, 3..5);
        assert_eq!(expansion.stops[2].choices, vec!["one", "two"]);
        assert_eq!(expand("${1:call(${2:arg})}$0").unwrap().text, "call(arg)");
    }
    #[test]
    fn variables_defaults_unknowns_and_transforms_share_the_expansion_contract() {
        let variables = Variables::from([
            ("TM_FILENAME".into(), "some-file.rs".into()),
            ("EMPTY".into(), String::new()),
        ]);
        let result = expand_with_variables(r"${TM_FILENAME/(.*)\.rs/${1:/pascalcase}/} ${EMPTY:fallback} $UNKNOWN $UNKNOWN ${1:hello} ${1/(.*)/${1:/upcase}/}$0", &variables).unwrap();
        assert_eq!(result.text, "SomeFile fallback UNKNOWN UNKNOWN hello HELLO");
        assert_eq!(
            result.stops.iter().filter(|stop| stop.index == 2).count(),
            2
        );
        assert!(result.stops.iter().any(|stop| stop.transform.is_some()));
    }
    #[test]
    fn malformed_recursive_unsupported_and_oversized_snippets_are_rejected() {
        for source in [
            "${1:missing",
            "${1:$1}",
            "${1|a,b}",
            "${1/a/b/x}",
            "${1:foo} ${1/(?=foo)/bar/}",
        ] {
            assert!(expand(source).is_err(), "{source}");
        }
        assert!(expand(&"$1 ".repeat(257)).is_err());
    }
}
