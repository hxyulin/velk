//! Declarative user bindings. Routing, focus and command execution belong to the host.
use serde::{Deserialize, Serialize};

pub const MAX_BYTES: usize = 256 * 1024;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Editor,
    Window,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct When {
    pub scope: Scope,
    pub language: Option<String>,
    pub mode: Option<String>,
}
impl When {
    /// Whether all scope, language, and mode predicates apply to this context.
    pub fn matches(&self, context: &Context<'_>) -> bool {
        self.scope == context.scope
            && matches_context(&self.language, context.language)
            && matches_context(&self.mode, context.mode)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub keys: String,
    /// Null consumes the binding without invoking a command.
    pub command: Option<String>,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub repeat: bool,
    #[serde(default)]
    pub when: When,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub bindings: Vec<Binding>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stroke {
    pub key: String,
    pub control: bool,
    pub meta: bool,
    pub alt: bool,
    pub shift: bool,
}
impl Stroke {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut parts: Vec<_> = text.split('+').collect();
        let key = normalize_key(parts.pop().unwrap_or_default())?;
        let mut stroke = Self {
            key,
            control: false,
            meta: false,
            alt: false,
            shift: false,
        };
        for modifier in parts {
            let field = match modifier.to_ascii_lowercase().as_str() {
                "primary" if cfg!(target_os = "macos") => &mut stroke.meta,
                "primary" | "ctrl" | "control" => &mut stroke.control,
                "cmd" | "super" | "meta" => &mut stroke.meta,
                "alt" | "option" => &mut stroke.alt,
                "shift" => &mut stroke.shift,
                _ => return Err(format!("Unknown modifier {modifier:?}")),
            };
            if *field {
                return Err(format!("Duplicate modifier in {text:?}"));
            }
            *field = true;
        }
        Ok(stroke)
    }
}
pub fn normalize_key(key: &str) -> Result<String, String> {
    let lower = key.to_lowercase();
    let named = match lower.as_str() {
        "space" | " " => "space",
        "enter" | "return" => "enter",
        "escape" | "esc" => "escape",
        "tab" => "tab",
        "backspace" => "backspace",
        "delete" => "delete",
        "left" | "arrowleft" => "left",
        "right" | "arrowright" => "right",
        "up" | "arrowup" => "up",
        "down" | "arrowdown" => "down",
        "home" => "home",
        "end" => "end",
        "pageup" => "pageup",
        "pagedown" => "pagedown",
        "plus" => "+",
        _ => "",
    };
    if !named.is_empty() {
        return Ok(named.into());
    }
    if lower
        .strip_prefix('f')
        .and_then(|s| s.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
    {
        return Ok(lower);
    }
    if key.chars().count() == 1 && !key.chars().any(char::is_control) {
        return Ok(lower);
    }
    Err(format!("Unknown key {key:?}"))
}
#[derive(Clone, Debug)]
pub struct Compiled {
    pub binding: Binding,
    pub strokes: Vec<Stroke>,
}
#[derive(Clone, Debug, Default)]
pub struct Bindings(pub Vec<Compiled>);
pub struct Context<'a> {
    pub scope: Scope,
    pub language: Option<&'a str>,
    pub mode: Option<&'a str>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    None,
    Prefix,
    Complete(Binding),
}
impl Bindings {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("Keybinding file exceeds 256 KiB".into());
        }
        let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if file.bindings.len() > 512 {
            return Err("Keybinding file exceeds 512 bindings".into());
        }
        let mut bindings: Vec<Compiled> = Vec::new();
        for (index, binding) in file.bindings.into_iter().enumerate() {
            let validate = || -> Result<Vec<Stroke>, String> {
                if binding.command.as_ref().is_some_and(|s| {
                    s.is_empty() || s.len() > 256 || s.chars().any(char::is_control)
                }) {
                    return Err("Invalid command ID".into());
                }
                if binding.input.as_ref().is_some_and(|s| s.len() > 4096)
                    || binding.command.is_none() && binding.input.is_some()
                {
                    return Err("Invalid command input".into());
                }
                for value in [&binding.when.language, &binding.when.mode]
                    .into_iter()
                    .flatten()
                {
                    if value.is_empty() || value.len() > 64 || value.chars().any(char::is_control) {
                        return Err("Invalid binding context".into());
                    }
                }
                let strokes: Vec<_> = binding
                    .keys
                    .split_whitespace()
                    .map(Stroke::parse)
                    .collect::<Result<_, _>>()?;
                if strokes.is_empty() || strokes.len() > 4 {
                    return Err("Use between one and four key strokes".into());
                }
                Ok(strokes)
            };
            let strokes = validate().map_err(|e| format!("Binding {}: {e}", index + 1))?;
            for previous in &bindings {
                let overlaps = previous.binding.when.scope == binding.when.scope
                    && overlap(&previous.binding.when.language, &binding.when.language)
                    && overlap(&previous.binding.when.mode, &binding.when.mode);
                if overlaps
                    && previous.strokes.len() != strokes.len()
                    && (previous.strokes.starts_with(&strokes)
                        || strokes.starts_with(&previous.strokes))
                {
                    return Err(format!(
                        "Binding {} conflicts with a shorter or longer chord {:?}",
                        index + 1,
                        previous.binding.keys
                    ));
                }
            }
            bindings.push(Compiled { binding, strokes });
        }
        Ok(Self(bindings))
    }
    pub fn resolve(&self, strokes: &[Stroke], context: &Context<'_>) -> Match {
        for item in self.0.iter().rev() {
            if !item.binding.when.matches(context) {
                continue;
            }
            if item.strokes.starts_with(strokes) {
                return if item.strokes.len() == strokes.len() {
                    Match::Complete(item.binding.clone())
                } else {
                    Match::Prefix
                };
            }
        }
        Match::None
    }
}
fn overlap(a: &Option<String>, b: &Option<String>) -> bool {
    a.is_none()
        || b.is_none()
        || a.as_ref()
            .zip(b.as_ref())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
}
fn matches_context(expected: &Option<String>, actual: Option<&str>) -> bool {
    expected
        .as_ref()
        .is_none_or(|s| actual.is_some_and(|a| s.eq_ignore_ascii_case(a)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(value: serde_json::Value) -> Result<Bindings, String> {
        Bindings::parse(&serde_json::to_vec(&value).unwrap())
    }
    fn context() -> Context<'static> {
        Context {
            scope: Scope::Editor,
            language: Some("rust"),
            mode: Some("normal"),
        }
    }
    #[test]
    fn contexts_chords_and_last_matching_override_are_deterministic() {
        let bindings = parse(serde_json::json!({"bindings":[
            {"keys":"Primary+K Primary+S","command":"velk.file.save-all"},
            {"keys":"F8","command":"velk.problems.next"},
            {"keys":"F8","command":null,"when":{"mode":"NORMAL","language":"rust"}}
        ]}))
        .unwrap();
        let first = Stroke::parse("Primary+k").unwrap();
        assert_eq!(
            bindings.resolve(std::slice::from_ref(&first), &context()),
            Match::Prefix
        );
        let Match::Complete(binding) =
            bindings.resolve(&[first, Stroke::parse("Primary+s").unwrap()], &context())
        else {
            panic!()
        };
        assert_eq!(binding.command.as_deref(), Some("velk.file.save-all"));
        let Match::Complete(binding) =
            bindings.resolve(&[Stroke::parse("F8").unwrap()], &context())
        else {
            panic!()
        };
        assert!(binding.command.is_none());
        let Match::Complete(binding) = bindings.resolve(
            &[Stroke::parse("F8").unwrap()],
            &Context {
                mode: Some("insert"),
                ..context()
            },
        ) else {
            panic!()
        };
        assert_eq!(binding.command.as_deref(), Some("velk.problems.next"));
        assert_eq!(
            bindings.resolve(
                &[Stroke::parse("F8").unwrap()],
                &Context {
                    scope: Scope::Window,
                    ..context()
                }
            ),
            Match::None
        );
    }
    #[test]
    fn invalid_keys_unknown_fields_oversized_input_and_ambiguous_chords_are_rejected() {
        for binding in [
            serde_json::json!({"keys":"Mystery+x","command":"test.command"}),
            serde_json::json!({"keys":"Ctrl+Control+x","command":"test.command"}),
            serde_json::json!({"keys":"F25","command":"test.command"}),
            serde_json::json!({"keys":"","command":"test.command"}),
            serde_json::json!({"keys":"a b c d e","command":"test.command"}),
            serde_json::json!({"keys":"x","command":"test.command","script":"anything"}),
            serde_json::json!({"keys":"x","command":null,"input":"unused"}),
        ] {
            assert!(parse(serde_json::json!({"bindings":[binding]})).is_err());
        }
        assert!(
            parse(serde_json::json!({"bindings":[
                {"keys":"Ctrl+k","command":"short"}, {"keys":"Ctrl+k s","command":"long"}
            ]}))
            .is_err()
        );
        assert!(
            parse(serde_json::json!({"bindings":[
                {"keys":"x","command":"short","when":{"mode":"insert"}},
                {"keys":"x x","command":"long","when":{"mode":"normal"}}
            ]}))
            .is_ok()
        );
        assert!(Bindings::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
        assert_eq!(Stroke::parse("Shift+Plus").unwrap().key, "+");
    }
}
