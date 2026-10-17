//! Expanded snippets use Unicode scalar offsets relative to the primary insertion.
use crate::WorkspaceEdit;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tabstop {
    /// Positive indices navigate in ascending order; zero is the final stop.
    pub index: u32,
    pub range: Range<usize>,
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
}
/// Parse numbered stops, defaults, nested placeholders, first-choice defaults,
/// mirrors, and escapes. Variables and transforms are declined explicitly.
pub fn expand(source: &str) -> Result<Expansion, String> {
    if source.len() > 64 * 1024 {
        return Err("snippet exceeds 64 KiB".into());
    }
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let characters: Vec<_> = normalized.chars().collect();
    let mut cursor = 0;
    let mut definitions = BTreeMap::new();
    let nodes = parse(&characters, &mut cursor, false, 0, &mut definitions)?;
    let mut expansion = Expansion {
        text: String::new(),
        stops: Vec::new(),
    };
    render(&nodes, &definitions, &mut Vec::new(), &mut expansion)?;
    if !expansion.stops.iter().any(|stop| stop.index == 0) {
        let end = expansion.text.chars().count();
        expansion.stops.push(Tabstop {
            index: 0,
            range: end..end,
        });
    }
    validate_stops(&expansion.stops, expansion.text.chars().count())?;
    Ok(expansion)
}
fn parse(
    chars: &[char],
    cursor: &mut usize,
    nested: bool,
    depth: usize,
    definitions: &mut BTreeMap<u32, Vec<Node>>,
) -> Result<Vec<Node>, String> {
    if depth > 16 {
        return Err("snippet nesting exceeds 16 levels".into());
    }
    let mut nodes = Vec::new();
    let mut text = String::new();
    while let Some(&character) = chars.get(*cursor) {
        *cursor += 1;
        match character {
            '}' if nested => {
                if !text.is_empty() {
                    nodes.push(Node::Text(text));
                }
                return Ok(nodes);
            }
            '\\' => {
                let escaped = *chars.get(*cursor).ok_or("unfinished snippet escape")?;
                if matches!(escaped, '$' | '}' | '\\') {
                    *cursor += 1;
                    text.push(escaped);
                } else {
                    text.push('\\');
                }
            }
            '$' => {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                let braced = chars.get(*cursor) == Some(&'{');
                if braced {
                    *cursor += 1;
                }
                let start = *cursor;
                while chars.get(*cursor).is_some_and(char::is_ascii_digit) {
                    *cursor += 1;
                }
                if start == *cursor {
                    return Err("snippet variables and transforms are not supported".into());
                }
                let index = chars[start..*cursor]
                    .iter()
                    .collect::<String>()
                    .parse::<u32>()
                    .map_err(|e| e.to_string())?;
                let default = if braced {
                    match chars.get(*cursor) {
                        Some('}') => {
                            *cursor += 1;
                            None
                        }
                        Some(':') => {
                            *cursor += 1;
                            Some(parse(chars, cursor, true, depth + 1, definitions)?)
                        }
                        Some('|') => {
                            *cursor += 1;
                            let mut first = String::new();
                            let mut chosen = false;
                            loop {
                                let next =
                                    *chars.get(*cursor).ok_or("unfinished snippet choice")?;
                                *cursor += 1;
                                match next {
                                    '|' if chars.get(*cursor) == Some(&'}') => {
                                        *cursor += 1;
                                        break;
                                    }
                                    ',' => chosen = true,
                                    '\\' => {
                                        let escaped = *chars
                                            .get(*cursor)
                                            .ok_or("unfinished choice escape")?;
                                        *cursor += 1;
                                        if !chosen {
                                            first.push(escaped);
                                        }
                                    }
                                    character if !chosen => first.push(character),
                                    _ => {}
                                }
                            }
                            Some(vec![Node::Text(first)])
                        }
                        _ => return Err("unsupported or malformed snippet placeholder".into()),
                    }
                } else {
                    None
                };
                if let Some(default) = &default {
                    definitions.entry(index).or_insert_with(|| default.clone());
                }
                nodes.push(Node::Stop(index, default));
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
fn render(
    nodes: &[Node],
    definitions: &BTreeMap<u32, Vec<Node>>,
    stack: &mut Vec<u32>,
    expansion: &mut Expansion,
) -> Result<(), String> {
    for node in nodes {
        match node {
            Node::Text(text) => expansion.text.push_str(text),
            Node::Stop(index, default) => {
                if stack.contains(index) {
                    return Err("recursive snippet placeholder".into());
                }
                let start = expansion.text.chars().count();
                if let Some(default) = default.as_ref().or_else(|| definitions.get(index)) {
                    stack.push(*index);
                    render(default, definitions, stack, expansion)?;
                    stack.pop();
                }
                let end = expansion.text.chars().count();
                expansion.stops.push(Tabstop {
                    index: *index,
                    range: start..end,
                });
            }
        }
        if expansion.text.len() > 64 * 1024 || expansion.stops.len() > 256 {
            return Err("expanded snippet exceeds limits".into());
        }
    }
    Ok(())
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
        assert_eq!(expand("${1:call(${2:arg})}$0").unwrap().text, "call(arg)");
    }
    #[test]
    fn malformed_unsupported_recursive_and_oversized_snippets_are_rejected() {
        for source in ["${1:missing", "$NAME", "${1/a/b/}", "${1:$1}", "${1|a,b}"] {
            assert!(expand(source).is_err(), "{source}");
        }
        assert!(expand(&"$1".repeat(257)).is_err());
    }
}
