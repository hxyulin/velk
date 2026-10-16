//! Declarative, host-rendered settings and owned configuration snapshots.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub type Values = BTreeMap<String, Value>;
pub type PluginValues = BTreeMap<String, Values>;
pub const MAX_SETTINGS_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    Boolean,
    Integer { min: i64, max: i64 },
    Text { max_length: usize },
    Choice { options: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub key: String,
    pub title: String,
    pub description: String,
    pub kind: Kind,
    pub default: Value,
}
impl Definition {
    pub fn validate_value(&self, value: &Value) -> Result<(), String> {
        let valid = match &self.kind {
            Kind::Boolean => value.is_boolean(),
            Kind::Integer { min, max } => value.as_i64().is_some_and(|n| n >= *min && n <= *max),
            Kind::Text { max_length } => value
                .as_str()
                .is_some_and(|s| s.len() <= *max_length && !s.chars().any(char::is_control)),
            Kind::Choice { options } => value
                .as_str()
                .is_some_and(|s| options.iter().any(|o| o == s)),
        };
        if valid {
            Ok(())
        } else {
            let expected = match &self.kind {
                Kind::Boolean => "a boolean".into(),
                Kind::Integer { min, max } => format!("an integer from {min} through {max}"),
                Kind::Text { max_length } => {
                    format!("single-line text of at most {max_length} UTF-8 bytes")
                }
                Kind::Choice { options } => format!("one of: {}", options.join(", ")),
            };
            Err(format!("{} must be {expected}", self.key))
        }
    }
}

pub fn validate_schema(schema: &[Definition]) -> Result<(), String> {
    if schema.len() > 64 {
        return Err("plugin declares more than 64 settings".into());
    }
    let mut keys = BTreeSet::new();
    for setting in schema {
        if setting.key.is_empty()
            || setting.key.len() > 128
            || !setting
                .key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || !keys.insert(&setting.key)
            || setting.title.trim().is_empty()
            || setting.title.len() > 256
            || setting.title.chars().any(char::is_control)
            || setting.description.len() > 2048
            || setting.description.chars().any(char::is_control)
        {
            return Err("invalid or duplicate plugin setting".into());
        }
        match &setting.kind {
            Kind::Integer { min, max } if min > max => return Err("invalid setting bounds".into()),
            Kind::Text { max_length } if *max_length > 16 * 1024 => {
                return Err("setting text limit exceeds 16 KiB".into());
            }
            Kind::Choice { options } => {
                let unique: BTreeSet<_> = options.iter().collect();
                if options.is_empty()
                    || options.len() > 64
                    || unique.len() != options.len()
                    || options
                        .iter()
                        .any(|o| o.is_empty() || o.len() > 256 || o.chars().any(char::is_control))
                {
                    return Err("invalid setting choices".into());
                }
            }
            _ => {}
        }
        setting.validate_value(&setting.default)?;
    }
    resolve(schema, &Values::new()).map(|_| ())
}

/// Unknown keys remain in host storage but are not delivered to a plugin.
pub fn resolve(schema: &[Definition], overrides: &Values) -> Result<Values, String> {
    let mut values = Values::new();
    for setting in schema {
        let value = overrides.get(&setting.key).unwrap_or(&setting.default);
        setting.validate_value(value)?;
        values.insert(setting.key.clone(), value.clone());
    }
    if serde_json::to_vec(&values)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_SETTINGS_BYTES
    {
        return Err("plugin configuration exceeds 256 KiB".into());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema() -> Vec<Definition> {
        vec![Definition {
            key: "limit".into(),
            title: "Limit".into(),
            description: "Maximum results.".into(),
            kind: Kind::Integer { min: 1, max: 100 },
            default: 10.into(),
        }]
    }
    #[test]
    fn defaults_overrides_unknown_keys_and_invalid_values() {
        let schema = schema();
        validate_schema(&schema).unwrap();
        assert_eq!(resolve(&schema, &Values::new()).unwrap()["limit"], 10);
        let mut overrides =
            Values::from([("limit".into(), 50.into()), ("future".into(), true.into())]);
        assert_eq!(
            resolve(&schema, &overrides).unwrap(),
            Values::from([("limit".into(), 50.into())])
        );
        for bad in [
            0.into(),
            101.into(),
            true.into(),
            "10".into(),
            serde_json::json!(1.5),
        ] {
            overrides.insert("limit".into(), bad);
            assert!(resolve(&schema, &overrides).is_err());
        }
    }
    #[test]
    fn rejects_bad_schema_and_defaults() {
        let mut schema = schema();
        schema.push(schema[0].clone());
        assert!(validate_schema(&schema).is_err());
        schema.pop();
        schema[0].default = 101.into();
        assert!(validate_schema(&schema).is_err());
        schema[0].default = 10.into();
        schema[0].key = "other.plugin.limit".into();
        assert!(validate_schema(&schema).is_err());
    }
    #[test]
    fn rejects_default_snapshots_larger_than_the_wire_limit() {
        let schema: Vec<_> = (0..17)
            .map(|i| Definition {
                key: format!("text_{i}"),
                title: "Text".into(),
                description: String::new(),
                kind: Kind::Text {
                    max_length: 16 * 1024,
                },
                default: "x".repeat(16 * 1024).into(),
            })
            .collect();
        assert!(validate_schema(&schema).unwrap_err().contains("256 KiB"));
    }
    #[test]
    fn text_and_choices_are_bounded() {
        let mut s = schema().remove(0);
        s.kind = Kind::Text { max_length: 3 };
        assert!(s.validate_value(&"abc".into()).is_ok());
        assert!(s.validate_value(&"abcd".into()).is_err());
        assert!(s.validate_value(&"a\nb".into()).is_err());
        s.kind = Kind::Choice {
            options: vec!["one".into(), "two".into()],
        };
        assert!(s.validate_value(&"one".into()).is_ok());
        assert!(s.validate_value(&"three".into()).is_err());
    }
}
