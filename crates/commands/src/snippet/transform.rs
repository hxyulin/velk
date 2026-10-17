//! Bounded Rust-regex transforms; no scripting or backtracking engine.
use regex::{Captures, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    pub pattern: String,
    pub format: String,
    pub options: String,
}
impl Transform {
    fn regex(&self) -> Result<Regex, String> {
        if self.pattern.len() > 4096
            || self.format.len() > 4096
            || self.options.len() > 4
            || self.options.chars().any(|c| !"gims".contains(c))
        {
            return Err("invalid snippet transform or unsupported flags".into());
        }
        RegexBuilder::new(&self.pattern)
            .case_insensitive(self.options.contains('i'))
            .multi_line(self.options.contains('m'))
            .dot_matches_new_line(self.options.contains('s'))
            .size_limit(256 * 1024)
            .dfa_size_limit(1024 * 1024)
            .nest_limit(64)
            .build()
            .map_err(|e| format!("unsupported snippet regex: {e}"))
    }
    pub fn validate(&self) -> Result<(), String> {
        self.regex()?;
        // Validate even branches that an actual capture might never select.
        validate_format(&self.format, 0)
    }
    pub fn apply(&self, value: &str) -> Result<String, String> {
        if value.len() > 64 * 1024 {
            return Err("snippet transform input too large".into());
        }
        let regex = self.regex()?;
        validate_format(&self.format, 0)?;
        let mut output = String::new();
        let mut end = 0;
        for (index, captures) in regex.captures_iter(value).enumerate() {
            if index >= 4096 {
                return Err("snippet transform exceeds 4096 matches".into());
            }
            let whole = captures.get(0).ok_or("missing regex match")?;
            output.push_str(&value[end..whole.start()]);
            output.push_str(&format(&self.format, Some(&captures), 0)?);
            if output.len() > 64 * 1024 {
                return Err("snippet transform output too large".into());
            }
            end = whole.end();
            if !self.options.contains('g') {
                break;
            }
        }
        output.push_str(&value[end..]);
        if output.len() > 64 * 1024 {
            return Err("snippet transform output too large".into());
        }
        Ok(output)
    }
}
fn validate_format(source: &str, depth: usize) -> Result<(), String> {
    format(source, None, depth).map(|_| ())
}
fn format(source: &str, captures: Option<&Captures<'_>>, depth: usize) -> Result<String, String> {
    if depth > 16 {
        return Err("snippet transform format nesting too deep".into());
    }
    let chars: Vec<_> = source.chars().collect();
    let mut cursor = 0;
    let mut output = String::new();
    while let Some(&c) = chars.get(cursor) {
        cursor += 1;
        if c == '\\' {
            if let Some(&next) = chars.get(cursor) {
                if !matches!(next, '\\' | '/' | '$' | '}' | ':') {
                    output.push('\\');
                }
                output.push(next);
                cursor += 1;
            } else {
                return Err("unfinished transform escape".into());
            }
            continue;
        }
        if c != '$' {
            output.push(c);
            continue;
        }
        let braced = chars.get(cursor) == Some(&'{');
        if braced {
            cursor += 1;
        }
        let start = cursor;
        while chars.get(cursor).is_some_and(char::is_ascii_digit) {
            cursor += 1;
        }
        if start == cursor {
            if braced {
                return Err("invalid transform capture".into());
            }
            output.push('$');
            continue;
        }
        let index: usize = chars[start..cursor]
            .iter()
            .collect::<String>()
            .parse::<usize>()
            .map_err(|e| e.to_string())?;
        let capture = captures
            .and_then(|c| c.get(index))
            .map_or("", |m| m.as_str());
        if !braced {
            if output.len() + capture.len() > 64 * 1024 {
                return Err("snippet transform format output too large".into());
            }
            output.push_str(capture);
            continue;
        }
        let start = cursor;
        let mut nesting = 0;
        while let Some(&next) = chars.get(cursor) {
            if next == '\\' {
                cursor += 2;
                continue;
            }
            if next == '$' && chars.get(cursor + 1) == Some(&'{') {
                nesting += 1;
                cursor += 2;
                continue;
            }
            if next == '}' {
                if nesting == 0 {
                    break;
                }
                nesting -= 1;
            }
            cursor += 1;
        }
        if chars.get(cursor) != Some(&'}') {
            return Err("unfinished transform capture".into());
        }
        let suffix: String = chars[start..cursor].iter().collect();
        cursor += 1;
        let present = !capture.is_empty();
        let replacement = if suffix.is_empty() {
            capture.into()
        } else if let Some(case) = suffix.strip_prefix(":/") {
            convert_case(capture, case)?
        } else if let Some(branch) = suffix.strip_prefix(":+") {
            if captures.is_none() {
                validate_format(branch, depth + 1)?;
                String::new()
            } else if present {
                format(branch, captures, depth + 1)?
            } else {
                String::new()
            }
        } else if let Some(branches) = suffix.strip_prefix(":?") {
            let (yes, no) = split_branches(branches)?;
            if captures.is_none() {
                validate_format(yes, depth + 1)?;
                validate_format(no, depth + 1)?;
                String::new()
            } else {
                format(if present { yes } else { no }, captures, depth + 1)?
            }
        } else if let Some(branch) = suffix
            .strip_prefix(":-")
            .or_else(|| suffix.strip_prefix(':'))
        {
            if captures.is_none() {
                validate_format(branch, depth + 1)?;
                String::new()
            } else if present {
                capture.into()
            } else {
                format(branch, captures, depth + 1)?
            }
        } else {
            return Err("unsupported transform format".into());
        };
        output.push_str(&replacement);
        if output.len() > 64 * 1024 {
            return Err("snippet transform format output too large".into());
        }
    }
    Ok(output)
}
fn split_branches(source: &str) -> Result<(&str, &str), String> {
    let mut depth = 0;
    let mut escaped = false;
    let mut previous = None;
    for (offset, c) in source.char_indices() {
        if escaped {
            escaped = false;
            previous = None;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '{' && previous == Some('$') {
            depth += 1;
        }
        if c == '}' {
            depth -= 1;
        }
        if c == ':' && depth == 0 {
            return Ok((&source[..offset], &source[offset + 1..]));
        }
        previous = Some(c);
    }
    Err("conditional transform needs an else branch".into())
}
fn convert_case(value: &str, case: &str) -> Result<String, String> {
    let capitalize = |text: &str| {
        let mut chars = text.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
            .unwrap_or_default()
    };
    Ok(match case {
        "upcase" => value.to_uppercase(),
        "downcase" => value.to_lowercase(),
        "capitalize" => capitalize(value),
        "camelcase" | "pascalcase" => {
            let mut words = Vec::new();
            let mut word = String::new();
            for c in value.chars() {
                if !c.is_alphanumeric() {
                    if !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                } else {
                    if c.is_uppercase() && word.chars().last().is_some_and(char::is_lowercase) {
                        words.push(std::mem::take(&mut word));
                    }
                    word.push(c);
                }
            }
            if !word.is_empty() {
                words.push(word);
            }
            words
                .into_iter()
                .enumerate()
                .map(|(i, word)| {
                    if i == 0 && case == "camelcase" {
                        word.to_lowercase()
                    } else {
                        capitalize(&word.to_lowercase())
                    }
                })
                .collect()
        }
        _ => return Err("unsupported snippet case conversion".into()),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captures_cases_conditionals_flags_and_limits() {
        let transform = |pattern: &str, format: &str, options: &str| Transform {
            pattern: pattern.into(),
            format: format.into(),
            options: options.into(),
        };
        assert_eq!(
            transform("(.*)", "${1:/pascalcase}", "")
                .apply("hello_world")
                .unwrap(),
            "HelloWorld"
        );
        assert_eq!(
            transform("(foo)?(bar)", "${1:?yes:no}-$2", "g")
                .apply("foobar bar")
                .unwrap(),
            "yes-bar no-bar"
        );
        assert_eq!(
            transform("(foo)", "${1:/upcase}", "gi")
                .apply("foo FOO")
                .unwrap(),
            "FOO FOO"
        );
        assert_eq!(
            transform("(.*)", r"\w-$1", "").apply("value").unwrap(),
            r"\w-value"
        );
        assert!(transform("(?=foo)", "$0", "").validate().is_err());
        assert!(transform("(.*)", "${1:/unknown}", "").validate().is_err());
        assert!(
            transform(".", &"x".repeat(4096), "g")
                .apply(&"a".repeat(100))
                .is_err()
        );
    }
}
